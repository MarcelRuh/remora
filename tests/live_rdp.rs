//! Live-Tests gegen einen echten RDP-Server. Standardmäßig ignoriert:
//!
//! ```text
//! cargo test --test live_rdp -- --ignored --nocapture
//! ```
//!
//! Zielserver über `REMORA_TEST_HOST` (Standard `127.0.0.2`). Es wird ausschließlich ein nicht
//! existierendes Konto verwendet – es findet keine erfolgreiche Anmeldung statt, eine laufende
//! Konsolensitzung wird daher nicht übernommen.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use remora::database::{Connection, Database};
use remora::rdp::client::IronRdpBackend;
use remora::rdp::{ConnectRequest, RdpBackend, SessionEvent, SessionEventSink, SessionId, SessionState};
use remora::security::Secret;
use remora::security::certificates::CertDecision;

struct ChannelSink {
    tx: Mutex<mpsc::Sender<SessionEvent>>,
    decision: CertDecision,
}

impl SessionEventSink for ChannelSink {
    fn emit(&self, _session: SessionId, event: SessionEvent) {
        if let SessionEvent::CertificatePrompt(p) = &event {
            println!("Zertifikat: {} – {} ({})", p.endpoint, p.fingerprint, p.reason);
            let _ = p.reply.send(self.decision);
        }
        let _ = self.tx.lock().unwrap().send(event);
    }
}

fn run(decision: CertDecision) -> (bool, SessionState) {
    let host = std::env::var("REMORA_TEST_HOST").unwrap_or_else(|_| "127.0.0.2".into());
    let db = Arc::new(Database::open_in_memory().unwrap());
    let backend = IronRdpBackend::new(db);
    let (tx, rx) = mpsc::channel();
    let sink = Arc::new(ChannelSink {
        tx: Mutex::new(tx),
        decision,
    });

    let mut conn = Connection::new("live", host);
    conn.username = "remora-nonexistent-probe".into();
    conn.options.clipboard = false;
    conn.options.audio = false;
    let req = ConnectRequest {
        session_id: 1,
        connection: conn,
        password: Some(Secret::new("not-a-real-password")),
        gateway_password: None,
        desktop_width: 1024,
        desktop_height: 768,
        scale_factor: 100,
        connect_timeout: Duration::from_secs(15),
        auto_reconnect_attempts: 0,
        terminal_font_size: 14.0,
    };
    let session = backend.connect(req, sink).expect("Sitzung startet");

    let mut saw_cert = false;
    let mut last = SessionState::Connecting;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(SessionEvent::CertificatePrompt(_)) => saw_cert = true,
            Ok(SessionEvent::State(s)) => {
                println!("Status: {}", s.label());
                let done = matches!(s, SessionState::Error { .. } | SessionState::Disconnected { .. });
                last = s;
                if done {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    drop(session);
    (saw_cert, last)
}

#[test]
#[ignore = "benötigt einen erreichbaren RDP-Server"]
fn live_rejected_certificate_aborts_before_authentication() {
    let (saw_cert, state) = run(CertDecision::Reject);
    assert!(saw_cert, "selbst signiertes Zertifikat muss eine Rückfrage auslösen");
    assert!(matches!(state, SessionState::Error { .. }), "{state:?}");
}

#[test]
#[ignore = "benötigt einen erreichbaren RDP-Server"]
fn live_tls_and_credssp_with_unknown_account_fails_cleanly() {
    let (saw_cert, state) = run(CertDecision::AcceptOnce);
    assert!(saw_cert);
    match state {
        SessionState::Error { message } => println!("Erwarteter Fehler: {message}"),
        other => panic!("unerwarteter Zustand: {other:?}"),
    }
}
