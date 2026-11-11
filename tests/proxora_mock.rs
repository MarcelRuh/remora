//! Proxora-Client gegen einen lokalen Nachbau der Proxora-API (HTTP und HTTPS).
//!
//! Der Mock-Server bildet die verwendeten Endpunkte nach: Anmeldung mit Sitzungs-Cookie
//! (optional mit 2FA), VM-Liste, IP-Nachladen und Abmelden. Geprüft werden Cookie-Weitergabe,
//! Auswertung, Fehlercodes und die Zertifikatsprüfung mit Pinning (selbstsigniertes
//! Testzertifikat in `tests/fixtures`, nur für `localhost`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use remora::security::certificates::fingerprint;
use remora::security::credentials::Secret;
use remora::services::proxora::{ProxoraError, ProxoraRequest, TotpAnswer, fetch_vms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const CERT: &[u8] = include_bytes!("fixtures/proxora-test-cert.pem");
const KEY: &[u8] = include_bytes!("fixtures/proxora-test-key.pem");
const COOKIE: &str = "pm_session=testsession123";

#[derive(Clone, Copy)]
struct Behaviour {
    totp: bool,
}

type Log = Arc<Mutex<Vec<String>>>;

fn read_request(stream: &mut impl Read) -> Option<(String, String, String, String)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut length = 0usize;
    let mut cookie = String::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("cookie:") {
            cookie = h[7..].trim().to_owned();
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some((method, path, cookie, String::from_utf8_lossy(&body).into_owned()))
}

fn respond(stream: &mut impl Write, status: &str, extra: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn handle(stream: &mut (impl Read + Write), behaviour: Behaviour, log: &Log) {
    let Some((method, path, cookie, body)) = read_request(stream) else {
        return;
    };
    log.lock().unwrap().push(format!("{method} {path}"));
    let authed = cookie == COOKIE;
    match (method.as_str(), path.as_str()) {
        ("POST", "/api/auth/login") => {
            let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if json["ticket"] == "t-1" && json["totp"] == "123456" {
                respond(
                    stream,
                    "200 OK",
                    &format!("Set-Cookie: {COOKIE}; Path=/; HttpOnly\r\n"),
                    "{\"id\":\"u\"}",
                );
            } else if json["ticket"].is_string() {
                respond(
                    stream,
                    "401 Unauthorized",
                    "",
                    "{\"error\":\"Invalid 2FA code\",\"code\":\"INVALID_TOTP\"}",
                );
            } else if json["username"] == "admin" && json["password"] == "geheim" {
                if behaviour.totp {
                    respond(stream, "200 OK", "", "{\"totpRequired\":true,\"ticket\":\"t-1\"}");
                } else {
                    respond(
                        stream,
                        "200 OK",
                        &format!("Set-Cookie: {COOKIE}; Path=/; HttpOnly; SameSite=Lax\r\n"),
                        "{\"id\":\"u\"}",
                    );
                }
            } else {
                respond(
                    stream,
                    "401 Unauthorized",
                    "",
                    "{\"error\":\"Invalid username or password\",\"code\":\"INVALID_CREDENTIALS\"}",
                );
            }
        }
        ("GET", "/api/dashboard/guests?kind=vm") if authed => respond(
            stream,
            "200 OK",
            "",
            r#"{"vms":[
                {"vmid":101,"name":"win11-client","node":"pve","status":"running","template":false,"hostId":"h1","hostName":"pve-lab-01","tags":"windows;lab"},
                {"vmid":102,"name":"srv-dc","node":"pve","status":"running","template":false,"hostId":"h1","hostName":"pve-lab-01","ips":["10.0.0.10"]},
                {"vmid":900,"name":"vorlage","node":"pve","status":"stopped","template":true,"hostId":"h1","hostName":"pve-lab-01"}
            ],"containers":[]}"#,
        ),
        ("POST", "/api/dashboard/guest-ips") if authed => {
            let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            assert_eq!(
                json["guests"].as_array().map(Vec::len),
                Some(1),
                "nur VM ohne IP nachladen"
            );
            respond(
                stream,
                "200 OK",
                "",
                r#"{"ips":[{"hostId":"h1","kind":"vm","node":"pve","vmid":101,"ips":["fe80::1","192.168.50.21"]}]}"#,
            );
        }
        ("POST", "/api/auth/logout") if authed => respond(stream, "200 OK", "", "{}"),
        _ => respond(stream, "401 Unauthorized", "", "{\"error\":\"Unauthorized\"}"),
    }
}

