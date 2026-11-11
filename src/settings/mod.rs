//! Anwendungseinstellungen (TOML) und Pfade.

pub mod autostart;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::database::{Resolution, ScaleMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
}

impl ThemeMode {
    pub fn index(self) -> i32 {
        match self {
            ThemeMode::Dark => 0,
            ThemeMode::Light => 1,
            ThemeMode::System => 2,
        }
    }

    pub fn from_index(i: i32) -> Self {
        match i {
            1 => ThemeMode::Light,
            2 => ThemeMode::System,
            _ => ThemeMode::Dark,
        }
    }
}

/// Was beim Öffnen einer bereits verbundenen Verbindung passiert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OpenBehavior {
    /// Vorhandenen Tab aktivieren.
    #[default]
    FocusExisting,
    /// Immer eine weitere Sitzung öffnen.
    AlwaysNew,
}

/// Wann Windows-Tastenkombinationen (Win, Alt+Tab, …) an die Remote-Sitzung gehen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowsKeyMode {
    Always,
    #[default]
    FullscreenOnly,
    Never,
}

impl WindowsKeyMode {
    pub fn index(self) -> i32 {
        match self {
            WindowsKeyMode::Always => 0,
            WindowsKeyMode::FullscreenOnly => 1,
            WindowsKeyMode::Never => 2,
        }
    }

    pub fn from_index(i: i32) -> Self {
        match i {
            0 => WindowsKeyMode::Always,
            2 => WindowsKeyMode::Never,
            _ => WindowsKeyMode::FullscreenOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: ThemeMode,
    /// Akzentfarbe als `#rrggbb`.
    pub accent_color: String,
    pub default_scale_mode: ScaleMode,
    pub default_resolution: Resolution,
    pub auto_reconnect: bool,
    pub reconnect_attempts: u32,
    pub connect_timeout_secs: u32,
    pub open_behavior: OpenBehavior,
    pub confirm_close_active: bool,
    pub autostart: bool,
    /// Beim Start auf neue Versionen (GitHub-Releases) prüfen.
    pub check_updates: bool,
    pub windows_keys: WindowsKeyMode,
    pub sidebar_width: f32,
    pub sidebar_collapsed: bool,
    pub window_width: f32,
    pub window_height: f32,
}

/// Standard-Akzentfarbe (ora-Stack: Pink).
pub const DEFAULT_ACCENT: &str = "#ff006e";
/// Standard-Akzent bis Version 0.1.0; wird beim Laden auf [`DEFAULT_ACCENT`] umgestellt.
/// Eine bewusst gewählte andere Farbe bleibt unverändert.
const LEGACY_DEFAULT_ACCENT: &str = "#4c8dff";

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            accent_color: DEFAULT_ACCENT.to_owned(),
            default_scale_mode: ScaleMode::Fit,
            default_resolution: Resolution::FitToWindow,
            auto_reconnect: true,
            reconnect_attempts: 5,
            connect_timeout_secs: 20,
            open_behavior: OpenBehavior::FocusExisting,
            confirm_close_active: true,
            autostart: false,
            check_updates: true,
            windows_keys: WindowsKeyMode::FullscreenOnly,
            sidebar_width: 280.0,
            sidebar_collapsed: false,
            window_width: 1360.0,
            window_height: 860.0,
        }
    }
}

impl AppSettings {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<AppSettings>(&text) {
                Ok(s) => s.sanitized(),
                Err(e) => {
                    tracing::warn!(error = %e, "Einstellungsdatei ungültig, Standardwerte werden verwendet");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        // Atomar schreiben: erst temporäre Datei, dann umbenennen.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Korrigiert Werte außerhalb sinnvoller Grenzen.
    pub fn sanitized(mut self) -> Self {
        self.connect_timeout_secs = self.connect_timeout_secs.clamp(5, 300);
        self.reconnect_attempts = self.reconnect_attempts.min(50);
        self.sidebar_width = self.sidebar_width.clamp(200.0, 600.0);
        self.window_width = self.window_width.clamp(800.0, 10000.0);
        self.window_height = self.window_height.clamp(560.0, 10000.0);
        if parse_hex_color(&self.accent_color).is_none()
            || self.accent_color.eq_ignore_ascii_case(LEGACY_DEFAULT_ACCENT)
        {
            self.accent_color = DEFAULT_ACCENT.to_owned();
        }
        self
    }
}

/// `#rrggbb` → (r, g, b)
pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Verzeichnisse der Anwendung.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> anyhow::Result<Self> {
        // Portabler Modus: liegt `remora.portable` neben der EXE, werden alle Daten dort abgelegt.
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
            && dir.join("remora.portable").exists()
        {
            let base = dir.join("data");
            return Ok(Self {
                data_dir: base.clone(),
                config_dir: base.clone(),
                log_dir: base.join("logs"),
            });
        }
        let dirs = directories::ProjectDirs::from("de", "Remora", "Remora RDP")
            .ok_or_else(|| anyhow::anyhow!("Benutzerverzeichnis konnte nicht ermittelt werden"))?;
        Ok(Self {
            data_dir: dirs.data_dir().to_path_buf(),
            config_dir: dirs.config_dir().to_path_buf(),
            log_dir: dirs.data_local_dir().join("logs"),
        })
    }

    pub fn database(&self) -> PathBuf {
        self.data_dir.join("remora.sqlite3")
    }

    pub fn settings(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let s = AppSettings {
            theme: ThemeMode::Light,
            default_resolution: Resolution::Fixed {
                width: 1920,
                height: 1080,
            },
            ..Default::default()
        };
        s.save(&path).unwrap();
        assert_eq!(AppSettings::load(&path), s);
    }

    #[test]
    fn sanitizes_values() {
        let s = AppSettings {
            connect_timeout_secs: 1,
            accent_color: "blue".into(),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(s.connect_timeout_secs, 5);
        assert_eq!(s.accent_color, DEFAULT_ACCENT);
    }

    #[test]
    fn migrates_old_default_accent_only() {
        let old = AppSettings {
            accent_color: "#4C8DFF".into(),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(old.accent_color, DEFAULT_ACCENT);
        let custom = AppSettings {
            accent_color: "#06d6a0".into(),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(custom.accent_color, "#06d6a0");
    }

    #[test]
    fn partial_file_uses_defaults() {
        let s: AppSettings = toml::from_str("theme = \"light\"").unwrap();
        assert_eq!(s.theme, ThemeMode::Light);
        assert!(s.auto_reconnect);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#4c8dff"), Some((0x4c, 0x8d, 0xff)));
        assert_eq!(parse_hex_color("4c8dff"), None);
        assert_eq!(parse_hex_color("#zzzzzz"), None);
    }
}
