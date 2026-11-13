//! Lebenszyklus einer SSH-Sitzung.
//!
//! Wie bei RDP läuft jede Sitzung in einem eigenen Thread mit eigener Tokio-Runtime: Aufbau,
//! Prüfung des Hostschlüssels (Trust-on-first-use, gemeinsam mit den RDP-Zertifikaten
//! gespeichert), Anmeldung, interaktive Shell mit Pseudo-Terminal. Die Ausgabe wird im
//! Sitzungs-Thread emuliert und als Bild an die UI geliefert.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind};
use tokio::sync::mpsc;

use super::terminal::TermView;
use crate::database::{Database, SshAuth};
use crate::rdp::input::{InputEvent, KeyMods};
use crate::rdp::renderer::Presenter;
use crate::rdp::{ConnectRequest, RdpSession, SessionEvent, SessionEventSink, SessionId, SessionState};
use crate::security::certificates::{CertificateVerifier, PromptSink};

const PRESENT_INTERVAL: Duration = Duration::from_millis(16);
/// Höchstdauer einer Rückfrage zum Hostschlüssel (wie bei Zertifikaten).
const PROMPT_ALLOWANCE: Duration = Duration::from_secs(185);
const TERM_NAME: &str = "xterm-256color";

pub(super) enum Cmd {
    Input(InputEvent),
    Resize { width: u16, height: u16, scale: u32 },
    Paste(String),
    Copy,
    Close,
}

pub struct SshSession {
    cmds: mpsc::UnboundedSender<Cmd>,
    visible: Arc<AtomicBool>,
}

impl SshSession {
    pub fn spawn(req: ConnectRequest, db: Arc<Database>, sink: Arc<dyn SessionEventSink>) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let visible = Arc::new(AtomicBool::new(false));
        let worker_visible = visible.clone();
        let id = req.session_id;
        std::thread::Builder::new()
            .name(format!("ssh-session-{id}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        sink.emit(
                            id,
                            SessionEvent::State(SessionState::Error {
                                message: format!("Laufzeitumgebung konnte nicht gestartet werden: {e}"),
                            }),
                        );
                        return;
                    }
                };
                runtime.block_on(run(req, db, sink, rx, worker_visible));
                runtime.shutdown_timeout(Duration::from_secs(2));
                tracing::debug!(session = id, "SSH-Sitzungs-Thread beendet");
            })?;
        Ok(Self { cmds: tx, visible })
    }
}

impl RdpSession for SshSession {
    fn send_input(&self, event: InputEvent) {
        let _ = self.cmds.send(Cmd::Input(event));
    }

    fn resize(&self, width: u16, height: u16, scale_factor: u32) {
        let _ = self.cmds.send(Cmd::Resize {
            width,
            height,
            scale: scale_factor,
        });
    }

    fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Release);
    }

    fn disconnect(&self) {
        let _ = self.cmds.send(Cmd::Close);
    }

    fn text_input(&self) -> bool {
        true
    }

    fn paste(&self, text: &str) {
        let _ = self.cmds.send(Cmd::Paste(text.to_owned()));
    }

    fn copy_selection(&self) {
        let _ = self.cmds.send(Cmd::Copy);
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Close);
    }
}

/// Schriftgröße in physischen Pixeln.
fn font_px(points: f32, scale: u32) -> f32 {
    points * scale.clamp(50, 500) as f32 / 100.0
}

/// Aktuell gedrückte Modifier (für Mausereignisse, die keine mitbringen).
fn current_mods() -> KeyMods {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
        // SAFETY: trivialer API-Aufruf
        let down = |vk: u16| unsafe { GetAsyncKeyState(i32::from(vk)) } as u16 & 0x8000 != 0;
        KeyMods {
            shift: down(VK_SHIFT.0),
            alt: down(VK_MENU.0),
            ctrl: down(VK_CONTROL.0),
        }
    }
    #[cfg(not(windows))]
    {
        KeyMods::default()
    }
}

