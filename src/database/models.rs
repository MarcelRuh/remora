//! Persistente Domänenmodelle.
//!
//! Passwörter sind bewusst **nicht** Teil dieser Strukturen: sie liegen ausschließlich im
//! Credential-Store des Betriebssystems (siehe [`crate::security::credentials`]).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type ConnectionId = i64;
pub type FolderId = i64;

/// Standard-RDP-Port.
pub const DEFAULT_RDP_PORT: u16 = 3389;
/// Standard-SSH-Port.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// Protokoll einer Verbindung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Rdp,
    Ssh,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Rdp => "rdp",
            Protocol::Ssh => "ssh",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("ssh") {
            Protocol::Ssh
        } else {
            Protocol::Rdp
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Rdp => DEFAULT_RDP_PORT,
            Protocol::Ssh => DEFAULT_SSH_PORT,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Rdp => "RDP",
            Protocol::Ssh => "SSH",
        }
    }
}

/// Anmeldeverfahren für SSH.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshAuth {
    /// Passwort (auch per Keyboard-Interactive, wie bei PAM üblich).
    #[default]
    // Nicht "password": Exporte werden auf versehentlich enthaltene Passwort-Felder geprüft.
    #[serde(rename = "password_auth")]
    Password,
    /// Private Schlüsseldatei (OpenSSH- oder PEM-Format); eine Passphrase liegt wie ein
    /// Passwort im Credential Manager.
    KeyFile,
    /// Schlüssel aus einem laufenden SSH-Agenten (Windows OpenSSH-Agent oder Pageant).
    Agent,
}

/// SSH-spezifische Einstellungen einer Verbindung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SshSettings {
    pub auth: SshAuth,
    /// Pfad zur privaten Schlüsseldatei (bei [`SshAuth::KeyFile`]).
    pub key_path: String,
}

/// Ordner zur hierarchischen Gruppierung von Verbindungen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub parent_id: Option<FolderId>,
    pub sort_order: i64,
    pub expanded: bool,
}

/// Wie die Remote-Auflösung bestimmt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Resolution {
    /// Auflösung folgt der Größe der Anzeigefläche (inkl. dynamischer Anpassung).
    #[default]
    FitToWindow,
    /// Feste Auflösung.
    Fixed { width: u16, height: u16 },
}

impl Resolution {
    pub fn label(&self) -> String {
        match self {
            Resolution::FitToWindow => "An Fenster anpassen".to_owned(),
            Resolution::Fixed { width, height } => format!("{width} × {height}"),
        }
    }
}

/// Wie das Remote-Bild in die Anzeigefläche eingepasst wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScaleMode {
    /// Seitenverhältnis erhalten, in die Fläche einpassen (Letterboxing).
    #[default]
    Fit,
    /// 1:1 ohne Skalierung (oben links verankert, Überstand wird abgeschnitten).
    Original,
    /// Fläche vollständig ausfüllen, Seitenverhältnis wird ignoriert.
    Stretch,
}

impl ScaleMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScaleMode::Fit => "fit",
            ScaleMode::Original => "original",
            ScaleMode::Stretch => "stretch",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "original" => ScaleMode::Original,
            "stretch" => ScaleMode::Stretch,
            _ => ScaleMode::Fit,
        }
    }
}

/// RD-Gateway-Konfiguration einer Verbindung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GatewaySettings {
    pub enabled: bool,
    /// `host[:port]` des Gateways.
    pub host: String,
    /// Benutzername für das Gateway (leer = Anmeldedaten der Verbindung verwenden).
    pub username: String,
    /// Gateway nur nutzen, wenn eine direkte Verbindung fehlschlägt.
    pub bypass_for_direct: bool,
}

