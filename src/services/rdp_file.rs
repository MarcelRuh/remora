//! Import und Export von Microsoft-`.rdp`-Dateien.
//!
//! Format: eine Eigenschaft pro Zeile, `name:typ:wert` mit Typ `i` (Ganzzahl), `s` (String)
//! oder `b` (Binär). mstsc schreibt UTF-16LE mit BOM, liest aber auch UTF-8.
//! Unbekannte Eigenschaften werden in `custom_rdp_properties` übernommen, damit sie beim
//! Verbinden an IronRDP weitergereicht und beim Export wieder geschrieben werden.

use crate::database::{Connection, DEFAULT_RDP_PORT, Resolution, ScaleMode};

/// Eigenschaften, die auf Felder des Modells abgebildet werden.
const MAPPED_KEYS: &[&str] = &[
    "full address",
    "server port",
    "username",
    "domain",
    "screen mode id",
    "desktopwidth",
    "desktopheight",
    "dynamic resolution",
    "smart sizing",
    "session bpp",
    "redirectclipboard",
    "audiomode",
    "redirectprinters",
    "drivestoredirect",
    "gatewayhostname",
    "gatewayusagemethod",
    "gatewayprofileusagemethod",
    "gatewaycredentialssource",
    "enablecredsspsupport",
    "administrative session",
    "connect to console",
    "autoreconnection enabled",
];

/// Eigenschaften, die niemals importiert werden (Passwörter gehören in den Credential-Store).
const SENSITIVE_KEYS: &[&str] = &["password 51", "password", "clear text password", "gatewaypassword"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdpProperty {
    pub key: String,
    pub kind: char,
    pub value: String,
}

/// Dekodiert den Dateiinhalt (UTF-16LE/BE mit BOM oder UTF-8).
pub fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Kodiert als UTF-16LE mit BOM (wie mstsc).
pub fn encode(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

pub fn parse_properties(text: &str) -> Vec<RdpProperty> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_matches(|c: char| c == '\u{feff}' || c.is_whitespace());
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(3, ':');
            let key = parts.next()?.trim().to_ascii_lowercase();
            let kind = parts.next()?.trim().chars().next()?.to_ascii_lowercase();
            let value = parts.next().unwrap_or("").to_owned();
            if key.is_empty() || !matches!(kind, 'i' | 's' | 'b') {
                return None;
            }
            Some(RdpProperty { key, kind, value })
        })
        .collect()
}

fn split_host_port(addr: &str) -> (String, Option<u16>) {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let host = rest[..end].to_owned();
        let port = rest[end + 1..].strip_prefix(':').and_then(|p| p.parse().ok());
        return (host, port);
    }
    if addr.matches(':').count() == 1 {
        let (h, p) = addr.split_once(':').unwrap_or((addr, ""));
        return (h.to_owned(), p.parse().ok());
    }
    (addr.to_owned(), None)
}