/// Startet den Mock-Server; liefert Basis-URL und Protokoll der Anfragen.
fn start(tls: bool, behaviour: Behaviour) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let server_log = log.clone();
    let config = tls.then(|| {
        let certs = vec![CertificateDer::from_pem_slice(CERT).unwrap()];
        let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        )
    });
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream): Result<TcpStream, _> = stream else {
                continue;
            };
            let log = server_log.clone();
            let config = config.clone();
            std::thread::spawn(move || match config {
                Some(cfg) => {
                    let conn = rustls::ServerConnection::new(cfg).unwrap();
                    let mut tls = rustls::StreamOwned::new(conn, stream);
                    handle(&mut tls, behaviour, &log);
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                }
                None => {
                    let mut stream = stream;
                    handle(&mut stream, behaviour, &log);
                }
            });
        }
    });
    let scheme = if tls { "https" } else { "http" };
    (format!("{scheme}://localhost:{port}"), log)
}

fn request(url: &str, password: &str) -> ProxoraRequest {
    ProxoraRequest {
        url: url.to_owned(),
        username: "admin".into(),
        password: Secret::new(password),
        totp: None,
        pinned_fingerprint: String::new(),
    }
}

#[test]
fn loads_vms_with_session_cookie_and_logs_out() {
    let (url, log) = start(false, Behaviour { totp: false });
    let vms = fetch_vms(&request(&url, "geheim")).expect("VMs laden");
    assert_eq!(vms.len(), 2, "Vorlagen werden ausgelassen");
    let win = vms.iter().find(|v| v.vmid == 101).unwrap();
    assert_eq!(win.preferred_address().as_deref(), Some("192.168.50.21"));
    assert_eq!(win.tags, vec!["windows", "lab"]);
    assert_eq!(win.marker(), "proxora:h1:101");
    let calls = log.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            "POST /api/auth/login",
            "GET /api/dashboard/guests?kind=vm",
            "POST /api/dashboard/guest-ips",
            "POST /api/auth/logout"
        ]
    );
}

#[test]
fn wrong_password_is_reported() {
    let (url, _) = start(false, Behaviour { totp: false });
    assert!(matches!(
        fetch_vms(&request(&url, "falsch")),
        Err(ProxoraError::InvalidCredentials)
    ));
}

#[test]
fn two_factor_flow() {
    let (url, _) = start(false, Behaviour { totp: true });
    let ticket = match fetch_vms(&request(&url, "geheim")) {
        Err(ProxoraError::TotpRequired { ticket }) => ticket,
        other => panic!("2FA erwartet, erhalten: {other:?}"),
    };
    let mut retry = request(&url, "");
    retry.totp = Some(TotpAnswer {
        ticket: ticket.clone(),
        code: "000000".into(),
    });
    assert!(matches!(fetch_vms(&retry), Err(ProxoraError::InvalidTotp)));
    retry.totp = Some(TotpAnswer {
        ticket,
        code: " 123456 ".into(),
    });
    assert_eq!(fetch_vms(&retry).expect("Anmeldung mit 2FA").len(), 2);
}

#[test]
fn self_signed_certificate_requires_pinned_fingerprint() {
    let (url, log) = start(true, Behaviour { totp: false });
    let expected = fingerprint(CertificateDer::from_pem_slice(CERT).unwrap().as_ref());

    // Unbekanntes Zertifikat: keine Anmeldedaten übertragen, Fingerabdruck zur Rückfrage
    match fetch_vms(&request(&url, "geheim")) {
        Err(ProxoraError::UntrustedCertificate { fingerprint }) => assert_eq!(fingerprint, expected),
        other => panic!("Zertifikatsrückfrage erwartet, erhalten: {other:?}"),
    }
    assert!(
        log.lock().unwrap().is_empty(),
        "vor der Bestätigung darf keine Anfrage ankommen"
    );

    // Falscher Fingerabdruck bleibt abgelehnt
    let mut wrong = request(&url, "geheim");
    wrong.pinned_fingerprint = "00:".repeat(31) + "00";
    assert!(matches!(
        fetch_vms(&wrong),
        Err(ProxoraError::UntrustedCertificate { .. })
    ));

    // Bestätigter Fingerabdruck: Verbindung klappt
    let mut pinned = request(&url, "geheim");
    pinned.pinned_fingerprint = expected;
    assert_eq!(fetch_vms(&pinned).expect("mit bestätigtem Zertifikat").len(), 2);
}
