# Bekannte Einschränkungen

Stand: Version 0.1.0. Die Tabelle unterscheidet zwischen **vollständig**, **teilweise** und **nicht** umgesetzten Funktionen. Der Teststatus steht in der README.

## RDP (IronRDP)

| Funktion | Status | Anmerkung |
|---|---|---|
| TLS, NLA/CredSSP (NTLM/Kerberos über `sspi`) | vollständig | Über `ironrdp-client`. |
| Zertifikatsprüfung | vollständig | Strikte Plattformprüfung, bei Fehlschlag TOFU-Dialog mit SHA-256-Fingerabdruck; geänderte Zertifikate werden deutlich gewarnt. Keine globale Abschaltung. |
| Grafik (Bitmap-Codecs, RemoteFX, EGFX/H.264) | vollständig | H.264 über Windows Media Foundation (durch IronRDP). |
| Maus, Rad, X1/X2 | vollständig | Horizontales Scrollen abhängig vom Eingabegerät. |
| Tastatur (Scancodes) | vollständig (Windows) | Über Low-Level-Hook. **Pause/Untbr** wird nicht gesendet (benötigt eine Sonderfolge, die `ironrdp-input` nicht abbildet). |
| Strg+Alt+Entf | teilweise | Lokal nicht abfangbar (Windows SAS). Ersatz: Strg+Alt+Ende oder Tab-Kontextmenü. |
| IME/Unicode-Eingabe | nicht | Komposition asiatischer Eingabemethoden wird nicht übertragen (nur Scancodes). |
| Zwischenablage | teilweise | Text und Bilder in beide Richtungen. **Dateien** kopieren ist deaktiviert, da die Stream-Behandlung durch die einbettende Anwendung erfolgen muss. |
| Audio-Wiedergabe | vollständig | RDPSND über cpal/WASAPI (Feature `sound`, Standard). Mikrofon-Umleitung nicht in der UI. |
| Drucker | teilweise | Standarddrucker per RDPDR (IronRDP-Backend). Auswahl einzelner Drucker nicht in der UI. |
| Laufwerke | teilweise | Alle lokalen Laufwerke inkl. Hotplug-Auswahl beim Verbindungsstart; Auswahl einzelner Laufwerke nicht in der UI. |
| RD Gateway | teilweise | MS-TSGU (HTTPS/WebSocket, Fallback HTTP) über IronRDP. Gateway-Authentifizierung mit Benutzer/Passwort; Smartcard/Entra am Gateway nicht. |
| Dynamische Auflösung | vollständig* | Display Control (MS-RDPEDISP); falls der Server es nicht unterstützt, verbindet IronRDP mit neuer Größe neu. |
| Auto-Reconnect | vollständig | IronRDP-ARC-Cookie + anwendungsseitiger Neuaufbau mit Backoff. |
| Mehrere Monitore | nicht | Eine Sitzung = eine Fläche. |
| Smartcard-Anmeldung, Remote Credential Guard | nicht | IronRDP unterstützt Teile davon, die UI nicht. |
| RemoteApp (RAIL) | nicht | |

## Oberfläche

| Funktion | Status | Anmerkung |
|---|---|---|
| Tabs sortieren (Drag & Drop) | vollständig | |
| Tabs in Split-Bereiche ziehen | vollständig | Nur in den Split-Layouts (nebeneinander/übereinander/2×2). |
| Verbindungen per Drag & Drop in Ordner | vollständig | Auch auf „oberste Ebene“ und in Split-Bereiche. |
| Tastenkombinationen konfigurieren | nicht | Die Belegung ist fest und in den Einstellungen dokumentiert. |
| Light/Dark/System | vollständig | „System“ wird beim Start bzw. beim Umschalten gelesen, nicht live verfolgt. |
| Pro-Monitor-DPI | vollständig | Über Slint/winit; Remote-Skalierungsfaktor folgt dem Fenster. |

## Plattformen

* Ziel ist Windows 10/11 x64. Die Architektur ist plattformneutral (Slint, Tokio, IronRDP, rusqlite).
* Für Linux/macOS fehlen: nativer Credential-Store (derzeit nur Laufzeitspeicher), Tastatur-Erfassung mit Scancodes (kein globaler Hook; Ansatz: Slint-Plattform-Keycodes bzw. X11/Wayland-Scancodes), Autostart.

## Sicherheit

* Passwörter liegen nur im Windows Credential Manager und kurzzeitig im Arbeitsspeicher (`Secret`/`Zeroizing`). IronRDP erhält das Passwort als `String` in seiner Konfiguration; diese Kopie wird von IronRDP verwaltet und nicht explizit genullt.
* Für die Wiederverbindung wird das Passwort einer Sitzung bis zum Schließen des Tabs im Speicher gehalten.
* Der Tastatur-Hook ist systemweit installiert, wertet Ereignisse aber nur aus, solange Remora im Vordergrund ist und eine Remote-Anzeige fokussiert ist.

## Titelleiste und Installer

* Die integrierte Titelleiste zeigt beim Überfahren des Maximieren-Buttons nicht das Snap-Layout-Menü von Windows 11 (dafür müsste der Button als `HTMAXBUTTON` gemeldet werden). Snap per Ziehen an den Bildschirmrand und `Win+Pfeil`/`Win+Z` funktionieren.
* Unter Windows 10 fehlt am oberen Fensterrand die 1-px-Rahmenlinie, weil der Beschriftungsbereich zur Client-Fläche wird.
* Außerhalb von Windows wird die Standard-Titelleiste des Systems verwendet.
* Der Installer ersetzt nicht die Standardzuordnung von `.rdp` (Windows schützt sie per `UserChoice`); Remora erscheint unter „Öffnen mit“.
* Einzelinstanz gilt pro Windows-Sitzung. Startet ein anderer Benutzer Remora über „Ausführen als“ in derselben Sitzung, läuft er ohne Einzelinstanz weiter (die Pipe gehört dem ersten Benutzer).
* Installer und EXE sind nicht signiert; SmartScreen kann beim ersten Start warnen.

## Design (ora-Stack)

* Auswahlfelder, Zahlenfelder und Texteingaben stammen aus dem Fluent-Stil von Slint und übernehmen für Fokus und Auswahl die **Windows-Akzentfarbe**, nicht die App-Akzentfarbe (Slint erlaubt hier keine Überschreibung). Schalter und Kontrollkästchen sind deshalb eigene Komponenten (`ToggleSwitch`, `CheckField`).
* Text mit Farbverlauf (wie der Schriftzug in Proxora) unterstützt der Renderer nicht; Schriftzug und Überschriften sind einfarbig.
* Die Proxora-Hintergrundeffekte (schwebende Farbflächen, Weichzeichner) sind nicht übernommen.
* Light Mode ist nur auf der Einstellungsseite per Screenshot geprüft.
* Das Programmsymbol stammt in allen Größen aus der detailreichen Logo-Vorlage. Bei 16–20 px (Titelleiste, kleine Explorer-Ansicht, Infobereich) sind Monitore und „R“ nur noch als Farbfläche im Sechseck erkennbar.
