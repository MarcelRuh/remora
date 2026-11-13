//! Lebenszyklus einer einzelnen IronRDP-Sitzung.
//!
//! Jede Sitzung läuft in einem eigenen OS-Thread mit eigener Tokio-Runtime. Der Thread
//! verarbeitet die Ausgabeereignisse von `ironrdp-client`, pflegt den Framebuffer und liefert
//! – gedrosselt und nur für sichtbare Sitzungen – Frames an die UI. Beim Beenden werden
//! Runtime, Netzwerkverbindung und alle Puffer freigegeben.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ironrdp::client::output_channel::OutputEventReceiver;
use ironrdp::client::rdp::{AutoReconnectDecision, RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent};
use ironrdp::input::{Database as InputDatabase, MousePosition, Operation, Scancode, WheelRotations};
use smallvec::SmallVec;

use super::input::{InputEvent, MouseButton};
use super::renderer::{CursorImage, Framebuffer, Presenter};
use super::{ConnectRequest, CursorUpdate, RdpSession, SessionEvent, SessionEventSink, SessionId, SessionState};

/// Frame-Takt (Obergrenze, ~60 Hz).
const PRESENT_INTERVAL: Duration = Duration::from_millis(16);

/// Zwischen UI und Sitzungs-Thread geteilter Zustand.
#[derive(Clone, Default)]
pub struct SessionContext {
    inner: Arc<ContextInner>,
}

#[derive(Default)]
struct ContextInner {
    visible: AtomicBool,
    /// UI verlangt beim nächsten Tick einen vollständigen Frame (z. B. Tab wieder sichtbar).
    force_full_frame: AtomicBool,
    prompt_open: AtomicBool,
}

impl SessionContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_prompt_open(&self, open: bool) {
        self.inner.prompt_open.store(open, Ordering::Release);
    }

    fn prompt_open(&self) -> bool {
        self.inner.prompt_open.load(Ordering::Acquire)
    }

    fn set_visible(&self, visible: bool) {
        let was = self.inner.visible.swap(visible, Ordering::AcqRel);
        if visible && !was {
            self.inner.force_full_frame.store(true, Ordering::Release);
        }
    }

    fn visible(&self) -> bool {
        self.inner.visible.load(Ordering::Acquire)
    }

    fn take_force_full_frame(&self) -> bool {
        self.inner.force_full_frame.swap(false, Ordering::AcqRel)
    }
}

/// Native Windows-Backends, die auf dem UI-Thread leben müssen.
#[cfg(windows)]
#[derive(Default)]
pub struct NativeBackends {
    clipboard: Option<ironrdp_cliprdr_native::WinClipboard>,
}

/// Hängt Zwischenablage und Geräteumleitung (Laufwerke/Drucker) an den Client.
/// Muss auf dem UI-Thread aufgerufen werden.
#[cfg(windows)]
pub fn attach_windows_backends(
    mut client: RdpClient,
    options: &crate::database::ConnectionOptions,
) -> anyhow::Result<(RdpClient, NativeBackends)> {
    use ironrdp_rdpdr_native::{
        LogicalDriveSelection, WindowsPrinterRedirection, WindowsPrinterSelection, WindowsRdpdrBackendFactory,
    };

    let mut native = NativeBackends::default();
    if options.clipboard {
        super::clipboard::ensure_ole_initialized()?;
        match ironrdp_cliprdr_native::WinClipboard::with_file_transfer(
            super::clipboard::ClipboardProxy::new(client.input_sender()),
            // Dateiübertragung über die Zwischenablage erfordert eigene Stream-Behandlung
            // durch den Einbettenden – derzeit nicht umgesetzt (siehe docs/LIMITATIONS.md).
            false,
        ) {
            Ok(cb) => {
                client = client.with_cliprdr_backend_factory(cb.backend_factory());
                native.clipboard = Some(cb);
            }
            Err(e) => tracing::warn!(error = %e, "Zwischenablage konnte nicht initialisiert werden"),
        }
    }

    if options.drives || options.printers {
        let mut factory = WindowsRdpdrBackendFactory::from_drives(Vec::new())
            .map_err(|e| anyhow::anyhow!("Laufwerksumleitung: {e}"))?;
        if options.drives {
            factory = factory
                .with_logical_drives(LogicalDriveSelection::from_letters(true, []))
                .map_err(|e| anyhow::anyhow!("Laufwerksumleitung: {e}"))?;
        }
        if options.printers {
            factory =
                factory.with_printer_redirection(WindowsPrinterRedirection::new(WindowsPrinterSelection::Default));
        }
        client = client.with_rdpdr_backend_factory(Box::new(factory));
    }
    Ok((client, native))
}

