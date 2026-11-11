# Changelog

Alle nennenswerten Änderungen an Remora RDP. Format nach [Keep a Changelog](https://keepachangelog.com/de/1.1.0/), Versionen nach [Semantic Versioning](https://semver.org/lang/de/).

## [Unreleased]

## [0.2.0] – 2026-11-11

### Hinzugefügt

- Gestaltung im Stil des ora-Stacks (Proxora, Dockora, Sambora): Farben, Orbitron-Überschriften, Navigation, Kennzahlen-Kacheln auf dem Dashboard.
- Neues Logo und Programmsymbol; Setup-Assistent mit Logo.
- In das Design integrierte Titelleiste mit Snap-Layouts am Maximieren-Button (Windows 11).
- Einzelinstanz: Ein weiterer Start (z. B. Doppelklick auf eine `.rdp`-Datei) übergibt die Datei an das laufende Fenster.
- Update-Hinweis bei neuen GitHub-Releases (abschaltbar; öffnet nur die Release-Seite).
- Installer (Inno Setup) nach `C:\Program Files\Remora RDP` mit optionaler „Öffnen mit“-Zuordnung für `.rdp`.
- CI (Format, Clippy, Tests) und automatische Releases mit Installer und portabler ZIP.

### Geändert

- Standard-Akzentfarbe Pink (`#ff006e`); der bisherige Standard `#4c8dff` wird beim Laden umgestellt, selbst gewählte Farben bleiben.
- Schalter und Kontrollkästchen sind eigene Komponenten und folgen der App-Akzentfarbe statt der Windows-Akzentfarbe.
- Lizenz: MIT (wie die übrigen ora-Projekte).

### Behoben

- „Über die Anwendung“ zeigte als Slint-Version die Hauptversion von Remora.

## [0.1.0] – 2026-11-10

- Erste Version: Verbindungsverwaltung mit Ordnern, Tags, Favoriten und Verlauf; Tabs und Split-View; IronRDP-Sitzungen mit TLS/NLA, Zwischenablage, Audio, Laufwerken, Druckern, Gateway; Windows Credential Manager, TOFU-Zertifikatsprüfung, verschlüsselter Export.
