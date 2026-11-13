//! Verwaltung paralleler Sitzungen: Tabs, Split-Bereiche, Zustände und Wiederverbindung.
//!
//! Läuft ausschließlich im UI-Thread. Die eigentliche Arbeit jeder Sitzung findet in ihrem
//! eigenen Thread statt (siehe [`crate::rdp::session`]).

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::database::{Connection, ConnectionId, ScaleMode};
use crate::rdp::input::WheelAccumulator;
use crate::rdp::{ConnectRequest, RdpBackend, RdpSession, SessionEventSink, SessionId, SessionState};
use crate::security::Secret;

/// Eine geöffnete Sitzung (= ein Tab).
pub struct SessionEntry {
    pub id: SessionId,
    pub connection: Connection,
    pub state: SessionState,
    pub handle: Option<Rc<dyn RdpSession>>,
    pub connected_at: Option<Instant>,
    pub remote_size: (u16, u16),
    pub scale_mode: ScaleMode,
    pub wheel: WheelAccumulator,
    pub ever_connected: bool,
    /// Anwendungsseitige Wiederverbindungsversuche seit dem letzten erfolgreichen Aufbau.
    pub app_reconnects: u32,
    pub user_disconnected: bool,
    /// Zugangsdaten für Wiederverbindungen; werden beim Schließen genullt.
    pub password: Option<Secret>,
    pub gateway_password: Option<Secret>,
    pub logged_on: bool,
    /// Kennung des aktuellen Verbindungsversuchs (Ereignisse älterer Versuche werden verworfen).
    pub attempt: u64,
    /// Abgleich zwischen angeforderter und vom Server gemeldeter Desktopgröße.
    pub size: SizeTracker,
}

impl SessionEntry {
    pub fn is_connected(&self) -> bool {
        self.state == SessionState::Connected
    }
}

/// Abweichung in Pixeln, unterhalb derer keine neue Größe angefordert wird.
const RESIZE_TOLERANCE: u16 = 8;
/// Spätestens dann gilt eine unbeantwortete Größenanfrage als erledigt.
pub const SIZE_REQUEST_EXPIRY: Duration = Duration::from_secs(10);
/// Wie oft dieselbe Größe angefordert wird, wenn der Server danach eine andere meldet.
const SIZE_REQUEST_ATTEMPTS: u8 = 2;

fn near(a: (u16, u16), b: (u16, u16)) -> bool {
    a.0.abs_diff(b.0) <= RESIZE_TOLERANCE && a.1.abs_diff(b.1) <= RESIZE_TOLERANCE
}

/// Entscheidet, wann eine Desktopgröße (dynamische Auflösung) angefordert wird.
///
/// Nach einer Anfrage – auch beim Verbindungsaufbau oder bei einer Neuverbindung, die IronRDP
/// statt einer Größenänderung durchführt – kommt das erste Bild in der neuen Größe oft erst
/// Sekunden später. Bis dahin ist die zuletzt empfangene Größe veraltet; eine erneute Anfrage
/// derselben Größe ignoriert der Server, und IronRDP verbindet nach Ablauf seiner Wartezeit
/// ein weiteres Mal neu. Maßgeblich ist daher bis zur Antwort die angeforderte Größe, danach die
/// gemeldete. Mehrere Anfragen kurz hintereinander arbeitet IronRDP nacheinander ab; eine Antwort
/// in der Größe einer früheren Anfrage ist ein Zwischenstand, keine Ablehnung. Lehnt der Server
/// eine Größe wiederholt ab, wird sie nicht weiter angefordert.
#[derive(Debug, Default)]
pub struct SizeTracker {
    /// Offene Anfragen in Reihenfolge, die letzte ist das aktuelle Ziel.
    outstanding: Vec<(u16, u16)>,
    last_request: Option<Instant>,
    failed: Option<((u16, u16), u8)>,
}

impl SizeTracker {
    /// Neuer Verbindungsaufbau mit `size`: frühere Anfragen sind hinfällig.
    pub fn connecting(&mut self, size: (u16, u16), now: Instant) {
        self.outstanding.clear();
        self.failed = None;
        self.requested(size, now);
    }