/// Erweiterte Verbindungsoptionen. Wird als JSON gespeichert, damit neue Optionen ohne
/// Schema-Migration ergänzt werden können.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionOptions {
    pub resolution: Resolution,
    /// 15, 16, 24 oder 32 Bit.
    pub color_depth: u8,
    pub scale_mode: ScaleMode,
    pub fullscreen: bool,
    pub clipboard: bool,
    pub audio: bool,
    pub printers: bool,
    pub drives: bool,
    pub gateway: GatewaySettings,
    /// `None` = globale Einstellung verwenden.
    pub auto_reconnect: Option<bool>,
    /// Network Level Authentication (CredSSP) verwenden.
    pub nla: bool,
    /// Mit der Administrator-/Konsolensitzung verbinden.
    pub admin_session: bool,
    /// Zusätzliche, frei definierbare `.rdp`-Eigenschaften (`schlüssel:typ:wert` pro Zeile).
    pub custom_rdp_properties: String,
    /// Nur für SSH-Verbindungen.
    pub ssh: SshSettings,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            resolution: Resolution::FitToWindow,
            color_depth: 32,
            scale_mode: ScaleMode::Fit,
            fullscreen: false,
            clipboard: true,
            audio: true,
            printers: false,
            drives: false,
            gateway: GatewaySettings::default(),
            auto_reconnect: None,
            nla: true,
            admin_session: false,
            custom_rdp_properties: String::new(),
            ssh: SshSettings::default(),
        }
    }
}

/// Eine gespeicherte Verbindung (RDP oder SSH).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    /// Ältere Exporte kennen kein Protokoll → RDP.
    #[serde(default)]
    pub protocol: Protocol,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub description: String,
    pub folder_id: Option<FolderId>,
    pub tags: Vec<String>,
    /// Akzentfarbe als `#rrggbb`.
    pub color: String,
    /// Symbolname (siehe `ui/icons`).
    pub icon: String,
    pub favorite: bool,
    pub options: ConnectionOptions,
    pub sort_order: i64,
    pub last_used: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Connection {
    /// Neue, noch nicht gespeicherte Verbindung (`id == 0`).
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: 0,
            protocol: Protocol::Rdp,
            name: name.into(),
            host: host.into(),
            port: DEFAULT_RDP_PORT,
            username: String::new(),
            domain: String::new(),
            description: String::new(),
            folder_id: None,
            tags: Vec::new(),
            color: "#00b4d8".to_owned(),
            icon: "monitor".to_owned(),
            favorite: false,
            options: ConnectionOptions::default(),
            sort_order: 0,
            last_used: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Neue SSH-Verbindung (`id == 0`).
    pub fn new_ssh(name: impl Into<String>, host: impl Into<String>) -> Self {
        let mut c = Self::new(name, host);
        c.set_protocol(Protocol::Ssh);
        c
    }

    pub fn is_ssh(&self) -> bool {
        self.protocol == Protocol::Ssh
    }

    /// Wechselt das Protokoll; Standardport und -symbol folgen, eigene Werte bleiben.
    pub fn set_protocol(&mut self, protocol: Protocol) {
        if self.protocol == protocol {
            return;
        }
        if self.port == self.protocol.default_port() {
            self.port = protocol.default_port();
        }
        let default_icon = |p: Protocol| if p == Protocol::Ssh { "terminal" } else { "monitor" };
        if self.icon == default_icon(self.protocol) {
            self.icon = default_icon(protocol).to_owned();
        }
        self.protocol = protocol;
    }

    /// Schlüssel für vertraute Hosts. SSH-Hostschlüssel und RDP-Zertifikate desselben Hosts
    /// werden getrennt geführt.
    pub fn known_host_key(&self) -> String {
        match self.protocol {
            Protocol::Rdp => self.endpoint(),
            Protocol::Ssh => format!("ssh://{}", self.endpoint()),
        }
    }

    /// `host:port`, mit Klammern für IPv6-Adressen.
    pub fn endpoint(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Benutzername in der Form `DOMÄNE\benutzer` (nur zur Anzeige).
    pub fn display_user(&self) -> String {
        match (self.domain.is_empty(), self.username.is_empty()) {
            (_, true) => String::new(),
            (true, false) => self.username.clone(),
            (false, false) => format!("{}\\{}", self.domain, self.username),
        }
    }

    /// Prüft die Pflichtfelder.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Der Anzeigename darf nicht leer sein.".to_owned());
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Hostname oder IP-Adresse fehlt.".to_owned());
        }
        if host.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\') {
            return Err("Der Hostname enthält ungültige Zeichen.".to_owned());
        }
        if self.port == 0 {
            return Err("Der Port muss zwischen 1 und 65535 liegen.".to_owned());
        }
        if !matches!(self.options.color_depth, 15 | 16 | 24 | 32) {
            return Err("Die Farbtiefe muss 15, 16, 24 oder 32 Bit betragen.".to_owned());
        }
        if let Resolution::Fixed { width, height } = self.options.resolution
            && (!(200..=8192).contains(&width) || !(200..=8192).contains(&height))
        {
            return Err("Die Auflösung muss zwischen 200 und 8192 Pixeln liegen.".to_owned());
        }
        if self.is_ssh() {
            if self.options.ssh.auth == SshAuth::KeyFile && self.options.ssh.key_path.trim().is_empty() {
                return Err("Für die Anmeldung per Schlüssel fehlt die Schlüsseldatei.".to_owned());
            }
            return Ok(());
        }
        if self.options.gateway.enabled && self.options.gateway.host.trim().is_empty() {
            return Err("Für das RD-Gateway fehlt der Hostname.".to_owned());
        }
        Ok(())
    }
}