pub struct IronRdpSession {
    id: SessionId,
    input: RdpInputSender,
    keyboard: Mutex<InputDatabase>,
    ctx: SessionContext,
    thread: Option<JoinHandle<()>>,
    #[cfg(windows)]
    native: Option<NativeBackends>,
}

impl IronRdpSession {
    pub fn spawn(
        req: ConnectRequest,
        client: RdpClient,
        input: RdpInputSender,
        output: OutputEventReceiver,
        sink: Arc<dyn SessionEventSink>,
        ctx: SessionContext,
        #[cfg(windows)] native: NativeBackends,
    ) -> anyhow::Result<Self> {
        let id = req.session_id;
        let worker_ctx = ctx.clone();
        let worker_input = input.clone();
        let timeout = req.connect_timeout;
        let thread = std::thread::Builder::new()
            .name(format!("rdp-session-{id}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name(format!("rdp-{id}-io"))
                    .enable_all()
                    .build()
                {
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
                runtime.block_on(run_worker(id, client, worker_input, output, sink, worker_ctx, timeout));
                // Runtime wird hier verworfen → alle Tasks/Sockets werden geschlossen.
                runtime.shutdown_timeout(Duration::from_secs(2));
                tracing::debug!(session = id, "Sitzungs-Thread beendet");
            })?;

        Ok(Self {
            id,
            input,
            keyboard: Mutex::new(InputDatabase::new()),
            ctx,
            thread: Some(thread),
            #[cfg(windows)]
            native: Some(native),
        })
    }

    fn send_fast_path(&self, events: SmallVec<[ironrdp::pdu::input::fast_path::FastPathInputEvent; 2]>) {
        if events.is_empty() {
            return;
        }
        if let Err(e) = self.input.try_send(RdpInputEvent::FastPath(events)) {
            // Warteschlange voll (z. B. Netzwerk stockt) – Mausbewegungen dürfen verloren gehen.
            tracing::trace!(session = self.id, error = %e, "Eingabe verworfen");
        }
    }

    fn apply(&self, ops: impl IntoIterator<Item = Operation>) {
        let events = {
            let mut db = self.keyboard.lock().unwrap_or_else(|e| e.into_inner());
            db.apply(ops)
        };
        self.send_fast_path(events);
    }
}

fn map_button(b: MouseButton) -> ironrdp::input::MouseButton {
    match b {
        MouseButton::Left => ironrdp::input::MouseButton::Left,
        MouseButton::Right => ironrdp::input::MouseButton::Right,
        MouseButton::Middle => ironrdp::input::MouseButton::Middle,
        MouseButton::X1 => ironrdp::input::MouseButton::X1,
        MouseButton::X2 => ironrdp::input::MouseButton::X2,
    }
}

impl RdpSession for IronRdpSession {
    fn send_input(&self, event: InputEvent) {
        match event {
            InputEvent::Key {
                scancode,
                extended,
                pressed,
            } => {
                let sc = Scancode::from_u8(extended, scancode);
                self.apply([if pressed {
                    Operation::KeyPressed(sc)
                } else {
                    Operation::KeyReleased(sc)
                }]);
            }
            InputEvent::Unicode { ch, pressed } => self.apply([if pressed {
                Operation::UnicodeKeyPressed(ch)
            } else {
                Operation::UnicodeKeyReleased(ch)
            }]),
            InputEvent::MouseMove { x, y } => self.apply([Operation::MouseMove(MousePosition { x, y })]),
            InputEvent::MouseButton { button, pressed } => {
                let b = map_button(button);
                self.apply([if pressed {
                    Operation::MouseButtonPressed(b)
                } else {
                    Operation::MouseButtonReleased(b)
                }]);
            }
            InputEvent::Wheel { vertical, units } => self.apply([Operation::WheelRotations(WheelRotations {
                is_vertical: vertical,
                rotation_units: units,
            })]),
            InputEvent::ReleaseAll => {
                let events = {
                    let mut db = self.keyboard.lock().unwrap_or_else(|e| e.into_inner());
                    db.release_all()
                };
                self.send_fast_path(events);
            }
            InputEvent::SyncLocks { caps, num, scroll } => {
                let ev = ironrdp::input::synchronize_event(scroll, num, caps, false);
                self.send_fast_path(SmallVec::from_iter([ev]));
            }
            InputEvent::CtrlAltDel => {
                let ctrl = Scancode::from_u8(false, 0x1D);
                let alt = Scancode::from_u8(false, 0x38);
                let del = Scancode::from_u8(true, 0x53);
                self.apply([
                    Operation::KeyPressed(ctrl),
                    Operation::KeyPressed(alt),
                    Operation::KeyPressed(del),
                    Operation::KeyReleased(del),
                    Operation::KeyReleased(alt),
                    Operation::KeyReleased(ctrl),
                ]);
            }
        }
    }

