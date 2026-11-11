//! Umwandlung zwischen Domänenmodellen und Slint-Strukturen.

use chrono::{DateTime, Local, Utc};
use slint::{Color, SharedString};

use crate::database::{Connection, ConnectionOptions, Folder, FolderId, Resolution, ScaleMode};
use crate::settings::{AppSettings, OpenBehavior, ThemeMode, WindowsKeyMode, parse_hex_color};
use crate::ui::{ConnCard, EditorData, SettingsData};

/// Auswahlliste der Auflösungen (Index 0 = an Fenster anpassen, letzter = benutzerdefiniert).
pub const RESOLUTIONS: &[(u16, u16)] = &[
    (1280, 720),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (3840, 2160),
];

pub fn resolution_labels() -> Vec<SharedString> {
    let mut v = vec![SharedString::from("An Fenster anpassen (dynamisch)")];
    v.extend(
        RESOLUTIONS
            .iter()
            .map(|(w, h)| SharedString::from(format!("{w} × {h}"))),
    );
    v.push("Benutzerdefiniert …".into());
    v
}

pub fn resolution_to_index(r: Resolution) -> (i32, String, String) {
    match r {
        Resolution::FitToWindow => (0, "1920".into(), "1080".into()),
        Resolution::Fixed { width, height } => match RESOLUTIONS.iter().position(|&(w, h)| w == width && h == height) {
            Some(i) => (i as i32 + 1, width.to_string(), height.to_string()),
            None => (RESOLUTIONS.len() as i32 + 1, width.to_string(), height.to_string()),
        },
    }
}

pub fn index_to_resolution(index: i32, custom_w: &str, custom_h: &str) -> Result<Resolution, String> {
    if index <= 0 {
        return Ok(Resolution::FitToWindow);
    }
    let i = index as usize - 1;
    if let Some(&(width, height)) = RESOLUTIONS.get(i) {
        return Ok(Resolution::Fixed { width, height });
    }
    let w: u16 = custom_w.trim().parse().map_err(|_| "Ungültige Breite.".to_owned())?;
    let h: u16 = custom_h.trim().parse().map_err(|_| "Ungültige Höhe.".to_owned())?;
    Ok(Resolution::Fixed { width: w, height: h })
}

pub fn color_from_hex(s: &str) -> Color {
    let (r, g, b) = parse_hex_color(s).unwrap_or((0x4c, 0x8d, 0xff));
    Color::from_rgb_u8(r, g, b)
}

pub fn color_to_hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}

pub fn scale_to_index(s: ScaleMode) -> i32 {
    match s {
        ScaleMode::Fit => 0,
        ScaleMode::Original => 1,
        ScaleMode::Stretch => 2,
    }
}

pub fn index_to_scale(i: i32) -> ScaleMode {
    match i {
        1 => ScaleMode::Original,
        2 => ScaleMode::Stretch,
        _ => ScaleMode::Fit,
    }
}

const DEPTHS: [u8; 4] = [32, 24, 16, 15];

/// "vor 5 Min.", "gestern 14:03", "12.03.2026"
pub fn relative_time(t: DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now.signed_duration_since(t);
    let local = t.with_timezone(&Local);
    if diff.num_seconds() < 60 {
        "gerade eben".into()
    } else if diff.num_minutes() < 60 {
        format!("vor {} Min.", diff.num_minutes())
    } else if diff.num_hours() < 24 {
        format!("vor {} Std.", diff.num_hours())
    } else if diff.num_days() < 2 {
        format!("gestern {}", local.format("%H:%M"))
    } else if diff.num_days() < 7 {
        format!("vor {} Tagen", diff.num_days())
    } else {
        local.format("%d.%m.%Y").to_string()
    }
}

pub fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn subtitle(c: &Connection) -> String {
    let user = c.display_user();
    let host = if c.port == crate::database::DEFAULT_RDP_PORT {
        c.host.clone()
    } else {
        format!("{}:{}", c.host, c.port)
    };
    if user.is_empty() {
        host
    } else {
        format!("{user} @ {host}")
    }
}

pub fn card(c: &Connection, status: i32) -> ConnCard {
    ConnCard {
        id: c.id as i32,
        name: c.name.as_str().into(),
        host: c.host.as_str().into(),
        subtitle: subtitle(c).into(),
        color: color_from_hex(&c.color),
        icon: c.icon.as_str().into(),
        favorite: c.favorite,
        last_used: c.last_used.map(relative_time).unwrap_or_default().into(),
        status,
    }
}

