//! SSH-Sitzungen mit eingebautem Terminal.
//!
//! ```text
//!  UI-Thread                       Sitzungs-Thread (je Sitzung, eigene Tokio-Runtime)
//!  ─────────                       ───────────────────────────────────────────────
//!  SessionManager ──InputEvent──▶  SshSession ──Bytes──▶ russh (Kanal mit Pseudo-Terminal)
//!        ▲                                                       │
//!        └──── SessionEvent (Status, Frames, Zwischenablage) ◀── TermView (Emulation → Bild)
//! ```
//!
//! Für die Oberfläche verhält sich eine SSH-Sitzung wie eine RDP-Sitzung mit dynamischer
//! Auflösung: Sie liefert Bilder in der Größe des Bereichs.

pub mod font;
pub mod session;
pub mod terminal;

use std::sync::Arc;

use crate::database::Database;
use crate::rdp::{ConnectRequest, RdpBackend, RdpSession, SessionEventSink};

pub struct SshBackend {
    db: Arc<Database>,
}

impl SshBackend {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

impl RdpBackend for SshBackend {
    fn name(&self) -> &'static str {
        "russh"
    }

    fn connect(&self, req: ConnectRequest, sink: Arc<dyn SessionEventSink>) -> anyhow::Result<Box<dyn RdpSession>> {
        Ok(Box::new(session::SshSession::spawn(req, self.db.clone(), sink)?))
    }
}

/// Ob die Schlüsseldatei eine Passphrase braucht. Nicht lesbare oder unbekannte Formate
/// gelten als ungeschützt; der Fehler zeigt sich dann beim Verbinden mit genauer Meldung.
pub fn key_needs_passphrase(path: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path.trim()) else {
        return false;
    };
    match russh::keys::PrivateKey::from_openssh(&text) {
        Ok(key) => key.is_encrypted(),
        // Ältere PEM-Schlüssel (z. B. von ssh-keygen -m PEM)
        Err(_) => text.contains("ENCRYPTED"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wegwerf-Testschlüssel aus `ssh-keygen -t ed25519` (ohne bzw. mit Passphrase „geheim“).
    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn detects_encrypted_keys() {
        assert!(!key_needs_passphrase(&fixture("ssh_ed25519_plain")));
        assert!(key_needs_passphrase(&fixture("ssh_ed25519_passphrase")));
        assert!(!key_needs_passphrase(&fixture("fehlt")));
        // Mit Passphrase lässt sich der Schlüssel laden, ohne nicht
        assert!(russh::keys::load_secret_key(fixture("ssh_ed25519_passphrase"), Some("geheim")).is_ok());
        assert!(matches!(
            russh::keys::load_secret_key(fixture("ssh_ed25519_passphrase"), None),
            Err(russh::keys::Error::KeyIsEncrypted)
        ));
    }
}