    fn resize(&self, width: u16, height: u16, scale_factor: u32) {
        // RDP verlangt gerade Breiten für dynamische Größenänderungen.
        let width = (width.max(200)) & !1;
        let height = height.max(200);
        let _ = self.input.try_send(RdpInputEvent::Resize {
            width,
            height,
            scale_factor,
            physical_size: None,
        });
    }

    fn set_visible(&self, visible: bool) {
        self.ctx.set_visible(visible);
    }

    fn disconnect(&self) {
        self.input.request_graceful_close();
        // Falls der Server nicht reagiert, nach kurzer Zeit hart abbrechen.
        let input = self.input.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            input.request_close();
        });
    }
}

impl Drop for IronRdpSession {
    fn drop(&mut self) {
        self.input.request_close();
        // Native Backends (Fenster der Zwischenablage) auf dem UI-Thread freigeben.
        #[cfg(windows)]
        drop(self.native.take());
        // Thread nicht joinen: Der UI-Thread darf nicht blockieren. Der Thread beendet sich nach
        // dem Abbruch selbstständig und gibt alle Ressourcen frei.
        if let Some(t) = self.thread.take() {
            drop(t);
        }
    }
}

/// Übersetzt Fehlertexte in verständliche Meldungen.
pub fn friendly_error(raw: &str) -> String {
    let raw = strip_locations(raw);
    let raw = raw.as_str();
    let lower = raw.to_ascii_lowercase();
    let hint = if lower.contains("requires enhanced rdp security with credssp") {
        Some("Der Server verlangt Network Level Authentication (NLA) – in der Verbindung unter „Erweitert“ aktivieren.")
    } else if lower.contains("requires") && lower.contains("negotiation failure") {
        Some("Die Sicherheitsaushandlung mit dem Server ist fehlgeschlagen.")
    } else if lower.contains("logon")
        || lower.contains("0x8009030c")
        || lower.contains("credssp")
        || lower.contains("authentication")
    {
        Some("Anmeldung fehlgeschlagen – Benutzername oder Passwort prüfen.")
    } else if lower.contains("timed out") || lower.contains("timeout") {
        Some("Zeitüberschreitung – Server nicht erreichbar.")
    } else if lower.contains("connection refused") || lower.contains("10061") {
        Some("Verbindung abgelehnt – läuft der RDP-Dienst und ist der Port korrekt?")
    } else if lower.contains("no such host") || lower.contains("11001") || lower.contains("dns") {
        Some("Hostname konnte nicht aufgelöst werden.")
    } else if lower.contains("certificate") || lower.contains("invalid peer") || lower.contains("handshake") {
        Some("Das Serverzertifikat wurde nicht akzeptiert.")
    } else if lower.contains("network") || lower.contains("10065") || lower.contains("10051") {
        Some("Netzwerk nicht erreichbar.")
    } else {
        None
    };
    match hint {
        Some(h) => format!("{h} ({raw})"),
        None => raw.to_owned(),
    }
}

/// Fehlermeldung inklusive aller Ursachen (`source()`-Kette), ohne Wiederholungen.
fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = vec![error.to_string()];
    let mut source = error.source();
    while let Some(e) = source {
        let text = e.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = e.source();
    }
    parts.join(": ")
}

