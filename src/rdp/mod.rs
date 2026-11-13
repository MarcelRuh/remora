//! RDP-Schicht: protokollunabhängige Schnittstellen und die IronRDP-Implementierung.
//!
//! ```text
//!  UI-Thread                       Sitzungs-Thread (je Sitzung, eigene Tokio-Runtime)
//!  ─────────                       ───────────────────────────────────────────────
//!  SessionManager ──InputEvent──▶  IronRdpSession ──RdpInputEvent──▶ ironrdp-client (Netzwerk/Protokoll)
//!        ▲                                                                 │
//!        └──── SessionEvent (Status, Frames, Cursor, Zertifikat) ◀── Renderer (Framebuffer → RGBA)
//! ```
//!
//! Der UI-Thread führt niemals Netzwerkoperationen aus. Jede Sitzung besitzt einen eigenen
//! Thread, sodass langsame oder hängende Verbindungen sich nicht gegenseitig beeinflussen.

pub mod client;
pub mod clipboard;
pub mod input;
pub mod renderer;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use crate::database::Connection;
use crate::security::Secret;
use crate::security::certificates::CertificatePrompt;

pub use input::InputEvent;
pub use renderer::{CursorImage, FrameUpdate};

pub type SessionId = u64;

/// Lebenszyklus einer Sitzung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32, max: u32 },
    Disconnected { reason: String },
    Error { message: String },
}

impl SessionState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            SessionState::Connecting | SessionState::Connected | SessionState::Reconnecting { .. }
        )
    }

    /// Code für die UI: 0 getrennt, 1 verbinde, 2 verbunden, 3 Fehler, 4 Wiederverbindung.
    pub fn code(&self) -> i32 {
        match self {
            SessionState::Disconnected { .. } => 0,
            SessionState::Connecting => 1,
            SessionState::Connected => 2,
            SessionState::Error { .. } => 3,
            SessionState::Reconnecting { .. } => 4,
        }
    }

    pub fn label(&self) -> String {
        match self {
            SessionState::Connecting => "Verbindung wird hergestellt …".into(),
            SessionState::Connected => "Verbunden".into(),
            SessionState::Reconnecting { attempt, max } => format!("Wiederverbindung ({attempt}/{max}) …"),
            SessionState::Disconnected { reason } if reason.is_empty() => "Getrennt".into(),
            SessionState::Disconnected { reason } => format!("Getrennt – {reason}"),
            SessionState::Error { message } => format!("Fehler – {message}"),
        }
    }
}

/// Ereignisse einer Sitzung an die Anwendung. Werden vom Sitzungs-Thread gesendet.
#[derive(Debug)]
pub enum SessionEvent {
    State(SessionState),
    /// Neue Remote-Auflösung (nach Verbindungsaufbau oder dynamischer Anpassung).
    Resolution {
        width: u16,
        height: u16,
    },
    Frame(FrameUpdate),
    Cursor(CursorUpdate),
    CertificatePrompt(CertificatePrompt),
    /// Der Server hat eine Anmeldung abgeschlossen.
    LoggedOn,
    /// Text in die lokale Zwischenablage kopieren (Terminal-Auswahl).
    Clipboard(String),
    /// Die Sitzung möchte den Inhalt der Zwischenablage eingefügt bekommen (Rechtsklick).
    PasteRequest,
}

#[derive(Debug)]
pub enum CursorUpdate {
    Default,
    Hidden,
    Bitmap(CursorImage),
}

/// Empfänger für Sitzungsereignisse (Implementierung leitet in den UI-Event-Loop weiter).
pub trait SessionEventSink: Send + Sync {
    fn emit(&self, session: SessionId, event: SessionEvent);
}

/// Parameter für den Verbindungsaufbau.
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    pub session_id: SessionId,
    pub connection: Connection,
    pub password: Option<Secret>,
    pub gateway_password: Option<Secret>,
    /// Gewünschte Desktop-Größe in physischen Pixeln.
    pub desktop_width: u16,
    pub desktop_height: u16,
    /// Skalierungsfaktor in Prozent (100 = 96 DPI).
    pub scale_factor: u32,
    pub connect_timeout: Duration,
    /// Maximale Anzahl automatischer Wiederverbindungsversuche (0 = aus).
    pub auto_reconnect_attempts: u32,
    /// Schriftgröße für Terminal-Sitzungen in logischen Pixeln (bei 100 % Skalierung).
    pub terminal_font_size: f32,
}

/// Steuerung einer laufenden Sitzung. Alle Methoden sind nicht blockierend und dürfen aus dem
/// UI-Thread aufgerufen werden.
pub trait RdpSession {
    fn send_input(&self, event: InputEvent);
    /// Fordert eine neue Remote-Auflösung an (Display Control, sonst Reconnect).
    fn resize(&self, width: u16, height: u16, scale_factor: u32);
    /// Steuert, ob Frames an die UI geliefert werden (unsichtbare Tabs sparen Konvertierung).
    fn set_visible(&self, visible: bool);
    /// Ordentliches Trennen (RDP Shutdown), danach Abbruch.
    fn disconnect(&self);
    /// Die Sitzung erwartet Zeichen statt Scancodes (Terminal).
    fn text_input(&self) -> bool {
        false
    }
    /// Text einfügen (Terminal). RDP nutzt die geteilte Zwischenablage.
    fn paste(&self, _text: &str) {}
    /// Markierten Text als [`SessionEvent::Clipboard`] melden (Terminal).
    fn copy_selection(&self) {}
}

/// Austauschbare RDP-Implementierung.
pub trait RdpBackend {
    fn name(&self) -> &'static str;
    fn connect(&self, request: ConnectRequest, sink: Arc<dyn SessionEventSink>) -> anyhow::Result<Box<dyn RdpSession>>;
}
