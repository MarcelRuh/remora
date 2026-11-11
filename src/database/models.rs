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
        }
    }
}

/// Eine gespeicherte RDP-Verbindung.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
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
            None if after.is_empty() => DEFAULT_RDP_PORT,
            None => return None,
        };
        (host.to_owned(), port)
    } else if host_part.matches(':').count() == 1 {
        let (h, p) = host_part.split_once(':')?;
        (h.to_owned(), p.parse().ok()?)
    } else {
        // Hostname, IPv4 oder unverklammerte IPv6-Adresse
        (host_part.to_owned(), DEFAULT_RDP_PORT)
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
