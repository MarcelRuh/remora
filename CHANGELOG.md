# Changelog

Alle nennenswerten Änderungen an Remora RDP. Format nach [Keep a Changelog](https://keepachangelog.com/de/1.1.0/), Versionen nach [Semantic Versioning](https://semver.org/lang/de/).

## [Unreleased]

## [0.2.1] – 2026-11-11

### Hinzugefügt

- Eingebauter Updater: „Jetzt installieren“ (Über die Anwendung bzw. Hinweis in der Statusleiste) lädt das Setup des neuen Releases, prüft seine SHA-256-Summe gegen `SHA256SUMS.txt`, installiert nach Rückfrage still und startet Remora danach neu. Portable Kopien öffnen weiterhin nur die Release-Seite.
- Releases enthalten `SHA256SUMS.txt` für Setup und portable ZIP.

### Entfernt

- Proxora-Import (Einstellungen → Proxora) aus 0.2.0.

## [0.2.0] – 2026-11-11

### Hinzugefügt

- Gestaltung im Stil des ora-Stacks (Proxora, Dockora, Sambora): Farben, Orbitron-Überschriften, Navigation, Kennzahlen-Kacheln auf dem Dashboard.
- Neues Logo und Programmsymbol; Setup-Assistent mit Logo.
- In das Design integrierte Titelleiste mit Snap-Layouts am Maximieren-Button (Windows 11).
- Einzelinstanz: Ein weiterer Start (z. B. Doppelklick auf eine `.rdp`-Datei) übergibt die Datei an das laufende Fenster.
- Update-Hinweis bei neuen GitHub-Releases (abschaltbar; öffnet nur die Release-Seite).
- Import von VMs aus Proxora (Einstellungen → Proxora) mit 2FA, Zertifikats-Pinning und Aktualisierung geänderter IP-Adressen.
- Installer (Inno Setup) nach `C:\Program Files\Remora RDP` mit optionaler „Öffnen mit“-Zuordnung für `.rdp`.
- CI (Format, Clippy, Tests) und automatische Releases mit Installer und portabler ZIP.

### Geändert

- Standard-Akzentfarbe Pink (`#ff006e`); der bisherige Standard `#4c8dff` wird beim Laden umgestellt, selbst gewählte Farben bleiben.
- Schalter und Kontrollkästchen sind eigene Komponenten und folgen der App-Akzentfarbe statt der Windows-Akzentfarbe.
- Lizenz: MIT (wie die übrigen ora-Projekte).

### Behoben

- Sidebar: Der Bereich „Gruppen“ dehnte sich und schob die Tag-Chips nach unten.
- Statusleiste: Verbindungsanzahl wurde nach Änderungen nicht aktualisiert („1 Verbindungen“).
- Verbindungseditor: Eingabefelder im Gateway-Reiter wurden in die Höhe gezogen.
- „Über die Anwendung“ zeigte als Slint-Version die Hauptversion von Remora.

## [0.1.0] – 2026-11-10

- Erste Version: Verbindungsverwaltung mit Ordnern, Tags, Favoriten und Verlauf; Tabs und Split-View; IronRDP-Sitzungen mit TLS/NLA, Zwischenablage, Audio, Laufwerken, Druckern, Gateway; Windows Credential Manager, TOFU-Zertifikatsprüfung, verschlüsselter Export.
