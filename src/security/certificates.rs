//! TLS-Zertifikatsprüfung mit Trust-on-first-use (TOFU).
//!
//! IronRDP prüft das Serverzertifikat zuerst strikt gegen die Plattform-Vertrauensstellen
//! (`CertificateValidation::Strict`). Nur wenn diese Prüfung fehlschlägt (typisch: selbst
//! signierte RDP-Zertifikate), wird [`CertificateVerifier::verify`] aufgerufen:
//!
//! * Fingerabdruck bekannt und unverändert → Verbindung wird ohne Rückfrage fortgesetzt.
//! * Unbekannt → Benutzer wird gefragt (einmalig / dauerhaft vertrauen / abbrechen).
//! * Geändert → deutliche Warnung; standardmäßig wird abgebrochen.
//!
//! Es gibt keinen Weg, die Prüfung global oder automatisch zu deaktivieren.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::database::Database;

/// SHA-256-Fingerabdruck im Format `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let mut out = String::with_capacity(digest.len() * 3);
    for (i, b) in digest.iter().enumerate() {
        if i > 0 {
            out.push(':');
        }
        let _ = write!(out, "{b:02X}");
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    Trusted,
    Unknown,
    Changed { previous: String },
}

/// Entscheidung des Benutzers zu einem nicht verifizierbaren Zertifikat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertDecision {
    AcceptOnce,
    AcceptAlways,
    Reject,
}

/// Anfrage an die Oberfläche, über ein Zertifikat zu entscheiden.
#[derive(Debug)]
pub struct CertificatePrompt {
    pub endpoint: String,
    pub fingerprint: String,
    pub previous_fingerprint: Option<String>,
    pub reason: String,
    pub reply: mpsc::Sender<CertDecision>,
}

/// Callback, der eine [`CertificatePrompt`] an die UI weiterreicht (nicht blockierend).
pub type PromptSink = Arc<dyn Fn(CertificatePrompt) + Send + Sync>;

pub struct CertificateVerifier {
    db: Arc<Database>,
    endpoint: String,
    prompt: PromptSink,
    timeout: Duration,
}

impl CertificateVerifier {
    pub fn new(db: Arc<Database>, endpoint: impl Into<String>, prompt: PromptSink) -> Self {
        Self {
            db,
            endpoint: endpoint.into(),
            prompt,
            timeout: Duration::from_secs(180),
        }
    }

    pub fn trust_state(&self, fingerprint: &str) -> TrustState {
        match self.db.known_host(&self.endpoint) {
            Ok(Some(known)) if known.fingerprint == fingerprint => TrustState::Trusted,
            Ok(Some(known)) => TrustState::Changed {
                previous: known.fingerprint,
            },
            Ok(None) => TrustState::Unknown,
            Err(e) => {
                tracing::error!(error = %e, "Zertifikatsspeicher nicht lesbar");
                TrustState::Unknown
            }
        }
    }

    /// Wird aus dem TLS-Handshake (Sitzungs-Thread) aufgerufen und blockiert, bis der
    /// Benutzer entschieden hat. Der UI-Thread wird dabei nicht blockiert.
    pub fn verify(&self, der: &[u8], reason: &str) -> bool {
        let fp = fingerprint(der);
        let state = self.trust_state(&fp);
        if state == TrustState::Trusted {
            let _ = self.db.touch_known_host(&self.endpoint);
            tracing::info!(endpoint = %self.endpoint, "Zertifikat ist bekannt und vertrauenswürdig");
            return true;
        }

        let previous = match &state {
            TrustState::Changed { previous } => Some(previous.clone()),
            _ => None,
        };
        tracing::warn!(
            endpoint = %self.endpoint,
            changed = previous.is_some(),
            "Serverzertifikat konnte nicht verifiziert werden, Benutzer wird gefragt"
        );

        let (tx, rx) = mpsc::channel();
        (self.prompt)(CertificatePrompt {
            endpoint: self.endpoint.clone(),
            fingerprint: fp.clone(),
            previous_fingerprint: previous,
            reason: reason.to_owned(),
            reply: tx,
        });

        match rx.recv_timeout(self.timeout) {
            Ok(CertDecision::AcceptAlways) => {
                if let Err(e) = self.db.trust_host(&self.endpoint, &fp) {
                    tracing::error!(error = %e, "Zertifikat konnte nicht gespeichert werden");
                }
                true
            }
            Ok(CertDecision::AcceptOnce) => true,
            Ok(CertDecision::Reject) | Err(_) => false,
        }
    }

    /// Erzeugt den Callback im von IronRDP erwarteten Format.
    pub fn into_callback(self) -> ironrdp_tls_callback::Callback {
        let this = Arc::new(self);
        Arc::new(move |der: &[u8], _endpoint: &str, reason: &str| this.verify(der, reason))
    }
}

/// Typ-Alias, damit dieses Modul ohne direkte Abhängigkeit von `ironrdp-tls` testbar bleibt.
pub mod ironrdp_tls_callback {
    pub type Callback = std::sync::Arc<dyn Fn(&[u8], &str, &str) -> bool + Send + Sync>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verifier_with(db: Arc<Database>, decision: Option<CertDecision>) -> CertificateVerifier {
        let sink: PromptSink = Arc::new(move |p: CertificatePrompt| {
            if let Some(d) = decision {
                let _ = p.reply.send(d);
            }
        });
        let mut v = CertificateVerifier::new(db, "srv:3389", sink);
        v.timeout = Duration::from_millis(100);
        v
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"abc");
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert!(fp.starts_with("BA:78:16:BF"));
    }

    #[test]
    fn tofu_flow() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let cert_a = b"certificate-a";
        let cert_b = b"certificate-b";

        // Unbekannt + abgelehnt
        assert!(!verifier_with(db.clone(), Some(CertDecision::Reject)).verify(cert_a, "self signed"));
        // Unbekannt + keine Antwort (Timeout) → abgelehnt
        assert!(!verifier_with(db.clone(), None).verify(cert_a, "self signed"));
        // Einmalig akzeptiert → nicht gespeichert
        assert!(verifier_with(db.clone(), Some(CertDecision::AcceptOnce)).verify(cert_a, "x"));
        assert!(db.known_host("srv:3389").unwrap().is_none());
        // Dauerhaft akzeptiert → gespeichert
        assert!(verifier_with(db.clone(), Some(CertDecision::AcceptAlways)).verify(cert_a, "x"));
        // Danach ohne Rückfrage vertrauenswürdig
        assert!(verifier_with(db.clone(), None).verify(cert_a, "x"));
        // Geändertes Zertifikat wird erkannt
        let v = verifier_with(db.clone(), None);
        assert!(matches!(
            v.trust_state(&fingerprint(cert_b)),
            TrustState::Changed { .. }
        ));
        assert!(!v.verify(cert_b, "x"));
    }
}