struct Client {
    verifier: Arc<CertificateVerifier>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let public = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => russh::keys::PublicKey::from(cert.public_key().clone()),
        };
        let Ok(blob) = public.to_bytes() else {
            return Ok(false);
        };
        let algorithm = public.algorithm();
        let file = match algorithm.as_str() {
            a if a.contains("ed25519") => "ed25519",
            a if a.contains("ecdsa") => "ecdsa",
            a if a.contains("rsa") => "rsa",
            _ => "*",
        };
        let reason = format!(
            "SSH-Hostschlüssel {}. Zum Vergleich auf dem Server: ssh-keygen -lf /etc/ssh/ssh_host_{file}_key.pub",
            algorithm.as_str()
        );
        let verifier = self.verifier.clone();
        // Die Prüfung wartet ggf. auf die Entscheidung des Benutzers.
        let trusted = tokio::task::spawn_blocking(move || verifier.verify(&blob, &reason))
            .await
            .unwrap_or(false);
        Ok(trusted)
    }
}

/// Verständliche Meldung zu einem Verbindungsfehler.
fn friendly(stage: &str, error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let hint = if lower.contains("refused") || lower.contains("10061") {
        "Verbindung abgelehnt – läuft der SSH-Dienst und ist der Port korrekt?"
    } else if lower.contains("timed out") || lower.contains("timeout") || lower.contains("elapsed") {
        "Zeitüberschreitung – Server nicht erreichbar."
    } else if lower.contains("no such host") || lower.contains("11001") || lower.contains("not known") {
        "Hostname konnte nicht aufgelöst werden."
    } else if lower.contains("unknownkey") || lower.contains("unknown key") || lower.contains("server key") {
        "Der Hostschlüssel des Servers wurde nicht akzeptiert."
    } else if lower.contains("network") || lower.contains("10065") || lower.contains("10051") {
        "Netzwerk nicht erreichbar."
    } else {
        return format!("{stage}: {error}");
    };
    format!("{hint} ({error})")
}

/// Verbindet, prüft den Hostschlüssel und meldet an.
async fn connect(req: &ConnectRequest, db: Arc<Database>, prompt: PromptSink) -> Result<Handle<Client>, String> {
    let c = &req.connection;
    let host = c.host.trim().trim_start_matches('[').trim_end_matches(']').to_owned();
    let tcp = tokio::time::timeout(
        req.connect_timeout,
        tokio::net::TcpStream::connect((host.as_str(), c.port)),
    )
    .await
    .map_err(|_| {
        format!(
            "Zeitüberschreitung beim Verbindungsaufbau ({} s).",
            req.connect_timeout.as_secs()
        )
    })?
    .map_err(|e| friendly("Verbindung fehlgeschlagen", &e.to_string()))?;
    let _ = tcp.set_nodelay(true);

    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    });
    let verifier = Arc::new(CertificateVerifier::new(db, c.known_host_key(), prompt));
    let handshake = client::connect_stream(config, tcp, Client { verifier });
    let mut handle = tokio::time::timeout(req.connect_timeout + PROMPT_ALLOWANCE, handshake)
        .await
        .map_err(|_| "Zeitüberschreitung beim SSH-Handshake.".to_owned())?
        .map_err(|e| friendly("SSH-Handshake fehlgeschlagen", &e.to_string()))?;

    tokio::time::timeout(req.connect_timeout, authenticate(&mut handle, req))
        .await
        .map_err(|_| "Zeitüberschreitung bei der Anmeldung.".to_owned())??;
    Ok(handle)
}

const AUTH_FAILED: &str = "Anmeldung fehlgeschlagen";

