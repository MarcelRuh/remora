# Remora – Remote Connection Manager

Nativer Verbindungsmanager für **RDP und SSH** unter Windows 10/11 – geschrieben in **Rust**, Oberfläche mit **Slint**, RDP mit **IronRDP**, SSH mit **russh**. Kein Electron, kein Browser-Frontend, kein `mstsc.exe`, kein externes Terminal: Remote-Desktop und Terminal werden direkt in der eigenen Oberfläche dargestellt.

Bis Version 0.2.x hieß das Programm „Remora RDP Manager“.

* Verbindungsverwaltung mit Ordnern, Tags (Gruppen), Favoriten, Verlauf und Suche – RDP- und SSH-Verbindungen gemeinsam
* Mehrere parallele Sitzungen in Tabs, Split-View (nebeneinander, übereinander, 2×2), auch gemischt
* Echte RDP-Sitzungen: TLS, NLA/CredSSP, Grafik inkl. H.264, Maus, Tastatur (Scancodes), Zwischenablage, Audio, Laufwerke, Drucker, RD Gateway, dynamische Auflösung, Auto-Reconnect
* SSH-Sitzungen mit eingebautem Terminal (xterm-256color, Truecolor, Maus für htop/mc, 10 000 Zeilen Verlauf, Auswahl und Zwischenablage); Anmeldung per Passwort (auch Keyboard-Interactive), Schlüsseldatei oder SSH-Agent (Windows-OpenSSH-Agent, Pageant); Hostschlüsselprüfung wie bei RDP-Zertifikaten
* Passwörter und Schlüssel-Passphrasen ausschließlich im Windows Credential Manager, TOFU-Prüfung für Zertifikate und Hostschlüssel, verschlüsselter Export mit Master-Passwort
* Gestaltung im Stil des **ora-Stacks** ([Proxora](https://github.com/MarcelRuh/proxora), Dockora, Sambora): fast schwarze Flächen, Pink als Akzent, Orbitron-Überschriften, Neon-Sechseck-Logo
* Dark Mode (Standard), Light Mode, Systemdesign, frei wählbare Akzentfarbe
* In das Design integrierte Titelleiste mit nativem Fensterverhalten (Ziehen, Aero Snap, Snap-Layouts am Maximieren-Button, Doppelklick, Systemmenü)
* Eingebauter Updater: prüft GitHub-Releases (abschaltbar), lädt das Setup, prüft die SHA-256-Summe und installiert nach Rückfrage; portable Kopien öffnen die Release-Seite

## Installation

### Installer (empfohlen)

`Remora-RDP-Setup-<Version>-x64.exe` ausführen. Der Installer

* installiert nach `C:\Program Files\Remora` (Administratorrechte erforderlich; Updates bleiben im bisherigen Ordner, bis 0.2.x `C:\Program Files\Remora RDP`),
* legt einen Startmenü-Eintrag und optional ein Desktop-Symbol an,
* bietet Remora optional unter „Öffnen mit“ für `.rdp`-Dateien an (die Standardzuordnung zu `mstsc` bleibt unverändert; Remora lässt sich über „Öffnen mit → Immer“ selbst als Standard wählen),
* registriert sich unter „Einstellungen → Apps“ zur Deinstallation.

Die Deinstallation entfernt Programm, Verknüpfungen, Dateizuordnung und den Autostart-Eintrag. Verbindungen und Einstellungen werden nur auf Rückfrage gelöscht; gespeicherte Passwörter bleiben im Windows Credential Manager (Einträge `Remora RDP/…`).

Stille Installation: `Remora-RDP-Setup-<Version>-x64.exe /VERYSILENT /TASKS=rdpassoc`

### Ohne Installer

`target\release\remora.exe` ist eigenständig (keine Laufzeitabhängigkeiten außer Windows 10 1809+ bzw. Windows 11). Einfach kopieren und starten.

**Portabler Modus:** Liegt eine (leere) Datei `remora.portable` neben der EXE, werden alle Daten im Unterordner `data\` daneben abgelegt. Passwörter bleiben im Credential Manager des jeweiligen Benutzers.

### Datenablage

Die Pfade und die Namen im Credential Manager behalten den bisherigen Namen, damit Verbindungen und gespeicherte Passwörter nach dem Update erhalten bleiben. Aus demselben Grund heißen die Release-Dateien weiter `Remora-RDP-…`.

* `%APPDATA%\Remora\Remora RDP\data\remora.sqlite3` – Verbindungen, Ordner, vertraute Zertifikate und SSH-Hostschlüssel
* `%APPDATA%\Remora\Remora RDP\config\settings.toml` – Einstellungen
* `%LOCALAPPDATA%\Remora\Remora RDP\data\logs\` – Protokolle (ohne Zugangsdaten)

## Build

Voraussetzungen:

* Rust ≥ 1.89 (Edition 2024), Toolchain `x86_64-pc-windows-msvc`
* Visual Studio 2022 Build Tools mit „Desktopentwicklung mit C++“ (MSVC + Windows SDK; `rc.exe` bettet Symbol und Versionsinfo ein)
* Internetzugang für Cargo (IronRDP wird als Git-Abhängigkeit auf einen festen Commit gepinnt)
* CMake im `PATH` (für den Opus-Codec der Audio-Umleitung). Die VS Build Tools bringen CMake mit:
  `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`.
  Ohne Audio: `cargo build --release --no-default-features`
* Für den Installer: Inno Setup 6 (`winget install JRSoftware.InnoSetup`)

```bash
cargo build --release
```

Installer bauen (Release-Build + Setup, Ergebnis in `target\installer\`):

```bash
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

Tests, Formatierung, Lints:

```bash
cargo test
```

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy --all-targets -- -D warnings
```

Programmsymbol und Markenbilder neu erzeugen (Vorlage `assets/brand/remora-logo.webp` → Emblem `remora-mark.png`, Logo `remora-logo.png`, vorskalierte `remora-mark-32.png`/`-64.png`, `assets/remora.ico` in allen Größen, Setup-Assistentenbilder in `installer\images\`; Vorschauen in `target\icon-preview\`). Neues Logo: einfach `remora-logo.webp` ersetzen und erneut ausführen:

```bash
cargo run --example make_icon
```

Ausführliches Logging: Umgebungsvariable `REMORA_LOG=debug` (bzw. `remora=debug,ironrdp=info`).
Standard-Titelleiste von Windows statt der integrierten: `REMORA_NATIVE_TITLEBAR=1`.

## Bedienung

| Aktion | Weg |
|---|---|
| Verbindung anlegen | „Neue Verbindung“ unten in der Sidebar, `Strg+N`, oder Kontextmenü eines Ordners |
| Verbinden | Doppelklick, Kontextmenü, Dashboard-Karte, oder Eintrag in einen Split-Bereich ziehen |
| Schnellverbindung | `Strg+T`, dann z. B. `server01`, `10.0.0.5:3390`, `DOMÄNE\admin@host` (RDP) oder `ssh root@ct101`, `ssh://admin@host:2222`, `root@host:22` (SSH) |
| SSH-Verbindung anlegen | Im Editor oben „SSH – Terminal“ wählen; unter „Anmeldung“ Passwort, Schlüsseldatei oder SSH-Agent |
| Terminal: kopieren / einfügen | Text mit der Maus markieren (Doppelklick: Wort, Dreifachklick: Zeile), dann Rechtsklick oder `Strg+Umschalt+C`; Rechtsklick ohne Auswahl oder `Strg+Umschalt+V` fügt ein. In Programmen mit Mausbedienung (htop, mc) mit gedrückter `Umschalt`-Taste markieren |
| Terminal: Verlauf | Mausrad oder `Umschalt+Bild↑/↓`; Tippen springt ans Ende |
| Sortieren / Gruppieren | Sortiersymbol in der Sidebar (Ordner ↔ Name), Tags als `#tag` in der Suche oder per Chip |
| Split-View | Layout-Schalter rechts in der Tab-Leiste; Tabs in Bereiche ziehen; Trennlinien verschieben; Doppelklick auf Bereichskopf maximiert |
| Tastatur an die Sitzung | In die Remote-Anzeige klicken. Freigeben: `Strg+Alt+Pos1` |
| Vollbild | `F11` bzw. in der Sitzung `Strg+Alt+Pause` |
| Fenster | Titelleiste ziehen, an den Bildschirmrand ziehen (Snap), Doppelklick maximiert, Rechtsklick öffnet das Systemmenü |
| Import/Export | Einstellungen → Import/Export; `.rdp` auch über die Sidebar und das Kontextmenü |
| `.rdp` öffnen | `remora.exe C:\Pfad\verbindung.rdp` (bzw. „Öffnen mit“ nach Installation) importiert (ohne Duplikate) und verbindet – läuft Remora schon, übernimmt das offene Fenster die Datei |

Tastenkombinationen der Anwendung (`Strg+T/W/Tab/F`, `F11`) gelten nur, solange keine Remote-Sitzung die Tastatur besitzt – sonst gehen sie an den Server.

## Projektstruktur

```
src/
  main.rs                 Einstieg, Logging, Pfade
  lib.rs                  Modulübersicht, Slint-Einbindung
  app/                    Controller (UI ↔ Logik), Tastatur-Hook, Titelleiste,
                          Konvertierung, Plattform
  core/                   ConnectionManager, SessionManager, EventBus
  rdp/                    Traits, IronRDP-Backend (client.rs), Sitzung (session.rs),
                          Renderer, Eingabe, Zwischenablage
  ssh/                    SSH-Backend (russh), Sitzung, Terminal (alacritty_terminal),
                          Schriftrasterung (Windows-Schriften)
  database/               Modelle, Migrationen, Repository (SQLite)
  security/               Credential Manager, Zertifikate (TOFU), Vault
  settings/               Einstellungen, Autostart
  services/               .rdp-Import/Export, Konfigurations-Export
ui/
  app.slint               Hauptfenster, Split-Workspace
  bridge.slint            Datenstrukturen und Callbacks (UI ↔ Rust)
  components/ pages/ dialogs/ themes/ icons/
installer/                Inno-Setup-Skript und Build-Skript
examples/make_icon.rs     Erzeugt das Programmsymbol
tests/                    Integrationstests (SQLite-Datei, Credential Manager, Live-RDP, Live-SSH)
docs/                     ARCHITECTURE.md, LIMITATIONS.md
```

Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · Einschränkungen: [docs/LIMITATIONS.md](docs/LIMITATIONS.md)

## Verifikation

Stand der Prüfungen auf Windows 11 Pro x64 (Rust 1.99.1, MSVC 2022):

| Prüfung | Ergebnis |
|---|---|
| `cargo build` / `cargo build --release` | erfolgreich |
| `cargo fmt --all -- --check` | sauber |
| `cargo clippy --all-targets -- -D warnings` | keine Warnungen |
| `cargo test` | 100 Tests bestanden (91 Unit-Tests, 8 Datenbank-Integrationstests, 1 Credential Manager), u. a. Terminal-Darstellung, xterm-Tastenfolgen, Auswahl, Mausberichte, Schriftrasterung, Protokollwahl, Migration auf Schema 2 (Datenbank, Ordnerlogik, Suche/Baum, Sitzungs-/Split-Verwaltung, Renderer/Double-Buffer, Maus-Mapping, Tastatur-Entscheidungen, Titelleisten-Trefferlogik, `.rdp`-Import/Export, Vault, TOFU, Einstellungen, IronRDP-Konfiguration) |
| Echter Windows Credential Manager | Testeintrag schreiben/lesen/löschen (`tests/windows_credentials.rs`) |
| **Live gegen Windows-RDP-Dienst** (127.0.0.2) | `cargo test --test live_rdp -- --ignored`: TCP, X.224-Aushandlung, TLS, Zertifikatsabfrage (SHA-256, TOFU), CredSSP/NLA bis zur Serverantwort `STATUS_LOGON_FAILURE` (bewusst mit nicht existierendem Konto) |
| Oberfläche | App gestartet; Dashboard, Editor, Anmeldedialog, Fehlerüberlagerung, 2×2-Split und Einstellungen per Screenshot geprüft |
| Titelleiste | Trefferprüfung per `WM_NCHITTEST` (Rand oben, Ziehfläche, Buttons), Maximieren-Button als `HTMAXBUTTON` (Klick maximiert/stellt wieder her), Minimieren, Maximieren/Wiederherstellen, Doppelklick, Schließen, Vollbild ein/aus (keine Ziehfläche im Vollbild) – bei 100 % Skalierung |
| Design (ora-Stack) | Dashboard, Einstellungen (Allgemein, Darstellung, Über), Dunkel und Hell per Screenshot geprüft; Logo-Vorlage als Emblem in Sidebar und Fenstersymbol, vollständiges Logo unter „Über“, Programmsymbol in EXE und Setup (16–256 px aus der Vorlage); alter Standard-Akzent `#4c8dff` wird beim Laden auf `#ff006e` umgestellt (Test) |
| Einzelinstanz | Zweiter Start ohne Datei holt das Fenster nach vorn, zweiter Start mit relativer `.rdp`-Datei übergibt sie an die laufende Instanz (Import, Anmeldedialog); geprüft mit portabler Kopie |
| Update-Prüfung | Automatisch nach dem Start und per „Jetzt prüfen“ gegen die GitHub-API (ohne Release: „Remora ist aktuell“); Bewertung von Versionen, Vorabversionen und fremden URLs per Test ; Auswahl von Setup und Prüfsummenliste aus den Release-Dateien, Abweisen fremder Download-Adressen und Prüfsummen-Auswertung per Test. Download und Installation über ein echtes Release noch offen |
| **SSH gegen Test-Server** (russh-Server, 127.0.0.1) | `cargo test --test live_ssh -- --ignored`: Hostschlüssel-Rückfrage, Speichern und Wiedererkennen, Ablehnung bricht vor der Anmeldung ab, Passwort und Keyboard-Interactive, falsches Passwort, PTY `xterm-256color`, Eingaben, Größenänderung, Einfügen, `exit`. In der App: Schnellverbindung, Anmelde- und Hostschlüsseldialog, echte Tastendrücke mit deutschem Layout (AltGr+Q → `@`, `ä`, Umschalt+7, Pfeile, F5, Strg+C, Umschalt+Tab), Verlauf per Mausrad, Markieren und Kopieren per Rechtsklick, Einfügen per Strg+Umschalt+V und Rechtsklick, Größenänderung, Sitzungsende. **Nicht** gegen OpenSSH bzw. einen echten Linux-Server oder Proxmox-Container geprüft |
| Installer | Stille Installation nach `C:\Program Files\Remora RDP`, Startmenü-Verknüpfung, Uninstall-Eintrag, `.rdp`-„Öffnen mit“, Start der installierten EXE; stille Deinstallation entfernt Ordner, Verknüpfung, Dateizuordnung und Uninstall-Eintrag, Benutzerdaten bleiben erhalten |

**Noch nicht verifiziert:** eine vollständig angemeldete Sitzung (Bilddarstellung, Eingaben, Zwischenablage, Audio, dynamische Auflösung, Laufwerke/Drucker, Gateway). Eine erfolgreiche Anmeldung an `127.0.0.2` mit dem eigenen Konto würde auf Windows 10/11 Pro die laufende Sitzung übernehmen; für diese Tests wird ein separater RDP-Server (VM, Windows Server) empfohlen. Die Titelleiste ist nur bei 100 % Skalierung und auf Windows 11 geprüft (nicht: Windows 10, andere DPI-Stufen, Mehrschirm mit gemischter Skalierung).

## Drittanbieter-Schrift

Die Überschriften verwenden **Orbitron** (© The Orbitron Project Authors, SIL Open Font License 1.1). Die Schrift ist in die EXE eingebettet; die Lizenz liegt unter `assets/fonts/OFL.txt` und wird vom Installer nach `licenses\Orbitron-OFL.txt` kopiert.

## Lizenz

MIT – siehe [LICENSE](LICENSE). Orbitron: SIL Open Font License 1.1 (siehe oben).

## Releases

Versions-Tags `vX.Y.Z` (passend zu `version` in `Cargo.toml`) bauen in GitHub Actions automatisch Installer und portable ZIP und veröffentlichen sie als Release; die Release-Notizen stammen aus [CHANGELOG.md](CHANGELOG.md). Jeder Push und Pull Request durchläuft Format-, Clippy- und Testprüfung.