    /// Größe wurde per dynamischer Auflösung angefordert.
    pub fn requested(&mut self, size: (u16, u16), now: Instant) {
        // Begrenzen: IronRDP fasst ohnehin alle bis auf die laufende und die letzte zusammen.
        if self.outstanding.len() >= 4 {
            self.outstanding.remove(0);
        }
        self.outstanding.push(size);
        self.last_request = Some(now);
    }

    /// Ob noch eine Antwort des Servers aussteht.
    pub fn awaiting(&self, now: Instant) -> bool {
        !self.outstanding.is_empty()
            && self
                .last_request
                .is_some_and(|at| now.duration_since(at) < SIZE_REQUEST_EXPIRY)
    }

    /// Server meldet seine tatsächliche Größe (erstes Bild nach Aufbau oder Größenänderung).
    pub fn confirmed(&mut self, remote: (u16, u16)) {
        let Some(&latest) = self.outstanding.last() else { return };
        match self.outstanding.iter().position(|&s| near(s, remote)) {
            Some(i) if i + 1 == self.outstanding.len() => {
                self.outstanding.clear();
                self.failed = None;
            }
            // Zwischenstand: spätere Anfragen laufen noch
            Some(i) => {
                self.outstanding.drain(..=i);
            }
            None => {
                self.outstanding.clear();
                self.failed = match self.failed {
                    Some((size, n)) if near(size, latest) => Some((size, n.saturating_add(1))),
                    _ => Some((latest, 1)),
                };
            }
        }
    }

    /// Ob für die Bereichsgröße `target` eine neue Größe angefordert werden soll.
    pub fn wants(&self, target: (u16, u16), remote: (u16, u16), now: Instant) -> bool {
        if self.awaiting(now)
            && let Some(&latest) = self.outstanding.last()
        {
            return !near(latest, target);
        }
        if near(remote, target) {
            return false;
        }
        !matches!(self.failed, Some((size, n)) if near(size, target) && n >= SIZE_REQUEST_ATTEMPTS)
    }
}

/// Aufteilung der Arbeitsfläche.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    #[default]
    Single,
    Columns,
    Rows,
    Grid,
}

impl LayoutMode {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => LayoutMode::Columns,
            2 => LayoutMode::Rows,
            3 => LayoutMode::Grid,
            _ => LayoutMode::Single,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            LayoutMode::Single => 0,
            LayoutMode::Columns => 1,
            LayoutMode::Rows => 2,
            LayoutMode::Grid => 3,
        }
    }

    pub fn pane_count(self) -> usize {
        match self {
            LayoutMode::Single => 1,
            LayoutMode::Columns | LayoutMode::Rows => 2,
            LayoutMode::Grid => 4,
        }
    }
}

pub struct SessionManager {
    backend: Box<dyn RdpBackend>,
    sink: Arc<dyn SessionEventSink>,
    sessions: Vec<SessionEntry>,
    next_id: SessionId,
    next_attempt: u64,
    pub layout: LayoutMode,
    /// Sitzung je Bereich (Split-Ansicht).
    pub panes: Vec<Option<SessionId>>,
    pub focused_pane: usize,
    /// Aktiver Tab (Index in `sessions`), `None` = Dashboard.
    pub active: Option<usize>,
}

