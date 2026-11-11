//! Zwischenablage-Umleitung (CLIPRDR).
//!
//! Unter Windows wird die native Zwischenablage über `ironrdp-cliprdr-native::WinClipboard`
//! angebunden. Das Objekt besitzt ein unsichtbares Fenster und muss daher auf dem UI-Thread
//! (mit Nachrichtenschleife) erzeugt und dort auch wieder freigegeben werden. Die Nachrichten
//! werden über den Input-Kanal der jeweiligen Sitzung in die RDP-Sitzung eingespeist.

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use ironrdp::client::rdp::RdpInputSender;
    use ironrdp::cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};

    /// Leitet Nachrichten der OS-Zwischenablage an die RDP-Sitzung weiter.
    #[derive(Clone, Debug)]
    pub struct ClipboardProxy {
        tx: RdpInputSender,
    }

    impl ClipboardProxy {
        pub fn new(tx: RdpInputSender) -> Self {
            Self { tx }
        }
    }

    impl ClipboardMessageProxy for ClipboardProxy {
        fn send_clipboard_message(&self, message: ClipboardMessage) {
            if self.tx.send_clipboard(message).is_err() {
                tracing::debug!("Zwischenablage-Nachricht verworfen: Sitzung ist beendet");
            }
        }
    }

    /// Initialisiert OLE für den aktuellen (UI-)Thread. Mehrfache Aufrufe sind unschädlich.
    pub fn ensure_ole_initialized() -> anyhow::Result<()> {
        // SAFETY: OleInitialize darf pro Thread mehrfach aufgerufen werden (S_FALSE).
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }
            .map_err(|e| anyhow::anyhow!("OLE konnte nicht initialisiert werden: {}", e.message()))
    }
}