/// Zertifikat, dem der Benutzer explizit vertraut hat (Trust-on-first-use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    /// `host:port`
    pub endpoint: String,
    /// SHA-256 des DER-kodierten Zertifikats, hex, Doppelpunkt-getrennt.
    pub fingerprint: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// Parst eine Quick-Connect-Eingabe wie `server`, `server:3390`, `user@server`,
/// `DOMAIN\user@server:3390` oder `[::1]:3389`.
pub fn parse_quick_connect(input: &str) -> Option<(String, u16, String, String)> {
    parse_endpoint(input, DEFAULT_RDP_PORT)
}

/// Erkennt eine SSH-Schnellverbindung: `ssh user@host`, `ssh://user@host:2222` oder eine
/// Adresse mit Port 22. Liefert `(host, port, benutzer)`.
pub fn parse_ssh_quick_connect(input: &str) -> Option<(String, u16, String)> {
    let trimmed = input.trim();
    let lower = trimmed.to_ascii_lowercase();
    let explicit = if lower.starts_with("ssh://") {
        Some(&trimmed[6..])
    } else if lower.starts_with("ssh ") {
        Some(trimmed[4..].trim_start())
    } else {
        None
    };
    let (host, port, user, domain) = match explicit {
        Some(rest) => parse_endpoint(rest.trim_end_matches('/'), DEFAULT_SSH_PORT)?,
        None => {
            let parsed = parse_endpoint(trimmed, DEFAULT_RDP_PORT)?;
            if parsed.1 != DEFAULT_SSH_PORT {
                return None;
            }
            parsed
        }
    };
    // Eine Windows-Domäne gibt es bei SSH nicht; "a\b" wäre ein ungewöhnlicher Benutzername.
    let user = if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    };
    Some((host, port, user))
}

