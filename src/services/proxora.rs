//! Import von VMs aus [Proxora](https://github.com/MarcelRuh/proxora) (ora-Stack).
//!
//! Ablauf: Anmeldung an der Proxora-API (`POST /api/auth/login`, Sitzungs-Cookie `pm_session`,
//! optional mit 2FA), Abruf der VM-Liste (`GET /api/dashboard/guests?kind=vm`), Nachladen
//! fehlender IP-Adressen (`POST /api/dashboard/guest-ips`) und Abmelden. Remora speichert
//! weder das Sitzungs-Cookie noch Proxmox-Zugangsdaten; das Proxora-Passwort liegt nur im
//! Windows Credential Manager.
//!
//! TLS: Zertifikate werden normal gegen den Windows-Zertifikatspeicher geprüft. Ein
//! selbstsigniertes Zertifikat (üblich bei Proxora im LAN) wird nur akzeptiert, wenn der
//! Benutzer dessen SHA-256-Fingerabdruck ausdrücklich bestätigt hat (Pinning, wie bei RDP).

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::Deserialize;

use crate::security::certificates::fingerprint;
use crate::security::credentials::Secret;

/// Markierung in der Beschreibung importierter Verbindungen (`proxora:<hostId>:<vmid>`),
/// über die ein erneuter Import die Verbindung wiederfindet.
pub const MARKER_PREFIX: &str = "proxora:";
const SESSION_COOKIE: &str = "pm_session";
/// Proxora nimmt höchstens 40 Gäste pro IP-Abfrage an.
const IP_CHUNK: usize = 40;

/// Anfrage an eine Proxora-Instanz.
pub struct ProxoraRequest {
    /// Basisadresse, z. B. `https://proxora.lan:3000`.
    pub url: String,
    pub username: String,
    pub password: Secret,
    /// Antwort auf eine 2FA-Abfrage aus einem vorherigen Versuch.
    pub totp: Option<TotpAnswer>,
    /// Vom Benutzer bestätigter Zertifikats-Fingerabdruck (`AB:CD:…`), leer = keiner.
    pub pinned_fingerprint: String,
}

pub struct TotpAnswer {
    pub ticket: String,
    pub code: String,
}

/// Eine VM aus Proxora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxoraVm {
    pub host_id: String,
    pub host_name: String,
    pub node: String,
    pub vmid: u32,
    pub name: String,
    pub status: String,
    pub tags: Vec<String>,
    pub ips: Vec<String>,
}

impl ProxoraVm {
    /// Eindeutiger Schlüssel innerhalb einer Proxora-Instanz.
    pub fn marker(&self) -> String {
        format!("{MARKER_PREFIX}{}:{}", self.host_id, self.vmid)
    }

    /// Bevorzugte Adresse für RDP: erste nutzbare IPv4, sonst erste IPv6, sonst keine.
    pub fn preferred_address(&self) -> Option<String> {
        let usable = |ip: &IpAddr| !ip.is_loopback() && !ip.is_unspecified() && !is_link_local(ip);
        let parsed: Vec<IpAddr> = self
            .ips
            .iter()
            .filter_map(|s| s.trim().parse().ok())
            .filter(usable)
            .collect();
        parsed
            .iter()
            .find(|ip| ip.is_ipv4())
            .or_else(|| parsed.first())
            .map(|ip| ip.to_string())
    }
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Liest die Markierung aus einer Verbindungsbeschreibung.
pub fn marker_in(description: &str) -> Option<&str> {
    description
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(MARKER_PREFIX))
}

#[derive(Debug, thiserror::Error)]
pub enum ProxoraError {
    #[error("Das Zertifikat von Proxora ist nicht vertrauenswürdig (SHA-256 {fingerprint})")]
    UntrustedCertificate { fingerprint: String },
    #[error("Proxora verlangt einen 2FA-Code")]
    TotpRequired { ticket: String },
    #[error("Benutzername oder Passwort ist falsch")]
    InvalidCredentials,
    #[error("Der 2FA-Code ist ungültig")]
    InvalidTotp,
    #[error("{0}")]
    Other(String),
}