impl SessionManager {
    pub fn new(backend: Box<dyn RdpBackend>, sink: Arc<dyn SessionEventSink>) -> Self {
        Self {
            backend,
            sink,
            sessions: Vec::new(),
            next_id: 1,
            next_attempt: 1_000_000,
            layout: LayoutMode::Single,
            panes: vec![None],
            focused_pane: 0,
            active: None,
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    pub fn sessions(&self) -> &[SessionEntry] {
        &self.sessions
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Ordnet einen Verbindungsversuch seiner Sitzung zu; `None` für veraltete Versuche.
    pub fn resolve_attempt(&self, attempt: u64) -> Option<SessionId> {
        self.sessions.iter().find(|s| s.attempt == attempt).map(|s| s.id)
    }

    pub fn index_of(&self, id: SessionId) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    pub fn get(&self, id: SessionId) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut SessionEntry> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn active_session(&self) -> Option<&SessionEntry> {
        self.active.and_then(|i| self.sessions.get(i))
    }

    /// Sitzung im fokussierten Bereich (bzw. aktiver Tab in der Einzelansicht).
    pub fn focused_session_id(&self) -> Option<SessionId> {
        self.panes.get(self.focused_pane).copied().flatten()
    }

    pub fn find_by_connection(&self, connection: ConnectionId) -> Option<SessionId> {
        self.sessions
            .iter()
            .find(|s| s.connection.id == connection && connection != 0)
            .map(|s| s.id)
    }

    pub fn active_connection_ids(&self) -> Vec<(ConnectionId, i32)> {
        self.sessions
            .iter()
            .map(|s| (s.connection.id, s.state.code()))
            .collect()
    }

    /// Öffnet eine neue Sitzung als Tab und startet den Verbindungsaufbau.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        connection: Connection,
        password: Option<Secret>,
        gateway_password: Option<Secret>,
        desktop: (u16, u16),
        scale_factor: u32,
        connect_timeout: std::time::Duration,
        auto_reconnect_attempts: u32,
    ) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;
        let scale_mode = connection.options.scale_mode;
        let mut entry = SessionEntry {
            id,
            connection,
            state: SessionState::Connecting,
            handle: None,
            connected_at: None,
            remote_size: (0, 0),
            scale_mode,
            wheel: WheelAccumulator::default(),
            ever_connected: false,
            app_reconnects: 0,
            user_disconnected: false,
            password,
            gateway_password,
            logged_on: false,
            attempt: 0,
            size: SizeTracker::default(),
        };
        self.start(
            &mut entry,
            desktop,
            scale_factor,
            connect_timeout,
            auto_reconnect_attempts,
        );
        self.sessions.push(entry);
        let index = self.sessions.len() - 1;
        self.activate(Some(index));
        id
    }

    fn start(
        &mut self,
        entry: &mut SessionEntry,
        desktop: (u16, u16),
        scale_factor: u32,
        connect_timeout: std::time::Duration,
        auto_reconnect_attempts: u32,
    ) {
        let (w, h) = match entry.connection.options.resolution {
            crate::database::Resolution::Fixed { width, height } => (width, height),
            crate::database::Resolution::FitToWindow => desktop,
        };
        let (w, h) = (w.max(200), h.max(200));
        entry.size.connecting((w, h), Instant::now());
        tracing::info!(session = entry.id, w, h, scale_factor, "Verbindungsaufbau");
        self.next_attempt += 1;
        entry.attempt = self.next_attempt;
        let request = ConnectRequest {
            session_id: entry.attempt,
            connection: entry.connection.clone(),
            password: entry.password.clone(),
            gateway_password: entry.gateway_password.clone(),
            desktop_width: w,
            desktop_height: h,
            scale_factor,
            connect_timeout,
            auto_reconnect_attempts,
        };
        entry.state = SessionState::Connecting;
        entry.logged_on = false;
        match self.backend.connect(request, self.sink.clone()) {
            Ok(handle) => entry.handle = Some(Rc::from(handle)),
            Err(e) => {
                tracing::error!(session = entry.id, error = %e, "Sitzung konnte nicht gestartet werden");
                entry.state = SessionState::Error {
                    message: format!("{e:#}"),
                };
                entry.handle = None;
            }
        }
    }

    /// Baut eine Sitzung (nach Trennung/Fehler oder auf Wunsch) neu auf.
    pub fn reconnect(
        &mut self,
        id: SessionId,
        desktop: (u16, u16),
        scale_factor: u32,
        connect_timeout: std::time::Duration,
        auto_reconnect_attempts: u32,
    ) -> bool {
        let Some(index) = self.index_of(id) else { return false };
        let mut entry = self.sessions.remove(index);
        if let Some(old) = entry.handle.take() {
            old.disconnect();
        }
        entry.user_disconnected = false;
        entry.connected_at = None;
        entry.remote_size = (0, 0);
        self.start(
            &mut entry,
            desktop,
            scale_factor,
            connect_timeout,
            auto_reconnect_attempts,
        );
        self.sessions.insert(index, entry);
        true
    }

    pub fn disconnect(&mut self, id: SessionId) {
        if let Some(s) = self.get_mut(id) {
            s.user_disconnected = true;
            if let Some(h) = &s.handle {
                h.disconnect();
            }
        }
    }

    /// Schließt einen Tab und gibt alle Ressourcen der Sitzung frei.
    pub fn close(&mut self, index: usize) -> Option<SessionEntry> {
        if index >= self.sessions.len() {
            return None;
        }
        let mut entry = self.sessions.remove(index);
        entry.user_disconnected = true;
        if let Some(h) = entry.handle.take() {
            h.set_visible(false);
            h.disconnect();
            // Drop → request_close; Thread räumt selbstständig auf.
        }
        entry.password = None;
        entry.gateway_password = None;
        for slot in &mut self.panes {
            if *slot == Some(entry.id) {
                *slot = None;
            }
        }
        // Aktiven Tab anpassen
        self.active = match self.active {
            None => None,
            Some(_) if self.sessions.is_empty() => None,
            Some(a) if a > index => Some(a - 1),
            Some(a) if a == index => Some(index.min(self.sessions.len() - 1)),
            Some(a) => Some(a),
        };
        if self.layout == LayoutMode::Single {
            self.panes[0] = self.active.map(|i| self.sessions[i].id);
        }
        self.sync_visibility();
        Some(entry)
    }

    /// Aktiviert einen Tab (`None` = Dashboard).
    pub fn activate(&mut self, index: Option<usize>) {
        let index = index.filter(|i| *i < self.sessions.len());
        self.active = index;
        if let Some(i) = index {
            let id = self.sessions[i].id;
            if self.layout == LayoutMode::Single {
                self.panes[0] = Some(id);
                self.focused_pane = 0;
            } else if let Some(pos) = self.panes.iter().position(|p| *p == Some(id)) {
                self.focused_pane = pos;
            } else {
                // In den fokussierten (bzw. ersten freien) Bereich legen
                let target = self.panes.iter().position(Option::is_none).unwrap_or(self.focused_pane);
                self.panes[target] = Some(id);
                self.focused_pane = target;
            }
        } else if self.layout == LayoutMode::Single {
            self.panes[0] = None;
        }
        self.sync_visibility();
    }

    pub fn cycle(&mut self, delta: i32) {
        if self.sessions.is_empty() {
            return;
        }
        let n = self.sessions.len() as i32;
        let current = self.active.map_or(if delta > 0 { -1 } else { 0 }, |i| i as i32);
        let next = (current + delta).rem_euclid(n) as usize;
        self.activate(Some(next));
    }

    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.sessions.len() || to >= self.sessions.len() || from == to {
            return;
        }
        let active_id = self.active.map(|i| self.sessions[i].id);
        let entry = self.sessions.remove(from);
        self.sessions.insert(to, entry);
        self.active = active_id.and_then(|id| self.index_of(id));
    }

