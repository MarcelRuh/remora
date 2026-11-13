# Changelog

Alle nennenswerten Änderungen an Remora (bis 0.2.x „Remora RDP“). Format nach [Keep a Changelog](https://keepachangelog.com/de/1.1.0/), Versionen nach [Semantic Versioning](https://semver.org/lang/de/).

## [Unreleased]

## [0.3.1] – 2026-11-13

### Behoben

- Bilder (und Text), die kopiert wurden, während Remora das aktive Fenster war, kamen nicht in der RDP-Sitzung an, z. B. Screenshots mit Win+Umschalt+S oder einem Screenshot-Tool per Tastenkürzel. Die Zwischenablage von IronRDP meldet lokale Änderungen nur, solange Remora im Hintergrund ist; Remora reicht sie jetzt selbst an die Sitzungen weiter. Das betrifft auch Text, der im SSH-Terminal von Remora kopiert wird.

## [0.3.0] – 2026-11-13

### Hinzugefügt

- SSH-Verbindungen mit eingebautem Terminal als Tab bzw. Split-Bereich, neben RDP in derselben Verbindungsliste. Terminal-Emulation mit `alacritty_terminal` (xterm-256color, Truecolor, Vollbildprogramme wie htop/mc/nano mit Mausbedienung, 10 000 Zeilen Verlauf), SSH mit `russh`. Darstellung mit den Windows-Schriften Cascadia Mono bzw. Consolas.
- Anmeldung per Passwort (auch Keyboard-Interactive, wie bei PAM üblich), Schlüsseldatei (OpenSSH/PEM; Passphrase im Windows Credential Manager) oder SSH-Agent (Windows-Dienst „OpenSSH Authentication Agent“ oder Pageant).
- Hostschlüsselprüfung nach dem Muster der RDP-Zertifikate: Rückfrage beim ersten Verbinden mit Fingerabdruck im `ssh-keygen`-Format, deutliche Warnung bei geändertem Schlüssel; vertraute Schlüssel erscheinen unter Einstellungen → Sicherheit.
- Tastatur im Terminal über die Zeichen von Windows (Tastaturlayout, AltGr, Tottasten); Pfeile, F-Tasten, Pos1/Ende usw. als xterm-Folgen. Kopieren per Markieren und Rechtsklick oder Strg+Umschalt+C, Einfügen per Rechtsklick oder Strg+Umschalt+V (mit Bracketed Paste). Die Sitzung kann die Zwischenablage nicht selbst lesen oder schreiben (OSC 52 wird ignoriert).
- Schnellverbindung erkennt `ssh benutzer@host`, `ssh://benutzer@host:port` und Adressen mit Port 22.
- Einstellung „Schriftgröße“ für das Terminal.

### Geändert

- Umbenannt in **Remora – Remote Connection Manager** (vorher „Remora RDP Manager“). Datenordner, Einträge im Credential Manager und die Namen der Release-Dateien bleiben, damit Updates und gespeicherte Passwörter weiter funktionieren. Neue Installationen gehen nach `C:\Program Files\Remora`, Updates bleiben im bisherigen Ordner; die alte Startmenü-Verknüpfung „Remora RDP“ wird ersetzt.
- Datenbank-Schema Version 2: Spalte für das Protokoll (bestehende Verbindungen bleiben RDP).
- Mindestversion von Rust zum Bauen: 1.89.

### Behoben

- Absturz bei Strg+Alt+Pos1 („Tastatur freigeben“) in einer Sitzung.
- Mauszeiger verschwand beim Tippen in der Sitzung und kam erst nach einem Klick wieder. Remora schickte Mausbewegungen nur bei gedrückter Taste an den Server; Windows blendet den beim Tippen versteckten Zeiger aber erst bei einer Bewegung wieder ein. Bewegungen ohne gedrückte Taste werden jetzt übertragen, damit funktionieren auch Hover-Effekte und Tooltips in der Sitzung.

## [0.2.4] – 2026-11-13

### Behoben

- Dynamische Auflösung: Kurz nach dem Verbinden fragte Remora die schon vorhandene Desktopgröße erneut an, solange noch kein Bild empfangen war. Der Server ignoriert eine unveränderte Größe, und nach drei Sekunden verband IronRDP neu („verbindet hin und wieder einfach so neu“).
- Nach einer Größenänderung, die IronRDP per Neuverbindung durchführt (Server reaktiviert nicht schnell genug), forderte Remora dieselbe Größe noch einmal an. Das löste eine zweite Neuverbindung aus, z. B. beim Verschieben des Fensters auf einen anderen Bildschirm. Remora verfolgt jetzt die angeforderten Größen bis zur Antwort des Servers. Eine Größe, die der Server wiederholt ablehnt, wird nicht endlos weiter angefordert.
- Verbindungsaufbau, Größenanfragen und vom Server gemeldete Desktopgrößen stehen jetzt im Protokoll (Stufe info).

## [0.2.3] – 2026-11-12

### Behoben

- Tastatureingaben kamen in RDP-Sitzungen nicht an, wenn Windows den Low-Level-Tastatur-Hook nicht aufruft (beobachtet: Hook installiert, aber nie aufgerufen). Tasten werden jetzt zusätzlich direkt aus den Tastennachrichten des Remora-Fensters mit Scancode an die Sitzung geleitet; der Hook dient weiterhin für Systemkombinationen wie Win oder Alt+Tab.

## [0.2.2] – 2026-11-12

### Behoben

- Tastatureingaben kamen nach „Erneut verbinden“ bzw. automatischem Wiederverbinden nicht mehr in der RDP-Sitzung an; ein Klick in die Anzeige half nicht. Nach einem Tabwechsel im selben Bereich gingen Tasten noch an die vorher angezeigte Sitzung. Das Tastaturziel folgt jetzt immer der Sitzung im fokussierten Bereich und ihrer aktuellen Verbindung.

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