/// Entfernt Quelltext-Positionen wie `[negotiation failure @ C:\…\connection.rs:1546] `.
fn strip_locations(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('[') {
        let Some(len) = rest[start..].find(']') else { break };
        let inner = &rest[start..start + len];
        out.push_str(&rest[..start]);
        if !inner.contains(" @ ") {
            out.push_str(&rest[start..=start + len]);
        }
        rest = rest[start + len + 1..].trim_start();
    }
    out.push_str(rest);
    out.trim().to_owned()
}

fn disconnect_reason(reason: &ironrdp::session::GracefulDisconnectReason) -> String {
    use ironrdp::session::GracefulDisconnectReason as R;
    match reason {
        R::UserInitiated => "vom Benutzer getrennt".to_owned(),
        R::ServerInitiated => "vom Server getrennt".to_owned(),
        R::Other(s) => s.clone(),
    }
}

async fn run_worker(
    id: SessionId,
    client: RdpClient,
    input: RdpInputSender,
    mut output: OutputEventReceiver,
    sink: Arc<dyn SessionEventSink>,
    ctx: SessionContext,
    connect_timeout: Duration,
) {
    sink.emit(id, SessionEvent::State(SessionState::Connecting));
    // `RdpClient::run` ist nicht `Send` (native Backends) – daher im selben Task pollen.
    let client_fut = client.run();
    tokio::pin!(client_fut);
    let mut client_done = false;

    let mut fb = Framebuffer::new(0, 0);
    let mut presenter = Presenter::new();
    let mut connected = false;
    // Nach jedem (Neu-)Aufbau die Größe des ersten Bildes melden, auch wenn sie gleich blieb
    let mut announce_size = false;
    let mut connect_started = Instant::now();
    let mut final_state: Option<SessionState> = None;
    let mut ticker = tokio::time::interval(PRESENT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = &mut client_fut, if !client_done => {
                client_done = true;
            }
            event = output.recv() => {
                let Some(event) = event else { break };
                let done = handle_event(id, event, &input, &sink, &mut fb, &mut presenter, &mut connected, &mut announce_size, &mut final_state);
                if done {
                    break;
                }
            }
            _ = ticker.tick() => {
                if !connected {
                    if ctx.prompt_open() {
                        connect_started = Instant::now();
                    } else if connect_started.elapsed() > connect_timeout {
                        tracing::warn!(session = id, "Zeitüberschreitung beim Verbindungsaufbau");
                        final_state = Some(SessionState::Error {
                            message: format!(
                                "Zeitüberschreitung beim Verbindungsaufbau ({} s).",
                                connect_timeout.as_secs()
                            ),
                        });
                        input.request_close();
                        connected = true; // Timeout nur einmal auslösen
                    }
                }
                if ctx.take_force_full_frame() && fb.width() > 0 {
                    presenter.invalidate(fb.width(), fb.height());
                }
                if ctx.visible() && presenter.has_pending()
                    && let Some(frame) = presenter.present(&fb)
                {
                    sink.emit(id, SessionEvent::Frame(frame));
                }
            }
        }
    }

    if !client_done {
        client_fut.await;
    }
    let state = final_state.unwrap_or(SessionState::Disconnected { reason: String::new() });
    sink.emit(id, SessionEvent::State(state));
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    id: SessionId,
    event: RdpOutputEvent,
    input: &RdpInputSender,
    sink: &Arc<dyn SessionEventSink>,
    fb: &mut Framebuffer,
    presenter: &mut Presenter,
    connected: &mut bool,
    announce_size: &mut bool,
    final_state: &mut Option<SessionState>,
) -> bool {
    match event {
        RdpOutputEvent::Connected => {
            *connected = true;
            *announce_size = true;
            tracing::info!(session = id, "RDP-Sitzung aktiv");
            sink.emit(id, SessionEvent::State(SessionState::Connected));
        }
        RdpOutputEvent::DesktopUpdate(update) => {
            let (buffer, width, height, region) = update.into_parts();
            let resized = fb.ensure_size(width.get(), height.get());
            if resized {
                presenter.invalidate(width.get(), height.get());
            }
            if std::mem::take(announce_size) | resized {
                sink.emit(
                    id,
                    SessionEvent::Resolution {
                        width: width.get(),
                        height: height.get(),
                    },
                );
            }
            if let Some(rect) = fb.apply_region(&buffer, region.left, region.top, region.right, region.bottom) {
                presenter.mark_dirty(rect);
            }
        }
        RdpOutputEvent::Image { buffer, width, height } => {
            let size_changed = fb.width() != width.get() || fb.height() != height.get();
            if fb.replace(buffer, width.get(), height.get()) {
                presenter.invalidate(width.get(), height.get());
                if std::mem::take(announce_size) | size_changed {
                    sink.emit(
                        id,
                        SessionEvent::Resolution {
                            width: width.get(),
                            height: height.get(),
                        },
                    );
                }
            }
        }
        RdpOutputEvent::PointerDefault => sink.emit(id, SessionEvent::Cursor(CursorUpdate::Default)),
        RdpOutputEvent::PointerHidden => sink.emit(id, SessionEvent::Cursor(CursorUpdate::Hidden)),
        RdpOutputEvent::PointerBitmap(pointer) => {
            if let Some(img) = CursorImage::from_rgba(
                pointer.width,
                pointer.height,
                pointer.hotspot_x,
                pointer.hotspot_y,
                &pointer.bitmap_data,
            ) {
                sink.emit(id, SessionEvent::Cursor(CursorUpdate::Bitmap(img)));
            }
        }
        RdpOutputEvent::LoginComplete => sink.emit(id, SessionEvent::LoggedOn),
        RdpOutputEvent::AutoReconnecting {
            attempt,
            maximum_attempts,
            response,
            ..
        } => {
            tracing::warn!(
                session = id,
                attempt,
                "Verbindung unterbrochen, automatische Wiederverbindung"
            );
            sink.emit(
                id,
                SessionEvent::State(SessionState::Reconnecting {
                    attempt,
                    max: maximum_attempts,
                }),
            );
            let _ = response.send(AutoReconnectDecision::Continue);
        }
        RdpOutputEvent::AutoReconnected => {
            tracing::info!(session = id, "Wiederverbindung erfolgreich");
            sink.emit(id, SessionEvent::State(SessionState::Connected));
        }
        RdpOutputEvent::ServerRedirected { target } => {
            tracing::info!(session = id, %target, "Server-Umleitung");
        }
        RdpOutputEvent::DisplayResizeFallback(reason) => {
            tracing::info!(session = id, ?reason, "Dynamische Auflösung erfordert Neuverbindung");
        }
        RdpOutputEvent::LogonError(info) => {
            tracing::warn!(session = id, ?info, "Server meldet Anmeldefehler");
        }
        RdpOutputEvent::ConnectionFailure(error) => {
            let raw = error_chain(&error);
            tracing::error!(session = id, error = %raw, "Verbindungsaufbau fehlgeschlagen");
            if final_state.is_none() {
                *final_state = Some(SessionState::Error {
                    message: friendly_error(&raw),
                });
            }
            input.request_close();
        }
        RdpOutputEvent::Terminated(result) => {
            let state = match result {
                Ok(reason) => SessionState::Disconnected {
                    reason: disconnect_reason(&reason),
                },
                Err(error) => {
                    let raw = error_chain(&error);
                    tracing::error!(session = id, error = %raw, "Sitzung mit Fehler beendet");
                    SessionState::Error {
                        message: friendly_error(&raw),
                    }
                }
            };
            if final_state.is_none() {
                *final_state = Some(state);
            }
            return true;
        }
        other => {
            tracing::trace!(session = id, event = ?std::mem::discriminant(&other), "Nicht behandeltes Ereignis");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_errors() {
        let nla = friendly_error(
            r"[negotiation failure @ C:/x/connection.rs:1546] negotiation failure: server requires Enhanced RDP Security with CredSSP",
        );
        assert!(
            nla.starts_with("Der Server verlangt Network Level Authentication"),
            "{nla}"
        );
        assert!(!nla.contains("connection.rs"));
        assert!(friendly_error("CredSSP: SEC_E_LOGON_DENIED").starts_with("Anmeldung fehlgeschlagen"));
        assert!(friendly_error("connect: Connection refused (os error 10061)").starts_with("Verbindung abgelehnt"));
        assert_eq!(friendly_error("something odd"), "something odd");
    }

    #[test]
    fn context_visibility_requests_full_frame() {
        let ctx = SessionContext::new();
        assert!(!ctx.take_force_full_frame());
        ctx.set_visible(true);
        assert!(ctx.take_force_full_frame());
        assert!(!ctx.take_force_full_frame());
        ctx.set_visible(true);
        assert!(!ctx.take_force_full_frame());
    }
}