/// Ordnerauswahl für den Editor: (Beschriftung, Ordner-ID), Index 0 = kein Ordner.
pub fn folder_choices(folders: &[Folder]) -> Vec<(String, Option<FolderId>)> {
    use std::collections::HashMap;
    let by_id: HashMap<FolderId, &Folder> = folders.iter().map(|f| (f.id, f)).collect();
    let mut out: Vec<(String, Option<FolderId>)> = folders
        .iter()
        .map(|f| {
            (
                crate::core::connection_manager::folder_path(&by_id, Some(f.id)),
                Some(f.id),
            )
        })
        .collect();
    out.sort_by_key(|(label, _)| label.to_lowercase());
    out.insert(0, ("(Kein Ordner)".into(), None));
    out
}

pub fn editor_from_connection(
    c: &Connection,
    folders: &[(String, Option<FolderId>)],
    password_stored: bool,
    gateway_password_stored: bool,
) -> EditorData {
    let o = &c.options;
    let (res_index, cw, ch) = resolution_to_index(o.resolution);
    EditorData {
        id: c.id as i32,
        name: c.name.as_str().into(),
        host: c.host.as_str().into(),
        port: c.port.to_string().into(),
        username: c.username.as_str().into(),
        domain: c.domain.as_str().into(),
        password: SharedString::default(),
        password_stored,
        password_action: 0,
        description: c.description.as_str().into(),
        folder_index: folders.iter().position(|(_, id)| *id == c.folder_id).unwrap_or(0) as i32,
        tags: c.tags.join(", ").into(),
        color: color_from_hex(&c.color),
        icon: c.icon.as_str().into(),
        favorite: c.favorite,
        resolution_index: res_index,
        custom_width: cw.into(),
        custom_height: ch.into(),
        color_depth_index: DEPTHS.iter().position(|d| *d == o.color_depth).unwrap_or(0) as i32,
        scale_mode: scale_to_index(o.scale_mode),
        fullscreen: o.fullscreen,
        clipboard: o.clipboard,
        audio: o.audio,
        printers: o.printers,
        drives: o.drives,
        nla: o.nla,
        admin_session: o.admin_session,
        auto_reconnect_index: match o.auto_reconnect {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        gateway_enabled: o.gateway.enabled,
        gateway_host: o.gateway.host.as_str().into(),
        gateway_username: o.gateway.username.as_str().into(),
        gateway_password: SharedString::default(),
        gateway_password_stored,
        gateway_bypass: o.gateway.bypass_for_direct,
        custom_properties: o.custom_rdp_properties.as_str().into(),
    }
}

/// Übernimmt die Editor-Daten in eine Verbindung (ohne Passwörter).
pub fn apply_editor(
    d: &EditorData,
    base: &Connection,
    folders: &[(String, Option<FolderId>)],
) -> Result<Connection, String> {
    let mut c = base.clone();
    c.name = d.name.trim().to_owned();
    c.host = d.host.trim().to_owned();
    c.port = d
        .port
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| "Der Port muss eine Zahl zwischen 1 und 65535 sein.".to_owned())?;
    c.username = d.username.trim().to_owned();
    c.domain = d.domain.trim().to_owned();
    // "DOMÄNE\benutzer" im Benutzerfeld akzeptieren
    if c.domain.is_empty()
        && let Some((dom, user)) = c.username.clone().split_once('\\')
    {
        c.domain = dom.to_owned();
        c.username = user.to_owned();
    }
    c.description = d.description.to_string();
    c.folder_id = folders.get(d.folder_index.max(0) as usize).and_then(|(_, id)| *id);
    c.tags = d
        .tags
        .split([',', ';'])
        .map(|t| t.trim().trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    c.tags.dedup();
    c.color = color_to_hex(d.color);
    c.icon = d.icon.to_string();
    c.favorite = d.favorite;

    let o: &mut ConnectionOptions = &mut c.options;
    o.resolution = index_to_resolution(d.resolution_index, &d.custom_width, &d.custom_height)?;
    o.color_depth = DEPTHS[d.color_depth_index.clamp(0, 3) as usize];
    o.scale_mode = index_to_scale(d.scale_mode);
    o.fullscreen = d.fullscreen;
    o.clipboard = d.clipboard;
    o.audio = d.audio;
    o.printers = d.printers;
    o.drives = d.drives;
    o.nla = d.nla;
    o.admin_session = d.admin_session;
    o.auto_reconnect = match d.auto_reconnect_index {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    };
    o.gateway.enabled = d.gateway_enabled;
    o.gateway.host = d.gateway_host.trim().to_owned();
    o.gateway.username = d.gateway_username.trim().to_owned();
    o.gateway.bypass_for_direct = d.gateway_bypass;
    o.custom_rdp_properties = d.custom_properties.to_string();
    c.validate()?;
    Ok(c)
}

pub fn settings_to_data(s: &AppSettings) -> SettingsData {
    SettingsData {
        theme: s.theme.index(),
        accent: color_from_hex(&s.accent_color),
        scale_mode: scale_to_index(s.default_scale_mode),
        resolution_index: resolution_to_index(s.default_resolution).0,
        auto_reconnect: s.auto_reconnect,
        reconnect_attempts: s.reconnect_attempts as i32,
        connect_timeout: s.connect_timeout_secs as i32,
        open_behavior: match s.open_behavior {
            OpenBehavior::FocusExisting => 0,
            OpenBehavior::AlwaysNew => 1,
        },
        confirm_close: s.confirm_close_active,
        autostart: s.autostart,
        windows_keys: s.windows_keys.index(),
    }
}

pub fn apply_settings_data(d: &SettingsData, s: &mut AppSettings) {
    s.theme = ThemeMode::from_index(d.theme);
    s.accent_color = color_to_hex(d.accent);
    s.default_scale_mode = index_to_scale(d.scale_mode);
    // Benutzerdefiniert ist für die Standardauflösung nicht vorgesehen
    let max = RESOLUTIONS.len() as i32;
    s.default_resolution = index_to_resolution(d.resolution_index.min(max), "", "").unwrap_or_default();
    s.auto_reconnect = d.auto_reconnect;
    s.reconnect_attempts = d.reconnect_attempts.clamp(1, 50) as u32;
    s.connect_timeout_secs = d.connect_timeout.clamp(5, 300) as u32;
    s.open_behavior = if d.open_behavior == 1 {
        OpenBehavior::AlwaysNew
    } else {
        OpenBehavior::FocusExisting
    };
    s.confirm_close_active = d.confirm_close;
    s.autostart = d.autostart;
    s.windows_keys = WindowsKeyMode::from_index(d.windows_keys);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_index_roundtrip() {
        for r in [
            Resolution::FitToWindow,
            Resolution::Fixed {
                width: 1920,
                height: 1080,
            },
            Resolution::Fixed {
                width: 1234,
                height: 777,
            },
        ] {
            let (i, w, h) = resolution_to_index(r);
            assert_eq!(index_to_resolution(i, &w, &h).unwrap(), r);
        }
        assert_eq!(resolution_labels().len(), RESOLUTIONS.len() + 2);
    }

    #[test]
    fn editor_roundtrip() {
        let mut c = Connection::new("Srv", "srv.local");
        c.id = 5;
        c.tags = vec!["prod".into()];
        c.options.color_depth = 16;
        c.options.gateway.enabled = true;
        c.options.gateway.host = "gw".into();
        let folders = vec![("(Kein Ordner)".to_owned(), None), ("A".to_owned(), Some(3))];
        c.folder_id = Some(3);
        let d = editor_from_connection(&c, &folders, true, false);
        assert_eq!(d.folder_index, 1);
        let back = apply_editor(&d, &c, &folders).unwrap();
        assert_eq!(back.options, c.options);
        assert_eq!(back.folder_id, Some(3));
        assert_eq!(back.tags, c.tags);
        assert_eq!(back.color, c.color);
    }

    #[test]
    fn editor_parses_domain_and_tags() {
        let c = Connection::new("x", "h");
        let folders = vec![("(Kein Ordner)".to_owned(), None)];
        let mut d = editor_from_connection(&c, &folders, false, false);
        d.username = "CORP\\admin".into();
        d.tags = "#Prod, kunde-a;  ".into();
        d.port = "abc".into();
        assert!(apply_editor(&d, &c, &folders).is_err());
        d.port = "3390".into();
        let r = apply_editor(&d, &c, &folders).unwrap();
        assert_eq!((r.domain.as_str(), r.username.as_str()), ("CORP", "admin"));
        assert_eq!(r.tags, vec!["prod".to_owned(), "kunde-a".to_owned()]);
        assert_eq!(r.port, 3390);
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(65), "01:05");
        assert_eq!(format_duration(3725), "1:02:05");
    }
}
