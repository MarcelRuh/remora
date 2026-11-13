//! IronRDP-Backend: Übersetzt eine gespeicherte Verbindung in eine `ironrdp-client`-Konfiguration
//! und startet die Sitzung.

use std::sync::Arc;

use anyhow::Context as _;
use ironrdp::client::config::{ClipboardType, ConfigBuilder, Destination, TransportKind};
use ironrdp::client::output_channel::output_channel;
use ironrdp::client::rdp::RdpClient;
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;

use super::session::{IronRdpSession, SessionContext};
use super::{ConnectRequest, RdpBackend, RdpSession, SessionEvent, SessionEventSink};
use crate::database::Database;
use crate::security::certificates::{CertificateVerifier, PromptSink};

/// Pfad, den mstsc/FreeRDP als Client-Verzeichnis melden (wird vom Server nur protokolliert).
const CLIENT_DIR: &str = "C:\\Windows\\System32\\mstscax.dll";

pub struct IronRdpBackend {
    db: Arc<Database>,
}

impl IronRdpBackend {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

fn client_build() -> u32 {
    let mut parts = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (a, b, c) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    a * 100 + b * 10 + c
}

fn client_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "remora".to_owned())
        .chars()
        .take(15)
        .collect()
}

fn platform() -> MajorPlatformType {
    if cfg!(windows) {
        MajorPlatformType::WINDOWS
    } else if cfg!(target_os = "macos") {
        MajorPlatformType::MACINTOSH
    } else {
        MajorPlatformType::UNIX
    }
}

/// Lokales Tastaturlayout (KLID), damit der Server dieselbe Belegung verwendet.
fn keyboard_layout() -> Option<u32> {
    #[cfg(windows)]
    {
        // SAFETY: GetKeyboardLayout(0) liefert das Layout des aktuellen Threads.
        let hkl = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout(0) };
        let lang = (hkl.0 as usize & 0xFFFF) as u32;
        (lang != 0).then_some(lang)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Baut die IronRDP-Konfiguration. Öffentlich für Tests.
pub fn build_config(
    req: &ConnectRequest,
    certificate_callback: Option<ironrdp_tls_cb::Callback>,
) -> anyhow::Result<ironrdp::client::config::Config> {
    let c = &req.connection;
    let o = &c.options;

    let mut b = ConfigBuilder::new()
        .with_destination(Destination::from_parts(c.host.trim(), c.port))
        .with_username(c.username.clone())
        .with_password(req.password.as_ref().map(|p| p.expose().to_owned()).unwrap_or_default())
        .with_client_build(client_build())
        .with_client_dir(CLIENT_DIR)
        .with_platform(platform())
        .with_client_name(client_name())
        .with_desktop_width(req.desktop_width.max(200))
        .with_desktop_height(req.desktop_height.max(200))
        .with_color_depth(u32::from(o.color_depth))
        .with_tls(true)
        .with_credssp(o.nla)
        .with_server_pointer(true)
        .with_pointer_software_rendering(false)
        .with_administrative_session(o.admin_session)
        .with_tcp_connect_attempt_timeout(req.connect_timeout)
        .with_clipboard(if o.clipboard {
            ClipboardType::Enable
        } else {
            ClipboardType::Disable
        });

    if (100..=500).contains(&req.scale_factor) && req.scale_factor != 100 {
        b = b.with_desktop_scale_factor(req.scale_factor);
    }
    if !c.domain.is_empty() {
        b = b.with_domain(c.domain.clone());
    }
    if let Some(layout) = keyboard_layout() {
        b = b.with_keyboard_layout(layout);
    }

    // TLS: Strikte Prüfung gegen die Plattform-Vertrauensstellen; selbst signierte Zertifikate
    // laufen über den TOFU-Callback (Benutzerentscheidung). Niemals stillschweigend akzeptieren.
    b = b.with_certificate_validation(ironrdp_tls::CertificateValidation::Strict);
    if let Some(cb) = certificate_callback {
        b = b.with_certificate_validation_callback(cb);
    }

    #[cfg(feature = "sound")]
    {
        b = b.with_sound(o.audio);
    }

    let rdpdr = cfg!(windows) && (o.drives || o.printers);
    b = b.with_rdpdr(rdpdr).with_printers(cfg!(windows) && o.printers);
    if cfg!(windows) && o.drives {
        b = b.with_drive_redirection(ironrdp::client::config::DriveRedirection::all());
    }

    if o.gateway.enabled {
        b = b.with_transport(TransportKind::Gateway {
            endpoint: o.gateway.host.trim().to_owned(),
            prefer_direct: o.gateway.bypass_for_direct,
        });
        let gw_user = if o.gateway.username.trim().is_empty() {
            c.display_user()
        } else {
            o.gateway.username.trim().to_owned()
        };
        let gw_pass = req
            .gateway_password
            .as_ref()
            .or(req.password.as_ref())
            .map(|p| p.expose().to_owned())
            .unwrap_or_default();
        b = b.with_gateway_username(gw_user).with_gateway_password(gw_pass);
    }

    // Individuelle `.rdp`-Eigenschaften zuletzt anwenden ("last writer wins").
    if !o.custom_rdp_properties.trim().is_empty() {
        let mut ps = ironrdp_propertyset::PropertySet::new();
        if let Err(errors) = ironrdp_rdpfile::load(&mut ps, &o.custom_rdp_properties) {
            tracing::warn!(
                count = errors.len(),
                "Einige individuelle RDP-Eigenschaften sind ungültig"
            );
        }
        b = b.with_property_set(&ps).context("Individuelle RDP-Eigenschaften")?;
    }

    let missing = b.missing();
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(ToString::to_string).collect();
        anyhow::bail!("Fehlende Angaben: {}", names.join(", "));
    }
    b.build()
}