/// Erstellt eine Verbindung aus dem Inhalt einer `.rdp`-Datei.
pub fn import(text: &str, display_name: &str) -> Result<Connection, String> {
    let props = parse_properties(text);
    let get = |k: &str| props.iter().find(|p| p.key == k).map(|p| p.value.as_str());
    let get_i = |k: &str| get(k).and_then(|v| v.trim().parse::<i64>().ok());

    let address = get("full address")
        .or_else(|| get("alternate full address"))
        .ok_or("Die Datei enthält keine Serveradresse ('full address').")?;
    let (host, port) = split_host_port(address);
    let mut conn = Connection::new(display_name.trim(), host);
    conn.port = port
        .or_else(|| get_i("server port").and_then(|p| u16::try_from(p).ok()))
        .unwrap_or(DEFAULT_RDP_PORT);

    if let Some(user) = get("username") {
        match user.split_once('\\') {
            Some((d, u)) => {
                conn.domain = d.to_owned();
                conn.username = u.to_owned();
            }
            None => conn.username = user.to_owned(),
        }
    }
    if let Some(domain) = get("domain").filter(|d| !d.is_empty()) {
        conn.domain = domain.to_owned();
    }

    let o = &mut conn.options;
    o.fullscreen = get_i("screen mode id") == Some(2);
    let dynamic = get_i("dynamic resolution") == Some(1);
    match (get_i("desktopwidth"), get_i("desktopheight")) {
        (Some(w), Some(h)) if !dynamic && (200..=8192).contains(&w) && (200..=8192).contains(&h) => {
            o.resolution = Resolution::Fixed {
                width: w as u16,
                height: h as u16,
            };
        }
        _ => o.resolution = Resolution::FitToWindow,
    }
    o.scale_mode = if get_i("smart sizing") == Some(1) || dynamic {
        ScaleMode::Fit
    } else if matches!(o.resolution, Resolution::Fixed { .. }) {
        ScaleMode::Original
    } else {
        ScaleMode::Fit
    };
    if let Some(bpp) = get_i("session bpp")
        && matches!(bpp, 15 | 16 | 24 | 32)
    {
        o.color_depth = bpp as u8;
    }
    if let Some(v) = get_i("redirectclipboard") {
        o.clipboard = v == 1;
    }
    if let Some(v) = get_i("audiomode") {
        o.audio = v == 0;
    }
    if let Some(v) = get_i("redirectprinters") {
        o.printers = v == 1;
    }
    o.drives = get("drivestoredirect").is_some_and(|v| !v.trim().is_empty());
    if let Some(v) = get_i("enablecredsspsupport") {
        o.nla = v != 0;
    }
    o.admin_session = get_i("administrative session") == Some(1) || get_i("connect to console") == Some(1);
    if let Some(v) = get_i("autoreconnection enabled") {
        o.auto_reconnect = Some(v == 1);
    }
    if let Some(gw) = get("gatewayhostname").filter(|g| !g.trim().is_empty()) {
        let usage = get_i("gatewayusagemethod").unwrap_or(1);
        o.gateway.enabled = matches!(usage, 1 | 2);
        o.gateway.host = gw.trim().to_owned();
        o.gateway.bypass_for_direct = usage == 2;
    }

    // Unbekannte, nicht sensible Eigenschaften unverändert übernehmen.
    let custom: Vec<String> = props
        .iter()
        .filter(|p| !MAPPED_KEYS.contains(&p.key.as_str()) && !SENSITIVE_KEYS.contains(&p.key.as_str()))
        .filter(|p| p.key != "alternate full address")
        .map(|p| format!("{}:{}:{}", p.key, p.kind, p.value))
        .collect();
    o.custom_rdp_properties = custom.join("\n");

    if conn.name.is_empty() {
        conn.name = conn.host.clone();
    }
    conn.validate()?;
    Ok(conn)
}