async fn authenticate(handle: &mut Handle<Client>, req: &ConnectRequest) -> Result<(), String> {
    let c = &req.connection;
    let user = c.username.trim();
    if user.is_empty() {
        return Err(format!("{AUTH_FAILED} – kein Benutzername angegeben."));
    }
    let secret = req.password.as_ref().map(|p| p.expose().to_owned());
    let err = |e: russh::Error| format!("{AUTH_FAILED}: {e}");
    match c.options.ssh.auth {
        SshAuth::Password => {
            let password = secret.unwrap_or_default();
            let result = handle
                .authenticate_password(user, password.clone())
                .await
                .map_err(err)?;
            if result.success() {
                return Ok(());
            }
            // Viele Server (PAM) erlauben Passwörter nur per Keyboard-Interactive.
            if let client::AuthResult::Failure { remaining_methods, .. } = &result
                && remaining_methods.contains(&MethodKind::KeyboardInteractive)
                && keyboard_interactive(handle, user, &password).await.map_err(err)?
            {
                return Ok(());
            }
            Err(format!("{AUTH_FAILED} – Benutzername oder Passwort falsch."))
        }
        SshAuth::KeyFile => {
            let path = c.options.ssh.key_path.trim();
            let key =
                russh::keys::load_secret_key(path, secret.as_deref().filter(|s| !s.is_empty())).map_err(
                    |e| match e {
                        russh::keys::Error::KeyIsEncrypted => {
                            format!("{AUTH_FAILED} – der Schlüssel ist mit einer Passphrase geschützt.")
                        }
                        other => format!("{AUTH_FAILED} – Schlüsseldatei „{path}“ nicht lesbar: {other}"),
                    },
                )?;
            let hash = handle.best_supported_rsa_hash().await.map_err(err)?.flatten();
            let result = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(err)?;
            if result.success() {
                Ok(())
            } else {
                Err(format!("{AUTH_FAILED} – der Server akzeptiert den Schlüssel nicht."))
            }
        }
        SshAuth::Agent => agent_auth(handle, user).await,
    }
}

/// Beantwortet Keyboard-Interactive-Abfragen mit dem Passwort (verdeckte Eingaben).
async fn keyboard_interactive(handle: &mut Handle<Client>, user: &str, password: &str) -> Result<bool, russh::Error> {
    let mut reply = handle
        .authenticate_keyboard_interactive_start(user, None::<String>)
        .await?;
    // Server stellen oft eine leere Abschlussrunde; mehr als einige Runden sind unüblich.
    for _ in 0..4 {
        match reply {
            client::KeyboardInteractiveAuthResponse::Success => return Ok(true),
            client::KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            client::KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = prompts
                    .iter()
                    .map(|p| if p.echo { String::new() } else { password.to_owned() })
                    .collect();
                reply = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
    Ok(false)
}

async fn agent_auth(handle: &mut Handle<Client>, user: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use russh::keys::agent::client::AgentClient;
        const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
        let identities_and_auth = async {
            if let Ok(mut agent) = AgentClient::connect_named_pipe(OPENSSH_PIPE).await {
                return try_agent_keys(handle, user, &mut agent).await;
            }
            match AgentClient::connect_pageant().await {
                Ok(mut agent) => try_agent_keys(handle, user, &mut agent).await,
                Err(_) => Err(format!(
                    "{AUTH_FAILED} – kein SSH-Agent erreichbar (Dienst „OpenSSH Authentication Agent“ oder Pageant starten)."
                )),
            }
        };
        identities_and_auth.await
    }
    #[cfg(not(windows))]
    {
        let _ = (handle, user);
        Err(format!("{AUTH_FAILED} – SSH-Agent wird nur unter Windows unterstützt."))
    }
}

#[cfg(windows)]
async fn try_agent_keys<S>(
    handle: &mut Handle<Client>,
    user: &str,
    agent: &mut russh::keys::agent::client::AgentClient<S>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| format!("{AUTH_FAILED} – SSH-Agent antwortet nicht: {e}"))?;
    if identities.is_empty() {
        return Err(format!("{AUTH_FAILED} – der SSH-Agent enthält keine Schlüssel."));
    }
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        match handle.authenticate_publickey_with(user, key, hash, agent).await {
            Ok(result) if result.success() => return Ok(()),
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "Schlüssel aus dem Agenten abgelehnt"),
        }
    }
    Err(format!(
        "{AUTH_FAILED} – der Server akzeptiert keinen Schlüssel aus dem SSH-Agenten."
    ))
}