    pub fn set_layout(&mut self, mode: LayoutMode) {
        if mode == self.layout {
            return;
        }
        let count = mode.pane_count();
        let mut new_panes: Vec<Option<SessionId>> = Vec::with_capacity(count);
        // Aktive Sitzung zuerst, dann bisher sichtbare, dann weitere Tabs auffüllen
        let mut order: Vec<SessionId> = Vec::new();
        if let Some(id) = self.active.map(|i| self.sessions[i].id) {
            order.push(id);
        }
        for id in self.panes.iter().flatten() {
            if !order.contains(id) {
                order.push(*id);
            }
        }
        for s in &self.sessions {
            if !order.contains(&s.id) {
                order.push(s.id);
            }
        }
        for i in 0..count {
            new_panes.push(order.get(i).copied());
        }
        self.layout = mode;
        self.panes = new_panes;
        self.focused_pane = 0;
        self.sync_visibility();
    }

    pub fn assign_pane(&mut self, pane: usize, session: Option<SessionId>) {
        if pane >= self.panes.len() {
            return;
        }
        if let Some(id) = session {
            // Eine Sitzung erscheint höchstens in einem Bereich
            for slot in &mut self.panes {
                if *slot == Some(id) {
                    *slot = None;
                }
            }
            self.active = self.index_of(id);
        }
        self.panes[pane] = session;
        self.focused_pane = pane;
        self.sync_visibility();
    }