/// Erzeugt den Text einer `.rdp`-Datei (ohne Passwörter).
pub fn export(conn: &Connection) -> String {
    let o = &conn.options;
    let mut lines: Vec<String> = Vec::new();
    let host = if conn.host.contains(':') {
        format!("[{}]", conn.host)
    } else {
        conn.host.clone()
    };
    if conn.port == DEFAULT_RDP_PORT {
        lines.push(format!("full address:s:{host}"));
    } else {
        lines.push(format!("full address:s:{host}:{}", conn.port));
    }
    if !conn.username.is_empty() {
        lines.push(format!("username:s:{}", conn.display_user()));
    }
    lines.push(format!("screen mode id:i:{}", if o.fullscreen { 2 } else { 1 }));
    match o.resolution {
        Resolution::Fixed { width, height } => {
            lines.push(format!("desktopwidth:i:{width}"));
            lines.push(format!("desktopheight:i:{height}"));
            lines.push("dynamic resolution:i:0".into());
        }
        Resolution::FitToWindow => lines.push("dynamic resolution:i:1".into()),
    }
    lines.push(format!(
        "smart sizing:i:{}",
        i32::from(o.scale_mode != ScaleMode::Original)
    ));
    lines.push(format!("session bpp:i:{}", o.color_depth));
    lines.push(format!("redirectclipboard:i:{}", i32::from(o.clipboard)));
    lines.push(format!("audiomode:i:{}", if o.audio { 0 } else { 2 }));
    lines.push(format!("redirectprinters:i:{}", i32::from(o.printers)));
    lines.push(format!("drivestoredirect:s:{}", if o.drives { "*" } else { "" }));
    lines.push(format!("enablecredsspsupport:i:{}", i32::from(o.nla)));
    lines.push(format!("administrative session:i:{}", i32::from(o.admin_session)));
    if let Some(ar) = o.auto_reconnect {
        lines.push(format!("autoreconnection enabled:i:{}", i32::from(ar)));
    }
    if o.gateway.enabled {
        lines.push(format!("gatewayhostname:s:{}", o.gateway.host));
        lines.push(format!(
            "gatewayusagemethod:i:{}",
            if o.gateway.bypass_for_direct { 2 } else { 1 }
        ));
        lines.push("gatewayprofileusagemethod:i:1".into());
        lines.push("gatewaycredentialssource:i:0".into());
    } else {
        lines.push("gatewayusagemethod:i:0".into());
    }
    for custom in parse_properties(&o.custom_rdp_properties) {
        if !SENSITIVE_KEYS.contains(&custom.key.as_str()) {
            lines.push(format!("{}:{}:{}", custom.key, custom.kind, custom.value));
        }
    }
    let mut text = lines.join("\r\n");
    text.push_str("\r\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "screen mode id:i:2\r\nuse multimon:i:0\r\ndesktopwidth:i:1920\r\ndesktopheight:i:1080\r\n\
session bpp:i:24\r\nfull address:s:srv01.contoso.local:3390\r\naudiomode:i:2\r\nredirectprinters:i:1\r\n\
redirectclipboard:i:0\r\nusername:s:CONTOSO\\admin\r\ndrivestoredirect:s:*\r\npassword 51:b:01000000D08C9DDF\r\n\
gatewayhostname:s:rdg.contoso.com\r\ngatewayusagemethod:i:2\r\nprompt for credentials:i:1\r\n";

    #[test]
    fn imports_standard_file() {
        let c = import(SAMPLE, "Server 01").unwrap();
        assert_eq!(c.name, "Server 01");
        assert_eq!(c.host, "srv01.contoso.local");
        assert_eq!(c.port, 3390);
        assert_eq!(c.username, "admin");
        assert_eq!(c.domain, "CONTOSO");
        assert!(c.options.fullscreen);
        assert_eq!(
            c.options.resolution,
            Resolution::Fixed {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(c.options.color_depth, 24);
        assert!(!c.options.audio);
        assert!(c.options.printers);
        assert!(!c.options.clipboard);
        assert!(c.options.drives);
        assert!(c.options.gateway.enabled);
        assert!(c.options.gateway.bypass_for_direct);
        assert_eq!(c.options.gateway.host, "rdg.contoso.com");
        // Unbekannte Werte bleiben erhalten, Passwörter werden verworfen.
        assert!(c.options.custom_rdp_properties.contains("use multimon:i:0"));
        assert!(c.options.custom_rdp_properties.contains("prompt for credentials:i:1"));
        assert!(!c.options.custom_rdp_properties.contains("password"));
    }

    #[test]
    fn utf16_roundtrip() {
        let c = import(SAMPLE, "Server 01").unwrap();
        let exported = export(&c);
        let bytes = encode(&exported);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let reimported = import(&decode(&bytes), "Server 01").unwrap();
        assert_eq!(reimported.host, c.host);
        assert_eq!(reimported.port, c.port);
        assert_eq!(reimported.username, c.username);
        assert_eq!(reimported.domain, c.domain);
        assert_eq!(reimported.options.resolution, c.options.resolution);
        assert_eq!(reimported.options.gateway, c.options.gateway);
        assert_eq!(
            reimported.options.custom_rdp_properties,
            c.options.custom_rdp_properties
        );
    }

    #[test]
    fn export_never_contains_password() {
        let mut c = Connection::new("x", "host");
        c.options.custom_rdp_properties = "password 51:b:ABCDEF\nkeyboardhook:i:2".into();
        let text = export(&c);
        assert!(!text.contains("password"));
        assert!(text.contains("keyboardhook:i:2"));
    }

    #[test]
    fn missing_address_is_error() {
        assert!(import("screen mode id:i:1", "x").is_err());
    }

    #[test]
    fn ipv6_address() {
        let c = import("full address:s:[fe80::1]:3390", "v6").unwrap();
        assert_eq!(c.host, "fe80::1");
        assert_eq!(c.port, 3390);
        assert!(export(&c).contains("full address:s:[fe80::1]:3390"));
    }
}