async fn run(
    req: ConnectRequest,
    db: Arc<Database>,
    sink: Arc<dyn SessionEventSink>,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    visible: Arc<AtomicBool>,
) {
    let id: SessionId = req.session_id;
    sink.emit(id, SessionEvent::State(SessionState::Connecting));

    let prompt: PromptSink = {
        let sink = sink.clone();
        Arc::new(move |p| sink.emit(id, SessionEvent::CertificatePrompt(p)))
    };

    // Aufbau; währenddessen nur Schließen und Größenänderungen beachten
    let mut size = (req.desktop_width.max(200), req.desktop_height.max(200));
    let mut scale = req.scale_factor;
    let connecting = connect(&req, db, prompt);
    tokio::pin!(connecting);
    let handle = loop {
        tokio::select! {
            result = &mut connecting => break result,
            cmd = cmds.recv() => match cmd {
                None | Some(Cmd::Close) => {
                    sink.emit(id, SessionEvent::State(SessionState::Disconnected { reason: String::new() }));
                    return;
                }
                Some(Cmd::Resize { width, height, scale: s }) => {
                    size = (width.max(200), height.max(200));
                    scale = s;
                }
                Some(_) => {}
            }
        }
    };
    let handle = match handle {
        Ok(h) => h,
        Err(message) => {
            tracing::warn!(session = id, %message, "SSH-Verbindung fehlgeschlagen");
            sink.emit(id, SessionEvent::State(SessionState::Error { message }));
            return;
        }
    };

    let Some(mut term) = TermView::new(size.0, size.1, font_px(req.terminal_font_size, scale)) else {
        sink.emit(
            id,
            SessionEvent::State(SessionState::Error {
                message: "Keine Terminalschrift gefunden (Cascadia Mono oder Consolas).".into(),
            }),
        );
        return;
    };

    let (cols, rows) = term.grid();
    let shell = async {
        let channel = handle.channel_open_session().await?;
        channel
            .request_pty(
                true,
                TERM_NAME,
                cols as u32,
                rows as u32,
                u32::from(size.0),
                u32::from(size.1),
                &[],
            )
            .await?;
        // Wird von vielen Servern verworfen (AcceptEnv), schadet aber nicht.
        let _ = channel.set_env(false, "COLORTERM", "truecolor").await;
        channel.request_shell(true).await?;
        Ok::<_, russh::Error>(channel)
    };
    let channel = match shell.await {
        Ok(ch) => ch,
        Err(e) => {
            sink.emit(
                id,
                SessionEvent::State(SessionState::Error {
                    message: format!("Shell konnte nicht gestartet werden: {e}"),
                }),
            );
            return;
        }
    };
    let (mut reader, writer) = channel.split();

    tracing::info!(session = id, cols, rows, "SSH-Sitzung aktiv");
    sink.emit(id, SessionEvent::State(SessionState::Connected));
    sink.emit(id, SessionEvent::LoggedOn);
    sink.emit(
        id,
        SessionEvent::Resolution {
            width: size.0,
            height: size.1,
        },
    );

    let mut presenter = Presenter::new();
    let mut was_visible = false;
    let mut ticker = tokio::time::interval(PRESENT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut exit_status: Option<u32> = None;
    let mut closed_by_server = false;
    let mut user_closed = false;

    macro_rules! send {
        ($bytes:expr) => {{
            let bytes: Vec<u8> = $bytes;
            if !bytes.is_empty() {
                if let Err(e) = writer.data(&bytes[..]).await {
                    tracing::debug!(session = id, error = %e, "Senden an die SSH-Sitzung fehlgeschlagen");
                }
            }
        }};
    }

    loop {
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let reply = term.feed(&data);
                    send!(reply);
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                Some(ChannelMsg::Close) => {
                    closed_by_server = true;
                    break;
                }
                Some(_) => {}
                None => break,
            },
            cmd = cmds.recv() => match cmd {
                None | Some(Cmd::Close) => {
                    user_closed = true;
                    break;
                }
                Some(Cmd::Input(event)) => {
                    let bytes = match event {
                        InputEvent::Char { ch, alt } => term.input_char(ch, alt),
                        InputEvent::Unicode { ch, pressed: true } => term.input_char(ch, false),
                        InputEvent::TermKey { key, mods } => term.input_key(key, mods),
                        InputEvent::MouseMove { x, y } => term.mouse_move(x, y, current_mods()),
                        InputEvent::MouseButton { button, pressed } => {
                            let outcome = term.mouse_button(button, pressed, current_mods());
                            if let Some(text) = outcome.copy {
                                sink.emit(id, SessionEvent::Clipboard(text));
                            }
                            if outcome.paste {
                                sink.emit(id, SessionEvent::PasteRequest);
                            }
                            outcome.bytes
                        }
                        InputEvent::Wheel { vertical: true, units } => term.wheel(units, current_mods()),
                        _ => Vec::new(),
                    };
                    send!(bytes);
                }
                Some(Cmd::Paste(text)) => send!(term.paste(&text)),
                Some(Cmd::Copy) => {
                    if let Some(text) = term.selection_text() {
                        sink.emit(id, SessionEvent::Clipboard(text));
                    }
                }
                Some(Cmd::Resize { width, height, scale }) => {
                    let (width, height) = (width.max(200), height.max(200));
                    if let Some((cols, rows)) = term.resize(width, height, font_px(req.terminal_font_size, scale)) {
                        tracing::debug!(session = id, cols, rows, "Terminalgröße geändert");
                        if let Err(e) = writer
                            .window_change(cols as u32, rows as u32, u32::from(width), u32::from(height))
                            .await
                        {
                            tracing::debug!(session = id, error = %e, "Größenänderung nicht übermittelt");
                        }
                    }
                    sink.emit(id, SessionEvent::Resolution { width, height });
                }
            },
            _ = ticker.tick() => {
                let now_visible = visible.load(Ordering::Acquire);
                if now_visible {
                    if !was_visible {
                        let (w, h) = term.size_px();
                        presenter.invalidate(w, h);
                    }
                    if let Some(rect) = term.render() {
                        presenter.mark_dirty(rect);
                    }
                    if presenter.has_pending()
                        && let Some(frame) = presenter.present(term.framebuffer())
                    {
                        sink.emit(id, SessionEvent::Frame(frame));
                    }
                }
                was_visible = now_visible;
            }
        }
    }

    if user_closed || closed_by_server {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            handle.disconnect(Disconnect::ByApplication, "", "de"),
        )
        .await;
    }
    let state = if user_closed {
        SessionState::Disconnected {
            reason: "vom Benutzer getrennt".into(),
        }
    } else if closed_by_server || exit_status.is_some() {
        SessionState::Disconnected {
            reason: match exit_status {
                Some(0) | None => "Sitzung beendet".into(),
                Some(code) => format!("Sitzung beendet (Exit-Code {code})"),
            },
        }
    } else {
        SessionState::Error {
            message: "Verbindung zum Server verloren.".into(),
        }
    };
    tracing::info!(session = id, ?exit_status, "SSH-Sitzung beendet");
    sink.emit(id, SessionEvent::State(state));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_messages() {
        assert!(friendly("x", "Connection refused (os error 10061)").starts_with("Verbindung abgelehnt"));
        assert!(friendly("x", "No such host is known. (os error 11001)").starts_with("Hostname"));
        assert_eq!(friendly("Stufe", "seltsam"), "Stufe: seltsam");
    }

    #[test]
    fn font_scales_with_dpi() {
        assert_eq!(font_px(14.0, 100), 14.0);
        assert_eq!(font_px(14.0, 150), 21.0);
    }
}