    pub fn maximize_pane(&mut self, pane: usize) {
        if let Some(Some(id)) = self.panes.get(pane).copied() {
            self.layout = LayoutMode::Single;
            self.panes = vec![Some(id)];
            self.focused_pane = 0;
            self.active = self.index_of(id);
            self.sync_visibility();
        }
    }

    pub fn focus_pane(&mut self, pane: usize) {
        if pane < self.panes.len() {
            self.focused_pane = pane;
            if let Some(Some(id)) = self.panes.get(pane) {
                self.active = self.index_of(*id);
            }
        }
    }

    /// Nur sichtbare Sitzungen erhalten Frames.
    pub fn sync_visibility(&self) {
        for s in &self.sessions {
            if let Some(h) = &s.handle {
                h.set_visible(self.panes.contains(&Some(s.id)));
            }
        }
    }

    pub fn pane_of(&self, id: SessionId) -> Option<usize> {
        self.panes.iter().position(|p| *p == Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdp::{InputEvent, SessionEvent};
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeSession {
        visible: RefCell<bool>,
    }
    impl RdpSession for FakeSession {
        fn send_input(&self, _event: InputEvent) {}
        fn resize(&self, _w: u16, _h: u16, _s: u32) {}
        fn set_visible(&self, v: bool) {
            *self.visible.borrow_mut() = v;
        }
        fn disconnect(&self) {}
    }
    struct FakeBackend;
    impl RdpBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn connect(&self, _r: ConnectRequest, _s: Arc<dyn SessionEventSink>) -> anyhow::Result<Box<dyn RdpSession>> {
            Ok(Box::new(FakeSession::default()))
        }
    }
    struct NullSink;
    impl SessionEventSink for NullSink {
        fn emit(&self, _s: SessionId, _e: SessionEvent) {}
    }

    fn manager() -> SessionManager {
        SessionManager::new(Box::new(FakeBackend), Arc::new(NullSink))
    }

    fn open(m: &mut SessionManager, name: &str) -> SessionId {
        let mut c = Connection::new(name, "host");
        c.id = m.len() as i64 + 1;
        m.open(c, None, None, (1280, 720), 100, std::time::Duration::from_secs(5), 0)
    }

    #[test]
    fn open_close_and_activate() {
        let mut m = manager();
        let a = open(&mut m, "a");
        let b = open(&mut m, "b");
        assert_eq!(m.active, Some(1));
        assert_eq!(m.panes, vec![Some(b)]);
        m.cycle(1);
        assert_eq!(m.active, Some(0));
        assert_eq!(m.panes, vec![Some(a)]);
        m.close(0);
        assert_eq!(m.active, Some(0));
        assert_eq!(m.panes, vec![Some(b)]);
        m.close(0);
        assert_eq!(m.active, None);
        assert_eq!(m.panes, vec![None]);
    }

    #[test]
    fn split_layout_fills_panes_and_maximize() {
        let mut m = manager();
        let a = open(&mut m, "a");
        let b = open(&mut m, "b");
        let c = open(&mut m, "c");
        m.set_layout(LayoutMode::Grid);
        assert_eq!(m.panes.len(), 4);
        assert_eq!(m.panes[0], Some(c));
        assert!(m.panes.contains(&Some(a)) && m.panes.contains(&Some(b)));
        assert_eq!(m.panes[3], None);
        // Zuweisen verschiebt statt zu duplizieren
        m.assign_pane(3, Some(a));
        assert_eq!(m.panes.iter().filter(|p| **p == Some(a)).count(), 1);
        assert_eq!(m.focused_pane, 3);
        m.maximize_pane(3);
        assert_eq!(m.layout, LayoutMode::Single);
        assert_eq!(m.panes, vec![Some(a)]);
    }

    #[test]
    fn move_tab_keeps_active_session() {
        let mut m = manager();
        let a = open(&mut m, "a");
        let _b = open(&mut m, "b");
        let _c = open(&mut m, "c");
        m.activate(Some(0));
        m.move_tab(0, 2);
        assert_eq!(m.sessions()[2].id, a);
        assert_eq!(m.active, Some(2));
    }

    #[test]
    fn closing_session_clears_secrets_and_panes() {
        let mut m = manager();
        let mut c = Connection::new("x", "h");
        c.id = 9;
        let id = m.open(
            c,
            Some(Secret::new("pw")),
            None,
            (800, 600),
            100,
            std::time::Duration::from_secs(5),
            0,
        );
        m.set_layout(LayoutMode::Columns);
        assert!(m.pane_of(id).is_some());
        let closed = m.close(0).unwrap();
        assert!(closed.password.is_none());
        assert!(m.pane_of(id).is_none());
    }

    #[test]
    fn no_second_request_while_reconnected_session_still_shows_old_size() {
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        s.requested((1104, 788), t0);
        // Altes Bild (1024×768) noch sichtbar, Bereich unverändert: keine erneute Anfrage
        assert!(!s.wants((1104, 788), (1024, 768), t0 + Duration::from_millis(450)));
        // Neuer Bereich während der Anfrage: weiterleiten (IronRDP stellt sie zurück)
        assert!(s.wants((904, 688), (1024, 768), t0 + Duration::from_millis(450)));
    }

    #[test]
    fn confirmed_size_is_the_truth() {
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        s.requested((804, 648), t0);
        // Server landet woanders (z. B. Anfrage unterwegs verloren): einmal erneut anfordern
        s.confirmed((904, 688));
        assert!(s.wants((804, 648), (904, 688), t0));
        assert!(!s.wants((904, 688), (904, 688), t0));
    }

    #[test]
    fn size_refused_twice_is_not_requested_again() {
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        for _ in 0..SIZE_REQUEST_ATTEMPTS {
            assert!(s.wants((1104, 788), (1024, 768), t0));
            s.requested((1104, 788), t0);
            s.confirmed((1024, 768));
        }
        assert!(!s.wants((1104, 788), (1024, 768), t0));
        // Eine andere Bereichsgröße wird wieder angefordert
        assert!(s.wants((904, 688), (1024, 768), t0));
    }

    #[test]
    fn success_resets_refusals() {
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        s.requested((1104, 788), t0);
        s.confirmed((1024, 768));
        s.requested((1104, 788), t0);
        s.confirmed((1104, 788));
        s.requested((1104, 788), t0);
        s.confirmed((1024, 768));
        assert!(s.wants((1104, 788), (1024, 768), t0));
    }

    #[test]
    fn unanswered_request_expires() {
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        s.requested((1104, 788), t0);
        assert!(s.wants((1104, 788), (1024, 768), t0 + SIZE_REQUEST_EXPIRY));
    }

    #[test]
    fn start_requests_connect_size() {
        let mut m = manager();
        let id = m.open(
            Connection::new("A", "a"),
            None,
            None,
            (1104, 788),
            100,
            std::time::Duration::from_secs(5),
            0,
        );
        let s = &m.get(id).unwrap().size;
        assert!(!s.wants((1104, 788), (0, 0), Instant::now()));
    }

    #[test]
    fn answer_to_earlier_request_is_intermediate() {
        // Zwei Anfragen kurz hintereinander: IronRDP bedient erst die laufende, dann die letzte.
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        s.connecting((1104, 788), t0);
        s.confirmed((1104, 788));
        s.requested((904, 688), t0);
        s.requested((804, 648), t0);
        s.confirmed((904, 688));
        assert!(!s.wants((804, 648), (904, 688), t0), "804×648 ist noch unterwegs");
        s.confirmed((804, 648));
        assert!(!s.awaiting(t0));
        assert!(!s.wants((804, 648), (804, 648), t0));
    }

    #[test]
    fn connecting_discards_old_requests_and_refusals() {
        let t0 = Instant::now();
        let mut s = SizeTracker::default();
        for _ in 0..SIZE_REQUEST_ATTEMPTS {
            s.requested((1104, 788), t0);
            s.confirmed((1024, 768));
        }
        s.connecting((800, 600), t0);
        s.confirmed((800, 600));
        assert!(s.wants((1104, 788), (800, 600), t0));
    }
}