fn parse_endpoint(input: &str, default_port: u16) -> Option<(String, u16, String, String)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (user_part, host_part) = match input.rfind('@') {
        Some(idx) => (&input[..idx], &input[idx + 1..]),
        None => ("", input),
    };
    let (domain, username) = match user_part.split_once('\\') {
        Some((d, u)) => (d.to_owned(), u.to_owned()),
        None => (String::new(), user_part.to_owned()),
    };

    let (host, port) = if let Some(rest) = host_part.strip_prefix('[') {
        // IPv6 in Klammern
        let end = rest.find(']')?;
        let host = &rest[..end];
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        (host.to_owned(), port)
    } else if host_part.matches(':').count() == 1 {
        let (h, p) = host_part.split_once(':')?;
        (h.to_owned(), p.parse().ok()?)
    } else {
        // Hostname, IPv4 oder unverklammerte IPv6-Adresse
        (host_part.to_owned(), default_port)
    };

    if host.is_empty() || port == 0 || host.chars().any(char::is_whitespace) {
        return None;
    }
    Some((host, port, username, domain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_connect_variants() {
        assert_eq!(
            parse_quick_connect("srv01"),
            Some(("srv01".into(), 3389, String::new(), String::new()))
        );
        assert_eq!(
            parse_quick_connect("10.0.0.5:3390"),
            Some(("10.0.0.5".into(), 3390, String::new(), String::new()))
        );
        assert_eq!(
            parse_quick_connect("CORP\\admin@srv01.corp.local"),
            Some(("srv01.corp.local".into(), 3389, "admin".into(), "CORP".into()))
        );
        assert_eq!(
            parse_quick_connect("[fe80::1]:4000"),
            Some(("fe80::1".into(), 4000, String::new(), String::new()))
        );
        assert_eq!(
            parse_quick_connect("fe80::1"),
            Some(("fe80::1".into(), 3389, String::new(), String::new()))
        );
        assert_eq!(parse_quick_connect(""), None);
        assert_eq!(parse_quick_connect("host:abc"), None);
        assert_eq!(parse_quick_connect("bad host"), None);
    }

    #[test]
    fn ssh_quick_connect() {
        assert_eq!(
            parse_ssh_quick_connect("ssh root@pve-ct01"),
            Some(("pve-ct01".into(), 22, "root".into()))
        );
        assert_eq!(
            parse_ssh_quick_connect("ssh://admin@10.0.0.7:2222/"),
            Some(("10.0.0.7".into(), 2222, "admin".into()))
        );
        // Port 22 genügt als Hinweis auf SSH
        assert_eq!(
            parse_ssh_quick_connect("root@srv:22"),
            Some(("srv".into(), 22, "root".into()))
        );
        // Ohne Hinweis bleibt es RDP
        assert_eq!(parse_ssh_quick_connect("srv01"), None);
        assert_eq!(parse_ssh_quick_connect("srv01:3390"), None);
        assert_eq!(parse_ssh_quick_connect("ssh "), None);
    }

    #[test]
    fn protocol_switch_keeps_custom_values() {
        let mut c = Connection::new("x", "h");
        c.set_protocol(Protocol::Ssh);
        assert_eq!((c.port, c.icon.as_str()), (22, "terminal"));
        assert_eq!(c.known_host_key(), "ssh://h:22");
        c.port = 2222;
        c.icon = "server".into();
        c.set_protocol(Protocol::Rdp);
        assert_eq!((c.port, c.icon.as_str()), (2222, "server"));
        // SSH mit Schlüssel verlangt eine Schlüsseldatei
        let mut s = Connection::new_ssh("x", "h");
        s.options.ssh.auth = SshAuth::KeyFile;
        assert!(s.validate().is_err());
        s.options.ssh.key_path = "C:\\Users\\me\\.ssh\\id_ed25519".into();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn old_exports_default_to_rdp() {
        let mut c = Connection::new_ssh("x", "h");
        let mut json: serde_json::Value = serde_json::to_value(&c).unwrap();
        json.as_object_mut().unwrap().remove("protocol");
        let back: Connection = serde_json::from_value(json).unwrap();
        assert_eq!(back.protocol, Protocol::Rdp);
        c.protocol = Protocol::Ssh;
        let back: Connection = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.protocol, Protocol::Ssh);
    }

    #[test]
    fn endpoint_formats_ipv6() {
        let mut c = Connection::new("x", "::1");
        assert_eq!(c.endpoint(), "[::1]:3389");
        c.host = "server".into();
        c.port = 4000;
        assert_eq!(c.endpoint(), "server:4000");
    }

    #[test]
    fn validation() {
        let mut c = Connection::new("Test", "server");
        assert!(c.validate().is_ok());
        c.name = " ".into();
        assert!(c.validate().is_err());
        c.name = "Test".into();
        c.options.color_depth = 8;
        assert!(c.validate().is_err());
        c.options.color_depth = 24;
        c.options.resolution = Resolution::Fixed { width: 50, height: 600 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn options_json_is_forward_compatible() {
        // Fehlende Felder müssen mit Standardwerten aufgefüllt werden.
        let opts: ConnectionOptions = serde_json::from_str(r#"{"color_depth":16}"#).unwrap();
        assert_eq!(opts.color_depth, 16);
        assert!(opts.clipboard);
        assert_eq!(opts.resolution, Resolution::FitToWindow);
    }
}