/// Lokaler Alias für den Callback-Typ von `ironrdp-tls`.
mod ironrdp_tls_cb {
    pub type Callback = ironrdp_tls::CertificateValidationCallback;
}

impl RdpBackend for IronRdpBackend {
    fn name(&self) -> &'static str {
        "IronRDP"
    }

    fn connect(&self, req: ConnectRequest, sink: Arc<dyn SessionEventSink>) -> anyhow::Result<Box<dyn RdpSession>> {
        let session_id = req.session_id;
        let ctx = SessionContext::new();

        // Zertifikatsabfragen laufen über die UI; während eine Abfrage offen ist, pausiert der
        // Verbindungs-Timeout.
        let prompt_sink: PromptSink = {
            let sink = sink.clone();
            let ctx = ctx.clone();
            Arc::new(move |prompt| {
                ctx.set_prompt_open(true);
                sink.emit(session_id, SessionEvent::CertificatePrompt(prompt));
            })
        };
        let verifier = CertificateVerifier::new(self.db.clone(), req.connection.endpoint(), prompt_sink);
        let cert_callback: ironrdp_tls_cb::Callback = {
            let ctx = ctx.clone();
            let verifier = Arc::new(verifier);
            Arc::new(move |der: &[u8], _endpoint: &str, reason: &str| {
                let ok = verifier.verify(der, reason);
                ctx.set_prompt_open(false);
                ok
            })
        };

        let config = build_config(&req, Some(cert_callback))?;
        let (output_tx, output_rx) = output_channel(64);
        let mut client = RdpClient::new(config, output_tx).with_desktop_updates();
        if req.auto_reconnect_attempts > 0 {
            client = client.with_auto_reconnect(req.auto_reconnect_attempts);
        }

        #[cfg(windows)]
        let (client, native) = super::session::attach_windows_backends(client, &req.connection.options)?;

        let input = client.input_sender();
        let session = IronRdpSession::spawn(
            req,
            client,
            input,
            output_rx,
            sink,
            ctx,
            #[cfg(windows)]
            native,
        )?;
        Ok(Box::new(session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Connection;
    use crate::security::Secret;
    use std::time::Duration;

    fn request(conn: Connection) -> ConnectRequest {
        ConnectRequest {
            session_id: 1,
            connection: conn,
            password: Some(Secret::new("pw")),
            gateway_password: None,
            desktop_width: 1280,
            desktop_height: 720,
            scale_factor: 100,
            connect_timeout: Duration::from_secs(10),
            auto_reconnect_attempts: 3,
            terminal_font_size: 14.0,
        }
    }

    #[test]
    fn builds_basic_config() {
        let mut c = Connection::new("Test", "server.example");
        c.username = "admin".into();
        c.domain = "CORP".into();
        c.port = 3390;
        let cfg = build_config(&request(c), None).unwrap();
        assert_eq!(cfg.destination().name(), "server.example");
        assert_eq!(cfg.destination().port(), 3390);
        assert_eq!(cfg.connector().desktop_size.width, 1280);
    }

    #[test]
    fn builds_gateway_config() {
        let mut c = Connection::new("Test", "server.example");
        c.username = "admin".into();
        c.options.gateway.enabled = true;
        c.options.gateway.host = "rdg.example:443".into();
        let cfg = build_config(&request(c), None).unwrap();
        assert!(!matches!(cfg.transport(), ironrdp::client::config::Transport::Direct));
    }

    #[test]
    fn applies_custom_properties() {
        let mut c = Connection::new("Test", "server.example");
        c.username = "admin".into();
        c.options.custom_rdp_properties = "desktopwidth:i:1600\ndesktopheight:i:900".into();
        let cfg = build_config(&request(c), None).unwrap();
        assert_eq!(cfg.connector().desktop_size.width, 1600);
    }
}
