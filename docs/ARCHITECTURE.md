# Architektur

```
┌─────────────────────────────── UI-Thread (Slint-Event-Loop) ───────────────────────────────┐
│                                                                                            │
│  ui/*.slint ──Bridge/Dialogs-Globals──▶ app::AppState (Controller)                         │
│                                          ├─ core::ConnectionManager ─▶ database (SQLite)   │
│                                          │                           └▶ security::credentials (WinCred)
│                                          ├─ core::SessionManager (Tabs, Split-Bereiche)    │
│                                          │     └─ Rc<dyn rdp::RdpSession> je Tab           │
│                                          └─ app::keyboard (WH_KEYBOARD_LL → Scancodes)     │
│                ▲                                                                           │
│                │ core::EventBus (gesammelt, invoke_from_event_loop)                         │
└────────────────┼───────────────────────────────────────────────────────────────────────────┘
                 │
┌────────────────┴──────────── Sitzungs-Thread (1 je Verbindung, eigene Tokio-Runtime) ──────┐
│  rdp::session::run_worker                                                                  │
│    ├─ ironrdp-client::RdpClient::run()  (TCP/TLS/CredSSP, MCS, Fast-Path, Kanäle)          │
│    ├─ RdpOutputEvent → rdp::renderer::Framebuffer (Dirty-Regionen, 0x00RRGGBB)             │
│    └─ rdp::renderer::Presenter (≤ 60 Hz, nur sichtbar, Double-Buffer → RGBA)               │
│  security::certificates::CertificateVerifier (TOFU; blockiert nur diesen Thread)           │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

## Schichten

| Modul | Aufgabe |
|---|---|
| `database` | Modelle, Migrationen (`PRAGMA user_version`), Repository. Keine Passwortspalten. |
| `security::credentials` | `CredentialStore`-Trait; Windows: `CredReadW/CredWriteW/CredDeleteW` (Generic Credentials, `Remora RDP/connection/<id>/password` – auch für SSH-Passwörter und Schlüssel-Passphrasen; der Präfix stammt aus der Zeit vor der Umbenennung). `Secret` nullt Speicher beim Drop und erscheint nie im Debug-Output. |
| `security::certificates` | SHA-256-Fingerabdruck, TOFU-Speicher (`known_hosts`), Entscheidungsdialog über die UI. |
| `security::vault` | Argon2id + XChaCha20-Poly1305 für verschlüsselte Exporte. |
| `services` | `.rdp`-Import/Export (UTF-16LE), JSON-Export/Import. |
| `rdp` | `RdpBackend`/`RdpSession`-Traits (austauschbar, z. B. für ein FreeRDP-Backend), IronRDP-Implementierung, Renderer, Eingabe-Mapping. |
| `ssh` | SSH-Backend mit derselben `RdpSession`-Schnittstelle: `russh` (Handshake, Hostschlüssel über `CertificateVerifier`, Anmeldung, PTY-Kanal), `alacritty_terminal` (Emulation, Verlauf, Auswahl), eigene Rasterung mit `fontdue` aus Windows-Schriften in einen `rdp::renderer::Framebuffer`. `core::backends::ProtocolBackends` wählt je Verbindung RDP oder SSH. |
| `core` | Verbindungs- und Sitzungsverwaltung, Ereignisbus. |
| `app` | Slint-Anbindung, Dialogsteuerung, Tastatur-Hook, Plattform-Helfer. |

## Grafikpipeline

1. `ironrdp-client` dekodiert Bitmap-Updates (RemoteFX, Planar, Interleaved, …; EGFX mit H.264 über Media Foundation) in einen eigenen Framebuffer und liefert mit `with_desktop_updates()` nur die **geänderten Rechtecke** (`RdpOutputEvent::DesktopUpdate`).
2. Der Sitzungs-Thread kopiert diese in seinen `Framebuffer` und vereinigt die Dirty-Rects.
3. Bei jedem Tick (16 ms) erzeugt der `Presenter` – nur wenn die Sitzung sichtbar ist und die UI den vorherigen Frame bestätigt hat – einen `SharedPixelBuffer<Rgba8Pixel>`. Dabei werden nur die Dirty-Bereiche konvertiert; zwei Puffer wechseln sich ab, sodass keine Allokation pro Frame nötig ist.
4. Der UI-Thread erzeugt daraus ein `slint::Image` und setzt es im Bereichsmodell. Slint lädt das Bild als Textur hoch und skaliert es GPU-seitig (Einpassen/1:1/Strecken).
5. Unsichtbare Tabs erhalten keine Frames; beim Wieder-Einblenden wird ein Vollbild angefordert.

Der Remote-Mauszeiger wird nicht in den Framebuffer gerendert, sondern als Overlay-Bild an der **lokalen** Mausposition gezeichnet – dadurch ist er latenzfrei.

## Eingabe

* Maus: Slint-`TouchArea` → `rdp::input::map_pointer` (gleiche Geometrieformel wie in `rdp_view.slint`) → `ironrdp-input::Database` → Fast-Path.
* Tastatur: Slint liefert nur Text, RDP braucht Scancodes. Unter Windows fängt ein Low-Level-Hook die Scancodes ab, **nur** wenn das Remora-Fenster im Vordergrund ist und eine Remote-Anzeige fokussiert ist. Lokale Kombinationen (Strg+Alt+Pos1/Pause/Bild↑/Bild↓/Ende) werden lokal behandelt. Bei Fokuswechsel werden alle Tasten der alten Sitzung losgelassen und Feststelltasten synchronisiert.

## SSH-Terminal

* Für die Oberfläche ist ein Terminal eine Sitzung mit dynamischer Auflösung: Der Sitzungs-Thread zeichnet das Zeichenraster in Bereichsgröße (physische Pixel, immer 1:1) und liefert Bilder über denselben `Presenter` wie RDP. Größenänderungen ergeben ein neues Raster und eine `window-change`-Anfrage an den Server.
* Tastatur: Für Terminal-Ziele lässt der Low-Level-Hook alle Tasten außer den lokalen Kombinationen durch. Im Fenster liefert Windows über `TranslateMessage` die Zeichen (`WM_CHAR`, `WM_SYSCHAR` für Alt) – Layout, AltGr und Tottasten sind damit aufgelöst. Tasten ohne Zeichen (Pfeile, F-Tasten, Pos1 …) werden als `InputEvent::TermKey` mit Modifiern gesendet und im Sitzungs-Thread je nach Terminalmodus (z. B. Application Cursor) in xterm-Folgen übersetzt. Strg+Umschalt+C/V kopieren bzw. fügen ein; das zugehörige `WM_CHAR` (^C/^V) wird verworfen.
* Maus: Ohne Mausmodus des Programms markiert die linke Taste (Doppel-/Dreifachklick: Wort/Zeile); Rechtsklick kopiert die Auswahl oder fügt ein. Mit Mausmodus (htop, mc) gehen Klicks, Bewegungen und Rad als SGR- bzw. X10-Berichte an den Server; Umschalt erzwingt die lokale Auswahl.
* Zwischenablage: Die Sitzung meldet `SessionEvent::Clipboard` bzw. `PasteRequest`; der UI-Thread liest und schreibt die Windows-Zwischenablage. OSC-52-Anfragen des Servers werden ignoriert.

## Lebenszyklus & Ressourcen

* Jede RDP-Sitzung = 1 OS-Thread + Tokio-Runtime mit 2 Workern, jede SSH-Sitzung = 1 OS-Thread mit einer Single-Thread-Runtime. Schließen → `request_close()`; der Thread beendet sich, die Runtime wird verworfen (Sockets, Tasks, Puffer frei). Der UI-Thread joint nie.
* Reconnect erzeugt einen neuen *Verbindungsversuch* mit neuer Versuchs-ID. Ereignisse älterer Versuche werden verworfen.
* Zwei Wiederverbindungsebenen: IronRDP-Auto-Reconnect mit Server-Cookie (Transportabbruch) und anwendungsseitiger Neuaufbau mit Backoff, falls dieser scheitert.

## Titelleiste (Windows)

`src/app/titlebar.rs` installiert eine Fenster-Subclass (`SetWindowSubclass`), sobald das native Fenster existiert (`init_native_window`, wiederholt bis winit das Handle liefert). Der normale Rahmen bleibt erhalten (Schatten, Ecken, Snap, Animationen):

* `WM_NCCALCSIZE`: Seiten und unten berechnet Windows wie üblich, die obere Kante bleibt auf dem Fensterrand – die Beschriftung wird Client-Fläche und von `ui/components/titlebar.slint` gezeichnet. Maximiert wird um die Rahmendicke eingerückt.
* `WM_NCHITTEST`: oberer Größenrand → `HTTOP`, Leiste links der Buttons → `HTCAPTION`, Buttons → `HTCLIENT` (Slint). Die reine Logik (`hit_test`) ist getestet.
* `WM_SIZE`/`WM_ACTIVATE` melden Maximiert/Aktiv an die Oberfläche (Symbol Wiederherstellen, gedimmte Leiste).
* Im Vollbild ist die Subclass passiv, damit Klicks am oberen Rand an die Remote-Sitzung gehen.

Minimieren/Maximieren laufen über `ShowWindow`, Schließen über `WM_CLOSE` – also denselben Weg wie `Alt+F4` inklusive Rückfrage bei aktiven Sitzungen.

Snap-Layouts (Windows 11): Der Maximieren-Button wird bei `WM_NCHITTEST` als `HTMAXBUTTON` gemeldet. Dadurch landen Mausereignisse dort als Nicht-Client-Nachrichten: `WM_NCMOUSEMOVE`/`WM_NCMOUSELEAVE` setzen den Hover-Zustand (`Bridge.max-button-hover`), `WM_NCLBUTTONDOWN`/`UP` werden selbst behandelt (nicht an `DefWindowProc`, die sonst einen klassischen Button zeichnen würde) und lösen Maximieren/Wiederherstellen aus.