impl From<reqwest::Error> for ProxoraError {
    fn from(e: reqwest::Error) -> Self {
        let mut msg = e.to_string();
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            msg = format!("{msg}: {s}");
            source = s.source();
        }
        ProxoraError::Other(format!("Proxora nicht erreichbar: {msg}"))
    }
}

/// Prüft und vereinheitlicht die eingegebene Adresse (`proxora.lan` → `https://proxora.lan`).
pub fn normalize_url(input: &str) -> Result<String, ProxoraError> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(ProxoraError::Other("Bitte die Proxora-Adresse angeben.".into()));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&with_scheme).map_err(|e| ProxoraError::Other(format!("Ungültige Adresse: {e}")))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return Err(ProxoraError::Other(
            "Adresse muss mit https:// oder http:// beginnen.".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProxoraError::Other("Zugangsdaten gehören nicht in die Adresse.".into()));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

// ---------------------------------------------------------------------------- TLS

/// Prüft gegen den Windows-Zertifikatspeicher; akzeptiert zusätzlich genau den bestätigten
/// Fingerabdruck. Merkt sich das vorgelegte Zertifikat für die Rückfrage.
#[derive(Debug)]
struct PinningVerifier {
    inner: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
    pinned: String,
    seen: Mutex<Option<String>>,
    rejected: AtomicBool,
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity.as_ref());
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(fp.clone());
        }
        if !self.pinned.is_empty() && self.pinned.eq_ignore_ascii_case(&fp) {
            return Ok(ServerCertVerified::assertion());
        }
        let result = self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now);
        if result.is_err() {
            self.rejected.store(true, Ordering::Relaxed);
        }
        result
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn build_verifier(pinned: &str) -> Result<Arc<PinningVerifier>, ProxoraError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| ProxoraError::Other(format!("Zertifikatsprüfung nicht verfügbar: {e}")))?;
    Ok(Arc::new(PinningVerifier {
        inner,
        provider,
        pinned: pinned.to_owned(),
        seen: Mutex::new(None),
        rejected: AtomicBool::new(false),
    }))
}

// ---------------------------------------------------------------------------- API

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    #[serde(default)]
    totp_required: bool,
    #[serde(default)]
    ticket: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    #[serde(default)]
    error: String,
    #[serde(default)]
    code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GuestDto {
    vmid: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    node: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    ips: Option<Vec<String>>,
    #[serde(default)]
    template: bool,
    host_id: String,
    #[serde(default)]
    host_name: Option<String>,
}

