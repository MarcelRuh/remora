//! Live-Tests gegen einen SSH-Server. Standardmäßig ignoriert:
//!
//! ```text
//! cargo test --test live_ssh -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Ziel über `REMORA_SSH_TEST` (`host:port`, Standard `127.0.0.1:2222`), Anmeldung als
//! `user`/`pass`. Gedacht für einen Testserver, der Eingaben protokolliert; ein Server, der
//! „exit“ mit dem Schließen des Kanals beantwortet, genügt.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use remora::database::{Connection, Database, SshAuth};
use remora::rdp::input::{InputEvent, KeyMods, TermKey};
use remora::rdp::{ConnectRequest, RdpBackend, RdpSession, SessionEvent, SessionEventSink, SessionId, SessionState};
use remora::security::Secret;
use remora::security::certificates::CertDecision;
use remora::ssh::SshBackend;

struct ChannelSink {
    tx: Mutex<mpsc::Sender<SessionEvent>>,
    decision: CertDecision,
}

impl SessionEventSink for ChannelSink {
    fn emit(&self, _session: SessionId, event: SessionEvent) {
        if let SessionEvent::CertificatePrompt(p) = &event {
            println!("Hostschlüssel: {} – {}", p.endpoint, p.reason);
            let _ = p.reply.send(self.decision);
        }
        if let SessionEvent::Frame(f) = &event {
            f.ack.done();
        }
        let _ = self.tx.lock().unwrap().send(event);
    }
}

fn target() -> (String, u16) {
    let t = std::env::var("REMORA_SSH_TEST").unwrap_or_else(|_| "127.0.0.1:2222".into());
    let (h, p) = t.rsplit_once(':').expect("host:port");
    (h.to_owned(), p.parse().expect("Port"))
}

fn connect(
    password: &str,
    decision: CertDecision,
    db: Arc<Database>,
) -> (Box<dyn RdpSession>, mpsc::Receiver<SessionEvent>) {
    let (host, port) = target();
    let mut conn = Connection::new_ssh("live", host);
    conn.port = port;
    conn.username = "user".into();
    conn.options.ssh.auth = SshAuth::Password;
    let (tx, rx) = mpsc::channel();
    let sink = Arc::new(ChannelSink {
        tx: Mutex::new(tx),
        decision,
    });
    let req = ConnectRequest {
        session_id: 1,
        connection: conn,
        password: Some(Secret::new(password)),
        gateway_password: None,
        desktop_width: 1000,
        desktop_height: 600,
        scale_factor: 100,
        connect_timeout: Duration::from_secs(10),
        auto_reconnect_attempts: 0,
        terminal_font_size: 14.0,
    };
    let session = SshBackend::new(db).connect(req, sink).expect("Sitzung startet");
    (session, rx)
}

/// Wartet auf ein Ereignis, das `f` erfüllt; liefert alle bis dahin gesehenen Zustände.
fn wait_for(rx: &mpsc::Receiver<SessionEvent>, secs: u64, mut f: impl FnMut(&SessionEvent) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            if let SessionEvent::State(s) = &ev {
                println!("Status: {}", s.label());
            }
            if f(&ev) {
                return true;
            }
        }
    }
    false
}

#[test]
#[ignore = "benötigt einen SSH-Testserver"]
fn live_shell_session() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let (session, rx) = connect("pass", CertDecision::AcceptAlways, db.clone());
    assert!(wait_for(&rx, 15, |e| matches!(
        e,
        SessionEvent::State(SessionState::Connected)
    )));
    assert!(wait_for(&rx, 5, |e| matches!(
        e,
        SessionEvent::Resolution {
            width: 1000,
            height: 600
        }
    )));
    // Sichtbar → Bilder kommen
    session.set_visible(true);
    assert!(wait_for(&rx, 5, |e| matches!(e, SessionEvent::Frame(_))));

    for ch in "ls -la".chars() {
        session.send_input(InputEvent::Char { ch, alt: false });
    }
    session.send_input(InputEvent::Char { ch: '\r', alt: false });
    session.send_input(InputEvent::TermKey {
        key: TermKey::Up,
        mods: KeyMods::default(),
    });
    session.send_input(InputEvent::Char {
        ch: '\u{3}',
        alt: false,
    });
    session.resize(800, 400, 100);
    assert!(wait_for(&rx, 5, |e| matches!(
        e,
        SessionEvent::Resolution {
            width: 800,
            height: 400
        }
    )));
    session.paste("echo eins\necho zwei");
    session.send_input(InputEvent::Char { ch: '\r', alt: false });
    for ch in "exit\r".chars() {
        session.send_input(InputEvent::Char { ch, alt: false });
    }
    let mut end = None;
    assert!(wait_for(&rx, 10, |e| {
        if let SessionEvent::State(s @ (SessionState::Disconnected { .. } | SessionState::Error { .. })) = e {
            end = Some(s.clone());
            true
        } else {
            false
        }
    }));
    assert_eq!(
        end,
        Some(SessionState::Disconnected {
            reason: "Sitzung beendet".into()
        })
    );
    // Hostschlüssel wurde dauerhaft gespeichert
    let (host, port) = target();
    assert!(db.known_host(&format!("ssh://{host}:{port}")).unwrap().is_some());

    // Zweite Verbindung: bekannter Schlüssel, keine Rückfrage mehr
    let (session, rx) = connect("pass", CertDecision::Reject, db);
    let mut prompted = false;
    assert!(wait_for(&rx, 15, |e| {
        prompted |= matches!(e, SessionEvent::CertificatePrompt(_));
        matches!(e, SessionEvent::State(SessionState::Connected))
    }));
    assert!(!prompted);
    drop(session);
}

#[test]
#[ignore = "benötigt einen SSH-Testserver"]
fn live_wrong_password_and_rejected_key() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let (_s, rx) = connect("falsch", CertDecision::AcceptOnce, db.clone());
    let mut message = String::new();
    assert!(wait_for(&rx, 15, |e| {
        if let SessionEvent::State(SessionState::Error { message: m }) = e {
            message = m.clone();
            true
        } else {
            false
        }
    }));
    assert!(message.starts_with("Anmeldung fehlgeschlagen"), "{message}");

    // Abgelehnter Hostschlüssel: keine Anmeldung
    let (_s, rx) = connect("pass", CertDecision::Reject, db);
    let mut message = String::new();
    assert!(wait_for(&rx, 15, |e| {
        if let SessionEvent::State(SessionState::Error { message: m }) = e {
            message = m.clone();
            true
        } else {
            false
        }
    }));
    println!("Meldung: {message}");
    assert!(message.contains("Hostschlüssel"), "{message}");
}