#[derive(Deserialize)]
struct GuestsResponse {
    #[serde(default)]
    vms: Vec<GuestDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IpRow {
    host_id: String,
    vmid: u32,
    #[serde(default)]
    ips: Vec<String>,
}

#[derive(Deserialize)]
struct IpsResponse {
    #[serde(default)]
    ips: Vec<IpRow>,
}

/// Wandelt die API-Antwort in VMs um (ohne Vorlagen), sortiert nach Host und Name.
fn parse_guests(json: &str) -> Result<Vec<ProxoraVm>, ProxoraError> {
    let parsed: GuestsResponse =
        serde_json::from_str(json).map_err(|e| ProxoraError::Other(format!("Unerwartete Antwort von Proxora: {e}")))?;
    let mut vms: Vec<ProxoraVm> = parsed
        .vms
        .into_iter()
        .filter(|g| !g.template)
        .map(|g| ProxoraVm {
            host_name: g.host_name.unwrap_or_else(|| g.host_id.clone()),
            host_id: g.host_id,
            node: g.node,
            vmid: g.vmid,
            name: if g.name.is_empty() {
                format!("VM {}", g.vmid)
            } else {
                g.name
            },
            status: g.status,
            tags: g
                .tags
                .unwrap_or_default()
                .split([';', ',', ' '])
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect(),
            ips: g.ips.unwrap_or_default(),
        })
        .collect();
    vms.sort_by(|a, b| {
        (a.host_name.to_lowercase(), a.name.to_lowercase()).cmp(&(b.host_name.to_lowercase(), b.name.to_lowercase()))
    });
    Ok(vms)
}

fn session_cookie(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .find_map(|pair| {
            pair.trim()
                .strip_prefix(&format!("{SESSION_COOKIE}="))
                .map(str::to_owned)
        })
        .filter(|v| !v.is_empty())
}

async fn error_of(response: reqwest::Response) -> ProxoraError {
    let status = response.status();
    let body: Option<ErrorResponse> = response.json().await.ok();
    let (msg, code) = body.map(|b| (b.error, b.code)).unwrap_or_default();
    match (status.as_u16(), code.as_str()) {
        (401, "INVALID_TOTP") => ProxoraError::InvalidTotp,
        (401, _) => ProxoraError::InvalidCredentials,
        (429, _) => ProxoraError::Other(
            "Zu viele Anmeldeversuche – Proxora sperrt kurzzeitig. Bitte später erneut versuchen.".into(),
        ),
        (403, _) => ProxoraError::Other("Keine Berechtigung, VMs in Proxora anzuzeigen.".into()),
        _ if !msg.is_empty() => ProxoraError::Other(format!("Proxora: {msg} (HTTP {status})")),
        _ => ProxoraError::Other(format!("Proxora antwortet mit HTTP {status}")),
    }
}

/// Meldet sich an, liest alle VMs samt IP-Adressen und meldet sich wieder ab (blockierend).
pub fn fetch_vms(req: &ProxoraRequest) -> Result<Vec<ProxoraVm>, ProxoraError> {
    let base = normalize_url(&req.url)?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let verifier = build_verifier(&req.pinned_fingerprint)?;
    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .user_agent(concat!("Remora-RDP/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(ProxoraError::from)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ProxoraError::Other(e.to_string()))?;

    let result = runtime.block_on(run(&client, &base, req));
    // Ein TLS-Fehler wegen eines unbekannten Zertifikats wird zur Rückfrage an den Benutzer.
    if result.is_err()
        && verifier.rejected.load(Ordering::Relaxed)
        && let Some(fp) = verifier.seen.lock().ok().and_then(|s| s.clone())
    {
        return Err(ProxoraError::UntrustedCertificate { fingerprint: fp });
    }
    result
}

async fn run(client: &reqwest::Client, base: &str, req: &ProxoraRequest) -> Result<Vec<ProxoraVm>, ProxoraError> {
    // Anmelden
    let body = match &req.totp {
        Some(t) => serde_json::json!({ "ticket": t.ticket, "totp": t.code.trim() }),
        None => serde_json::json!({ "username": req.username.trim(), "password": req.password.expose() }),
    };
    let response = client.post(format!("{base}/api/auth/login")).json(&body).send().await?;
    if !response.status().is_success() {
        return Err(error_of(response).await);
    }
    let cookie = session_cookie(&response);
    let login: LoginResponse = response
        .json()
        .await
        .map_err(|e| ProxoraError::Other(format!("Unerwartete Antwort von Proxora: {e}")))?;
    if login.totp_required {
        let ticket = login
            .ticket
            .ok_or_else(|| ProxoraError::Other("2FA-Ticket fehlt in der Antwort".into()))?;
        return Err(ProxoraError::TotpRequired { ticket });
    }
    let cookie = cookie.ok_or_else(|| ProxoraError::Other("Proxora hat keine Sitzung zurückgegeben".into()))?;
    let cookie_header = format!("{SESSION_COOKIE}={cookie}");

    let result = async {
        let response = client
            .get(format!("{base}/api/dashboard/guests?kind=vm"))
            .header(reqwest::header::COOKIE, &cookie_header)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(error_of(response).await);
        }
        let text = response.text().await?;
        let mut vms = parse_guests(&text)?;

        // Fehlende IPs nachladen (Proxora liest sie vom QEMU-Agent bzw. aus der Konfiguration)
        let missing: Vec<serde_json::Value> = vms
            .iter()
            .filter(|v| v.ips.is_empty() && !v.node.is_empty())
            .map(|v| serde_json::json!({ "hostId": v.host_id, "node": v.node, "vmid": v.vmid, "kind": "vm" }))
            .collect();
        for chunk in missing.chunks(IP_CHUNK) {
            let response = client
                .post(format!("{base}/api/dashboard/guest-ips"))
                .header(reqwest::header::COOKIE, &cookie_header)
                .json(&serde_json::json!({ "guests": chunk }))
                .send()
                .await?;
            if !response.status().is_success() {
                tracing::warn!(status = %response.status(), "Proxora: IP-Adressen nicht abrufbar");
                break;
            }
            let rows: IpsResponse = response
                .json()
                .await
                .map_err(|e| ProxoraError::Other(format!("Unerwartete Antwort von Proxora: {e}")))?;
            for row in rows.ips {
                if let Some(vm) = vms.iter_mut().find(|v| v.host_id == row.host_id && v.vmid == row.vmid) {
                    vm.ips = row.ips;
                }
            }
        }
        Ok(vms)
    }
    .await;

    // Abmelden (Sitzung serverseitig beenden), Fehler hier sind unerheblich
    let _ = client
        .post(format!("{base}/api/auth/logout"))
        .header(reqwest::header::COOKIE, &cookie_header)
        .send()
        .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(ips: &[&str]) -> ProxoraVm {
        ProxoraVm {
            host_id: "h1".into(),
            host_name: "pve-lab-01".into(),
            node: "pve".into(),
            vmid: 101,
            name: "win11".into(),
            status: "running".into(),
            tags: vec![],
            ips: ips.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(normalize_url("proxora.lan:3000/").unwrap(), "https://proxora.lan:3000");
        assert_eq!(normalize_url(" http://10.0.0.5 ").unwrap(), "http://10.0.0.5");
        assert!(normalize_url("").is_err());
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("https://admin:pw@proxora.lan").is_err());
    }

    #[test]
    fn preferred_address_skips_unusable() {
        assert_eq!(
            vm(&["127.0.0.1", "fe80::1", "169.254.1.2", "192.168.1.20"])
                .preferred_address()
                .as_deref(),
            Some("192.168.1.20")
        );
        assert_eq!(
            vm(&["fe80::1", "2001:db8::5"]).preferred_address().as_deref(),
            Some("2001:db8::5")
        );
        assert_eq!(vm(&["unsinn"]).preferred_address(), None);
        assert_eq!(vm(&[]).preferred_address(), None);
    }

    #[test]
    fn markers_roundtrip() {
        let v = vm(&[]);
        assert_eq!(v.marker(), "proxora:h1:101");
        let desc = format!("Importiert aus Proxora\n{}", v.marker());
        assert_eq!(marker_in(&desc), Some("proxora:h1:101"));
        assert_eq!(marker_in("ohne"), None);
    }

    #[test]
    fn guests_are_parsed_without_templates() {
        let json = r#"{"vms":[
            {"vmid":200,"name":"Zeta","node":"n","status":"stopped","template":false,"hostId":"b","hostName":"pve-b","tags":"windows;prod"},
            {"vmid":100,"name":"tpl","node":"n","status":"stopped","template":true,"hostId":"a"},
            {"vmid":101,"name":"","node":"n","status":"running","template":false,"hostId":"a","ips":["10.0.0.9"]}
        ],"containers":[]}"#;
        let vms = parse_guests(json).unwrap();
        assert_eq!(vms.len(), 2);
        assert_eq!(vms[0].host_name, "a");
        assert_eq!(vms[0].name, "VM 101");
        assert_eq!(vms[0].ips, vec!["10.0.0.9"]);
        assert_eq!(vms[1].tags, vec!["windows", "prod"]);
    }
}
