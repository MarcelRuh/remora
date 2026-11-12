//! Anwendungs-Controller: verbindet Slint-Oberfläche, Fachlogik und RDP-Sitzungen.
//!
//! Der gesamte Zustand ([`AppState`]) lebt im UI-Thread. Hintergrund-Threads kommunizieren
//! ausschließlich über den [`EventBus`]; der UI-Thread verarbeitet dessen Ereignisse gesammelt.

pub mod convert;
pub mod instance;
pub mod keyboard;
pub mod platform;
pub mod titlebar;
mod updater;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::core::connection_manager::{self, ConnectionManager, PasswordChange, SortMode};
use crate::core::event_bus::{AppEvent, BusSink, EventBus};
use crate::core::session_manager::{LayoutMode, SessionManager};
use crate::database::{
    Connection, ConnectionId, Database, Folder, FolderId, Resolution, ScaleMode, parse_quick_connect,
};
use crate::rdp::client::IronRdpBackend;
use crate::rdp::{CursorUpdate, InputEvent, SessionEvent, SessionId, SessionState};
use crate::security::certificates::{CertDecision, CertificatePrompt};
use crate::security::credentials::{CredentialStore, Secret};
use crate::services::{config_export, rdp_file};
use crate::settings::{AppPaths, AppSettings, OpenBehavior, ThemeMode, autostart};
use crate::ui::{
    AppWindow, Bridge, ConnCard, Dialogs, EditorData, KnownHostItem, PaneItem, SettingsData, TabItem, Toast, TreeItem,
};

use convert::*;

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<AppState>>>> = const { RefCell::new(None) };
}

/// Führt `f` mit dem Anwendungszustand aus. Ist der Zustand gerade ausgeliehen (Reentranz
/// durch synchrone Slint-Callbacks), wird der Aufruf auf den nächsten Event-Loop-Durchlauf
/// verschoben.
fn with_app(f: impl FnOnce(&mut AppState) + 'static) {
    let app = APP.with(|a| a.borrow().clone());
    let Some(app) = app else { return };
    match app.try_borrow_mut() {
        Ok(mut state) => f(&mut state),
        Err(_) => {
            Timer::single_shot(Duration::ZERO, move || with_app(f));
        }
    }
}

/// Offene Verbindungsanfrage, die noch Zugangsdaten benötigt.
struct PendingConnect {
    connection: Connection,
    pane: Option<usize>,
    reconnect: Option<SessionId>,
}

enum ConfirmAction {
    DeleteConnection(ConnectionId),
    DeleteFolder(FolderId),
    CloseTab(usize),
    QuitApp,
    /// Heruntergeladenes und geprüftes Setup starten, danach Remora beenden.
    InstallUpdate(std::path::PathBuf),
}

enum TextAction {
    NewFolder(Option<FolderId>),
    RenameFolder(FolderId),
}

enum VaultAction {
    Export(std::path::PathBuf),
    Import(String),
}

#[derive(Clone, Default)]
struct CursorState {
    mode: i32,
    image: Image,
    width: i32,
    height: i32,
    hx: i32,
    hy: i32,
}

pub struct AppState {
    ui: slint::Weak<AppWindow>,
    paths: AppPaths,
    settings: AppSettings,
    conns: ConnectionManager,
    sessions: SessionManager,
    bus: Arc<EventBus>,

    folders: Vec<Folder>,
    connections: Vec<Connection>,
    folder_choices: Vec<(String, Option<FolderId>)>,
    search: String,
    sidebar_view: i32,
    sort_mode: SortMode,
    selected_tag: String,

    tree_model: Rc<VecModel<TreeItem>>,
    tabs_model: Rc<VecModel<TabItem>>,
    panes_model: Rc<VecModel<PaneItem>>,
    favorites_model: Rc<VecModel<ConnCard>>,
    recent_model: Rc<VecModel<ConnCard>>,
    active_model: Rc<VecModel<ConnCard>>,

    frames: HashMap<SessionId, Image>,
    cursors: HashMap<SessionId, CursorState>,
    pane_geometry: Vec<(f32, f32)>,
    resize_timers: HashMap<SessionId, Timer>,
    reconnect_timers: HashMap<SessionId, Timer>,

    pending_connect: Option<PendingConnect>,
    pending_certs: VecDeque<CertificatePrompt>,
    pending_confirm: Option<ConfirmAction>,
    pending_text: Option<TextAction>,
    pending_vault: Option<VaultAction>,
    editor_base: Option<Connection>,
    toast_until: Option<Instant>,
    /// Gefundene neuere Version (Update-Prüfung).
    update_info: Option<crate::services::update::UpdateInfo>,
    /// Prüfung oder Download läuft.
    update_busy: bool,
    hwnd: isize,
    quitting: bool,
    /// Bereich, dessen Remote-Anzeige den Tastaturfokus hat.
    captured_pane: Option<usize>,
}

// ============================================================================ Start

pub fn run(
    paths: AppPaths,
    settings: AppSettings,
    db: Arc<Database>,
    creds: Arc<dyn CredentialStore>,
    startup_files: Vec<std::path::PathBuf>,
    instance: instance::Listener,
) -> anyhow::Result<()> {
    let ui = AppWindow::new()?;

    // Ereignisbus: weckt den UI-Thread aus beliebigen Threads
    let bus = EventBus::new(|| {
        let _ = slint::invoke_from_event_loop(|| with_app(|s| s.process_events()));
    });

    let backend = Box::new(IronRdpBackend::new(db.clone()));
    let sessions = SessionManager::new(backend, Arc::new(BusSink(bus.clone())));

    let state = AppState {
        ui: ui.as_weak(),
        paths,
        settings,
        conns: ConnectionManager::new(db, creds),
        sessions,
        bus,
        folders: Vec::new(),
        connections: Vec::new(),
        folder_choices: Vec::new(),
        search: String::new(),
        sidebar_view: 0,
        sort_mode: SortMode::Folders,
        selected_tag: String::new(),
        tree_model: Rc::new(VecModel::default()),
        tabs_model: Rc::new(VecModel::default()),
        panes_model: Rc::new(VecModel::default()),
        favorites_model: Rc::new(VecModel::default()),
        recent_model: Rc::new(VecModel::default()),
        active_model: Rc::new(VecModel::default()),
        frames: HashMap::new(),
        cursors: HashMap::new(),
        pane_geometry: vec![(0.0, 0.0); 4],
        resize_timers: HashMap::new(),
        reconnect_timers: HashMap::new(),
        pending_connect: None,
        pending_certs: VecDeque::new(),
        pending_confirm: None,
        pending_text: None,
        pending_vault: None,
        editor_base: None,
        toast_until: None,
        update_info: None,
        update_busy: false,
        hwnd: 0,
        quitting: false,
        captured_pane: None,
    };

    {
        let b = ui.global::<Bridge>();
        b.set_tree(ModelRc::from(state.tree_model.clone()));
        b.set_tabs(ModelRc::from(state.tabs_model.clone()));
        b.set_panes(ModelRc::from(state.panes_model.clone()));
        b.set_favorites(ModelRc::from(state.favorites_model.clone()));
        b.set_recent(ModelRc::from(state.recent_model.clone()));
        b.set_active_connections(ModelRc::from(state.active_model.clone()));
        b.set_resolution_options(ModelRc::from(Rc::new(VecModel::from(resolution_labels()))));
        b.set_version(env!("CARGO_PKG_VERSION").into());
        b.set_backend_info(
            format!(
                "RDP-Engine: {} (ironrdp-client, rustls) · UI: Slint · Datenbank: SQLite (rusqlite, gebündelt) · Zugangsdaten: {}",
                state.sessions.backend_name(),
                if cfg!(windows) { "Windows Credential Manager" } else { "Speicher (nur Laufzeit)" }
            )
            .into(),
        );
        b.set_data_path(state.paths.data_dir.display().to_string().into());
        ui.set_sidebar_width(state.settings.sidebar_width);
        ui.set_sidebar_collapsed(state.settings.sidebar_collapsed);
    }

    let app = Rc::new(RefCell::new(state));
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    wire_callbacks(&ui);

    {
        let mut s = app.borrow_mut();
        s.apply_theme();
        s.refresh_connections();
        s.refresh_sessions_ui();
        s.refresh_known_hosts();
        let data = settings_to_data(&s.settings);
        ui.global::<Dialogs>().set_settings(data);
    }

    // Periodische Aufgaben: Statusleiste, Toast-Ablauf, Vordergrund-Prüfung
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(250), || {
        keyboard::check_foreground();
        with_app(|s| s.tick());
    });

    ui.window().on_close_requested(|| {
        let mut keep = false;
        with_app_sync(|s| keep = s.on_close_requested());
        if keep {
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });

    ui.show()?;

    // Das native Fenster entsteht erst in der Ereignisschleife: so lange erneut versuchen.
    init_native_window(ui.as_weak(), 0);

    // Weitere Programmstarts (z. B. Doppelklick auf .rdp) übergeben ihre Dateien hierher.
    instance.spawn(|files| {
        let _ = slint::invoke_from_event_loop(move || {
            with_app(move |s| {
                if s.hwnd != 0 {
                    platform::bring_to_front(s.hwnd);
                }
                if !files.is_empty() {
                    s.open_rdp_files(files);
                }
            });
        });
    });

    // Update-Prüfung kurz nach dem Start (abschaltbar in den Einstellungen)
    if app.borrow().settings.check_updates {
        Timer::single_shot(Duration::from_secs(5), || with_app(|s| s.start_update_check(false)));
    }

    // Per Kommandozeile übergebene .rdp-Dateien importieren und öffnen (nach Layout-Aufbau)
    if !startup_files.is_empty() {
        Timer::single_shot(Duration::from_millis(300), move || {
            with_app(move |s| s.open_rdp_files(startup_files));
        });
    }

    slint::run_event_loop()?;

    // Aufräumen: Sitzungen trennen, Einstellungen sichern
    {
        let mut s = app.borrow_mut();
        s.save_settings();
        while !s.sessions.is_empty() {
            s.sessions.close(0);
        }
    }
    keyboard::set_target(None);
    APP.with(|a| *a.borrow_mut() = None);
    drop(ticker);
    Ok(())
}

/// Wie [`with_app`], aber synchron (für Rückgabewerte); bei Reentranz wird nichts ausgeführt.
fn with_app_sync(f: impl FnOnce(&mut AppState)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app
        && let Ok(mut s) = app.try_borrow_mut()
    {
        f(&mut s);
    }
}

/// Fensterhandle-abhängige Einrichtung: dunkler Rahmen, eigene Titelleiste, Tastatur-Hook.
fn init_native_window(weak: slint::Weak<AppWindow>, attempt: u32) {
    let Some(ui) = weak.upgrade() else { return };
    let Some(hwnd) = window_handle(&ui) else {
        if attempt < 100 {
            Timer::single_shot(Duration::from_millis(20), move || init_native_window(weak, attempt + 1));
        } else {
            tracing::warn!("Kein natives Fensterhandle verfügbar");
        }
        return;
    };
    with_app_sync(|s| s.hwnd = hwnd);
    platform::set_dark_title_bar(hwnd, ui.get_dark_mode());
    // Eigene Titelleiste; REMORA_NATIVE_TITLEBAR=1 behält die Standardleiste von Windows.
    if std::env::var_os("REMORA_NATIVE_TITLEBAR").is_none() {
        let weak = ui.as_weak();
        let installed = titlebar::install(
            hwnd,
            Box::new(move |event| {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let b = ui.global::<Bridge>();
                    match event {
                        titlebar::WindowEvent::Maximized(m) => b.set_window_maximized(m),
                        titlebar::WindowEvent::Active(a) => b.set_window_active(a),
                        titlebar::WindowEvent::MaxHover(h) => b.set_max_button_hover(h),
                        titlebar::WindowEvent::MaxPressed(p) => b.set_max_button_pressed(p),
                    }
                });
            }),
        );
        ui.global::<Bridge>().set_custom_titlebar(installed);
    }
    keyboard::install(hwnd, |action| with_app(move |s| s.on_local_action(action)));
    let mut policy = None;
    with_app_sync(|s| policy = Some(s.settings.windows_keys));
    if let Some(p) = policy {
        keyboard::set_policy(p, false);
    }
    tracing::debug!(
        custom_titlebar = ui.global::<Bridge>().get_custom_titlebar(),
        "Fenster eingerichtet"
    );
}

fn window_handle(ui: &AppWindow) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = ui.window().window_handle();
    let wh = handle.window_handle().ok()?;
    match wh.as_raw() {
        #[cfg(windows)]
        RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

// ============================================================================ Callbacks

fn wire_callbacks(ui: &AppWindow) {
    let b = ui.global::<Bridge>();

    // ---------------------------------------------------- Verbindungen / Sidebar
    b.on_search_changed(|t| {
        with_app(move |s| {
            s.search = t.to_string();
            s.refresh_tree();
        })
    });
    b.on_tree_activate(|kind, id| {
        with_app(move |s| {
            if kind == 1 {
                s.connect(id as ConnectionId, None);
            } else {
                s.toggle_folder(id as FolderId);
            }
        })
    });
    b.on_tree_toggle(|id| with_app(move |s| s.toggle_folder(id as FolderId)));
    b.on_connect(|id| with_app(move |s| s.connect(id as ConnectionId, None)));
    b.on_connect_in_pane(|id, pane| {
        with_app(move |s| {
            let pane = (pane >= 0).then_some(pane as usize);
            s.connect(id as ConnectionId, pane);
        })
    });
    b.on_quick_connect(|text| with_app(move |s| s.quick_connect(&text)));
    b.on_new_connection(|folder| with_app(move |s| s.open_editor(None, (folder >= 0).then_some(folder as FolderId))));
    b.on_edit_connection(|id| with_app(move |s| s.open_editor(Some(id as ConnectionId), None)));
    b.on_duplicate_connection(|id| {
        with_app(move |s| {
            match s.conns.duplicate(id as ConnectionId) {
                Ok(_) => s.toast("Verbindung dupliziert.", 1),
                Err(e) => s.toast(&format!("Duplizieren fehlgeschlagen: {e}"), 3),
            }
            s.refresh_connections();
        })
    });
    b.on_delete_connection(|id| {
        with_app(move |s| {
            let name = s
                .connection(id as ConnectionId)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            s.confirm(
                ConfirmAction::DeleteConnection(id as ConnectionId),
                "Verbindung löschen?",
                &format!("„{name}“ wird dauerhaft gelöscht, einschließlich des gespeicherten Passworts."),
                "Löschen",
                true,
            );
        })
    });
    b.on_toggle_favorite(|id| {
        with_app(move |s| {
            if let Some(c) = s.connection(id as ConnectionId) {
                let fav = !c.favorite;
                if let Err(e) = s.conns.db().set_favorite(id as ConnectionId, fav) {
                    s.toast(&e.to_string(), 3);
                }
                s.refresh_connections();
            }
        })
    });
    b.on_move_connection(|id, folder| {
        with_app(move |s| {
            let folder = (folder >= 0).then_some(folder as FolderId);
            if let Err(e) = s.conns.db().move_connection(id as ConnectionId, folder) {
                s.toast(&e.to_string(), 3);
            }
            s.refresh_connections();
        })
    });
    b.on_move_folder(|id, parent| {
        with_app(move |s| {
            let parent = (parent >= 0).then_some(parent as FolderId);
            if parent == Some(id as FolderId) {
                return;
            }
            if let Err(e) = s.conns.db().move_folder(id as FolderId, parent) {
                s.toast(&e.to_string(), 2);
            }
            s.refresh_connections();
        })
    });
    b.on_new_folder(|parent| {
        with_app(move |s| {
            s.text_dialog(
                TextAction::NewFolder((parent >= 0).then_some(parent as FolderId)),
                "Neuer Ordner",
                "Name",
                "",
                "Anlegen",
            );
        })
    });
    b.on_rename_folder(|id| {
        with_app(move |s| {
            let name = s
                .folders
                .iter()
                .find(|f| f.id == id as FolderId)
                .map(|f| f.name.clone())
                .unwrap_or_default();
            s.text_dialog(
                TextAction::RenameFolder(id as FolderId),
                "Ordner umbenennen",
                "Name",
                &name,
                "Umbenennen",
            );
        })
    });
    b.on_delete_folder(|id| with_app(move |s| {
        let name = s.folders.iter().find(|f| f.id == id as FolderId).map(|f| f.name.clone()).unwrap_or_default();
        s.confirm(
            ConfirmAction::DeleteFolder(id as FolderId),
            "Ordner löschen?",
            &format!("„{name}“ und alle Unterordner werden gelöscht. Enthaltene Verbindungen bleiben erhalten und werden in den übergeordneten Ordner verschoben."),
            "Ordner löschen",
            true,
        );
    }));
    b.on_export_rdp(|id| with_app(move |s| s.export_rdp(id as ConnectionId)));
    b.on_import_rdp(|| with_app(|s| s.import_rdp()));
    b.on_set_sidebar_view(|v| {
        with_app(move |s| {
            s.sidebar_view = v;
            s.with_ui(|ui| ui.global::<Bridge>().set_sidebar_view(v));
        })
    });
    b.on_set_sort_mode(|m| {
        with_app(move |s| {
            s.sort_mode = if m == 1 { SortMode::Name } else { SortMode::Folders };
            s.with_ui(|ui| ui.global::<Bridge>().set_sort_mode(m));
            s.refresh_tree();
        })
    });
    b.on_select_tag(|t| {
        with_app(move |s| {
            s.selected_tag = t.to_string();
            s.with_ui(|ui| ui.global::<Bridge>().set_selected_tag(t.clone()));
            s.refresh_tree();
        })
    });

    // ---------------------------------------------------- Sitzungen
    b.on_activate_tab(|i| {
        with_app(move |s| {
            s.sessions.activate((i >= 0).then_some(i as usize));
            if i < 0 && s.sessions.layout != LayoutMode::Single {
                s.sessions.set_layout(LayoutMode::Single);
                s.sessions.activate(None);
            }
            s.refresh_sessions_ui();
            if i >= 0 {
                s.request_pane_focus(s.sessions.focused_pane);
            }
        })
    });
    b.on_close_tab(|i| with_app(move |s| s.request_close_tab(i as usize)));
    b.on_move_tab(|from, to| {
        with_app(move |s| {
            s.sessions.move_tab(from as usize, to as usize);
            s.refresh_sessions_ui();
        })
    });
    b.on_next_tab(|d| {
        with_app(move |s| {
            s.sessions.cycle(d);
            s.refresh_sessions_ui();
        })
    });
    b.on_disconnect_session(|id| {
        with_app(move |s| {
            s.reconnect_timers.remove(&(id as SessionId));
            s.sessions.disconnect(id as SessionId);
        })
    });
    b.on_reconnect_session(|id| with_app(move |s| s.reconnect(id as SessionId, false)));
    b.on_set_layout(|m| {
        with_app(move |s| {
            s.sessions.set_layout(LayoutMode::from_index(m));
            s.refresh_sessions_ui();
        })
    });
    b.on_assign_pane(|pane, session| {
        with_app(move |s| {
            if pane < 0 {
                return;
            }
            s.sessions
                .assign_pane(pane as usize, (session >= 0).then_some(session as SessionId));
            s.refresh_sessions_ui();
        })
    });
    b.on_maximize_pane(|pane| {
        with_app(move |s| {
            s.sessions.maximize_pane(pane as usize);
            s.refresh_sessions_ui();
            s.request_pane_focus(0);
        })
    });
    b.on_focus_pane(|pane| {
        with_app(move |s| {
            if s.sessions.focused_pane != pane as usize {
                s.sessions.focus_pane(pane as usize);
                s.refresh_sessions_ui();
            }
        })
    });
    b.on_pane_geometry(|pane, w, h| with_app(move |s| s.on_pane_geometry(pane as usize, w, h)));
    b.on_pane_pointer(|pane, kind, button, x, y, w, h| {
        with_app(move |s| s.on_pointer(pane as usize, kind, button, x, y, w, h))
    });
    b.on_pane_scroll(|pane, dx, dy| with_app(move |s| s.on_scroll(pane as usize, dx, dy)));
    b.on_pane_capture(|pane, focused| with_app(move |s| s.on_capture(pane as usize, focused)));
    b.on_send_ctrl_alt_del(|id| {
        with_app(move |s| {
            if let Some(h) = s.sessions.get(id as SessionId).and_then(|e| e.handle.clone()) {
                h.send_input(InputEvent::CtrlAltDel);
            }
        })
    });
    b.on_toggle_fullscreen(|| with_app(|s| s.toggle_fullscreen()));
    b.on_window_minimize(|| with_app(|s| titlebar::minimize(s.hwnd)));
    b.on_window_toggle_maximize(|| with_app(|s| titlebar::toggle_maximize(s.hwnd)));
    b.on_window_close(|| with_app(|s| titlebar::close(s.hwnd)));
    b.on_set_scale_mode(|id, mode| {
        with_app(move |s| {
            if let Some(e) = s.sessions.get_mut(id as SessionId) {
                e.scale_mode = index_to_scale(mode);
            }
            s.refresh_panes();
        })
    });

    // ---------------------------------------------------- Dialoge
    b.on_editor_save(|data| {
        let mut result = SharedString::default();
        with_app_sync(|s| result = s.save_editor(&data).into());
        result
    });
    b.on_credentials_submit(|user, domain, password, save| {
        let password = Secret::from(password.to_string());
        with_app(move |s| s.submit_credentials(user.to_string(), domain.to_string(), password, save));
    });
    b.on_credentials_cancel(|| {
        with_app(|s| {
            s.pending_connect = None;
            s.with_ui(|ui| ui.global::<Dialogs>().set_credentials_open(false));
        })
    });
    b.on_certificate_decision(|d| {
        with_app(move |s| {
            let decision = match d {
                2 => CertDecision::AcceptAlways,
                1 => CertDecision::AcceptOnce,
                _ => CertDecision::Reject,
            };
            if let Some(prompt) = s.pending_certs.pop_front() {
                let _ = prompt.reply.send(decision);
            }
            s.show_next_certificate();
            if decision == CertDecision::AcceptAlways {
                s.refresh_known_hosts();
            }
        })
    });
    b.on_confirm_accept(|| with_app(|s| s.on_confirm(true)));
    b.on_confirm_cancel(|| with_app(|s| s.on_confirm(false)));
    b.on_text_dialog_accept(|t| with_app(move |s| s.on_text_accept(t.to_string())));
    b.on_text_dialog_cancel(|| {
        with_app(|s| {
            s.pending_text = None;
            s.with_ui(|ui| ui.global::<Dialogs>().set_text_open(false));
        })
    });
    b.on_vault_accept(|pw| {
        let pw = Secret::from(pw.to_string());
        with_app(move |s| s.on_vault_accept(pw));
    });
    b.on_vault_cancel(|| {
        with_app(|s| {
            s.pending_vault = None;
            s.with_ui(|ui| ui.global::<Dialogs>().set_vault_open(false));
        })
    });

    // ---------------------------------------------------- Einstellungen
    b.on_settings_changed(|data| with_app(move |s| s.on_settings_changed(&data)));
    b.on_export_config(|encrypted| with_app(move |s| s.export_config(encrypted)));
    b.on_import_config(|| with_app(|s| s.import_config()));
    b.on_forget_known_host(|endpoint| {
        with_app(move |s| {
            if let Err(e) = s.conns.db().forget_host(&endpoint) {
                s.toast(&e.to_string(), 3);
            }
            s.refresh_known_hosts();
        })
    });
    b.on_open_data_folder(|| with_app(|s| platform::open_folder(&s.paths.data_dir)));
    b.on_dismiss_toast(|| with_app(|s| s.hide_toast()));
    b.on_check_updates(|| with_app(|s| s.start_update_check(true)));
    b.on_open_update(|| with_app(|s| s.open_update()));
    b.on_install_update(|| with_app(|s| s.install_update()));

    ui.on_sidebar_resized(|w| {
        with_app(move |s| {
            s.settings.sidebar_width = w;
            s.save_settings();
        })
    });
    ui.on_sidebar_collapsed_changed(|c| {
        with_app(move |s| {
            s.settings.sidebar_collapsed = c;
            s.save_settings();
        })
    });
}

// ============================================================================ Zustand

impl AppState {
    fn with_ui(&self, f: impl FnOnce(&AppWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            f(&ui);
        }
    }

    fn connection(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    fn device_scale(&self) -> f32 {
        self.ui.upgrade().map_or(1.0, |ui| ui.window().scale_factor())
    }

    fn toast(&mut self, message: &str, kind: i32) {
        self.toast_until = Some(Instant::now() + Duration::from_millis(if kind >= 2 { 6000 } else { 3500 }));
        let message: SharedString = message.into();
        self.with_ui(|ui| {
            ui.global::<Bridge>().set_toast(Toast {
                message,
                kind,
                visible: true,
            });
        });
    }

    fn hide_toast(&mut self) {
        self.toast_until = None;
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            let mut t = b.get_toast();
            t.visible = false;
            b.set_toast(t);
        });
    }

    fn save_settings(&self) {
        if let Err(e) = self.settings.save(&self.paths.settings()) {
            tracing::error!(error = %e, "Einstellungen konnten nicht gespeichert werden");
        }
    }

    fn apply_theme(&self) {
        let dark = match self.settings.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => platform::system_prefers_dark(),
        };
        let accent = color_from_hex(&self.settings.accent_color);
        let hwnd = self.hwnd;
        self.with_ui(|ui| {
            ui.set_dark_mode(dark);
            ui.set_accent_color(accent);
        });
        if hwnd != 0 {
            platform::set_dark_title_bar(hwnd, dark);
        }
    }

    // ------------------------------------------------------------ Datenansichten

    fn refresh_connections(&mut self) {
        let db = self.conns.db().clone();
        match (db.list_folders(), db.list_connections()) {
            (Ok(f), Ok(c)) => {
                self.folders = f;
                self.connections = c;
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::error!(error = %e, "Verbindungen konnten nicht geladen werden");
                self.toast(&format!("Datenbankfehler: {e}"), 3);
            }
        }
        self.folder_choices = folder_choices(&self.folders);
        let tags = ConnectionManager::all_tags(&self.connections);
        if !self.selected_tag.is_empty() && !tags.contains(&self.selected_tag) {
            self.selected_tag.clear();
        }
        let labels: Vec<SharedString> = self.folder_choices.iter().map(|(l, _)| l.as_str().into()).collect();
        let count = self.connections.len() as i32;
        let selected_tag: SharedString = self.selected_tag.as_str().into();
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            b.set_tags(ModelRc::from(Rc::new(VecModel::from(
                tags.into_iter().map(SharedString::from).collect::<Vec<_>>(),
            ))));
            b.set_folder_labels(ModelRc::from(Rc::new(VecModel::from(labels))));
            b.set_connection_count(count);
            b.set_selected_tag(selected_tag);
        });
        self.refresh_tree();
        self.refresh_cards();
        self.refresh_status_bar();
    }

    fn status_by_connection(&self) -> HashMap<ConnectionId, i32> {
        let mut map = HashMap::new();
        for (id, code) in self.sessions.active_connection_ids() {
            // Verbunden hat Vorrang vor anderen Zuständen
            let e = map.entry(id).or_insert(code);
            if code == 2 {
                *e = 2;
            }
        }
        map
    }

    fn refresh_tree(&mut self) {
        let mut query = self.search.clone();
        if !self.selected_tag.is_empty() {
            query.push_str(&format!(" #{}", self.selected_tag));
        }
        let rows = connection_manager::build_tree(&self.folders, &self.connections, &query, self.sort_mode);
        let status = self.status_by_connection();
        let items: Vec<TreeItem> = rows
            .into_iter()
            .map(|r| {
                let conn = (r.kind == connection_manager::TreeRowKind::Connection)
                    .then(|| self.connection(r.id))
                    .flatten();
                TreeItem {
                    kind: if conn.is_some() { 1 } else { 0 },
                    id: r.id as i32,
                    depth: r.depth as i32,
                    name: r.name.into(),
                    subtitle: r.subtitle.into(),
                    expanded: r.expanded,
                    count: r.child_count as i32,
                    color: conn.map_or_else(|| color_from_hex("#8f96a3"), |c| color_from_hex(&c.color)),
                    icon: conn.map_or("folder".into(), |c| c.icon.as_str().into()),
                    favorite: conn.is_some_and(|c| c.favorite),
                    status: conn.and_then(|c| status.get(&c.id).copied()).unwrap_or(-1),
                    folder: match conn {
                        Some(c) => c.folder_id.map_or(-1, |f| f as i32),
                        None => self
                            .folders
                            .iter()
                            .find(|f| f.id == r.id)
                            .and_then(|f| f.parent_id)
                            .map_or(-1, |f| f as i32),
                    },
                }
            })
            .collect();
        self.tree_model.set_vec(items);
    }

    fn refresh_cards(&mut self) {
        let status = self.status_by_connection();
        let st = |c: &Connection| status.get(&c.id).copied().unwrap_or(-1);
        let favorites: Vec<ConnCard> = self
            .connections
            .iter()
            .filter(|c| c.favorite)
            .map(|c| card(c, st(c)))
            .collect();
        let mut recent: Vec<&Connection> = self.connections.iter().filter(|c| c.last_used.is_some()).collect();
        recent.sort_by_key(|c| std::cmp::Reverse(c.last_used));
        let recent: Vec<ConnCard> = recent.into_iter().take(12).map(|c| card(c, st(c))).collect();
        let active: Vec<ConnCard> = self
            .sessions
            .sessions()
            .iter()
            .map(|s| {
                let mut c = card(&s.connection, s.state.code());
                c.last_used = s.state.label().into();
                c
            })
            .collect();
        self.favorites_model.set_vec(favorites);
        self.recent_model.set_vec(recent);
        self.active_model.set_vec(active);
    }

    fn refresh_known_hosts(&self) {
        let hosts = self.conns.db().list_known_hosts().unwrap_or_default();
        let items: Vec<KnownHostItem> = hosts
            .into_iter()
            .map(|h| KnownHostItem {
                endpoint: h.endpoint.into(),
                fingerprint: h.fingerprint.into(),
                first_seen: h
                    .first_seen
                    .with_timezone(&chrono::Local)
                    .format("%d.%m.%Y %H:%M")
                    .to_string()
                    .into(),
            })
            .collect();
        self.with_ui(|ui| {
            ui.global::<Bridge>()
                .set_known_hosts(ModelRc::from(Rc::new(VecModel::from(items))))
        });
    }

    /// Tabs, Bereiche, Statusleiste und Sitzungsstatus in Sidebar/Karten aktualisieren.
    fn refresh_sessions_ui(&mut self) {
        let tabs: Vec<TabItem> = self
            .sessions
            .sessions()
            .iter()
            .map(|s| TabItem {
                session_id: s.id as i32,
                name: s.connection.name.as_str().into(),
                host: s.connection.host.as_str().into(),
                status: s.state.code(),
                status_text: s.state.label().into(),
                color: color_from_hex(&s.connection.color),
                icon: s.connection.icon.as_str().into(),
            })
            .collect();
        if self.tabs_model.row_count() == tabs.len() {
            for (i, t) in tabs.into_iter().enumerate() {
                if self.tabs_model.row_data(i).as_ref() != Some(&t) {
                    self.tabs_model.set_row_data(i, t);
                }
            }
        } else {
            self.tabs_model.set_vec(tabs);
        }
        let active = self.sessions.active.map_or(-1, |i| i as i32);
        let layout = self.sessions.layout.index();
        let focused = self.sessions.focused_pane as i32;
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            b.set_active_tab(active);
            b.set_layout_mode(layout);
            b.set_focused_pane(focused);
        });
        self.refresh_panes();
        // Tabwechsel, Bereichszuordnung, Schließen oder Neuverbinden ändern das Tastaturziel
        self.sync_keyboard_target();
        self.refresh_status_bar();
        self.refresh_tree();
        self.refresh_cards();
    }

    fn pane_item(&self, session: Option<SessionId>) -> PaneItem {
        let Some(entry) = session.and_then(|id| self.sessions.get(id)) else {
            return PaneItem {
                session_id: -1,
                ..Default::default()
            };
        };
        let cursor = self.cursors.get(&entry.id).cloned().unwrap_or_default();
        let frame = self.frames.get(&entry.id).cloned();
        PaneItem {
            session_id: entry.id as i32,
            name: entry.connection.name.as_str().into(),
            host: entry.connection.endpoint().into(),
            status: entry.state.code(),
            status_text: entry.state.label().into(),
            has_frame: frame.is_some(),
            frame: frame.unwrap_or_default(),
            remote_width: i32::from(entry.remote_size.0),
            remote_height: i32::from(entry.remote_size.1),
            scale_mode: scale_to_index(effective_scale(entry.scale_mode, entry.connection.options.resolution)),
            cursor_mode: cursor.mode,
            cursor: cursor.image,
            cursor_width: cursor.width,
            cursor_height: cursor.height,
            cursor_hotspot_x: cursor.hx,
            cursor_hotspot_y: cursor.hy,
            can_reconnect: matches!(
                entry.state,
                SessionState::Disconnected { .. } | SessionState::Error { .. }
            ),
        }
    }

    fn refresh_panes(&mut self) {
        let items: Vec<PaneItem> = self.sessions.panes.iter().map(|p| self.pane_item(*p)).collect();
        if self.panes_model.row_count() == items.len() {
            for (i, item) in items.into_iter().enumerate() {
                self.panes_model.set_row_data(i, item);
            }
        } else {
            self.panes_model.set_vec(items);
        }
    }

    fn refresh_pane_of(&mut self, id: SessionId) {
        if let Some(pane) = self.sessions.pane_of(id)
            && pane < self.panes_model.row_count()
        {
            let item = self.pane_item(Some(id));
            self.panes_model.set_row_data(pane, item);
        }
    }

    fn refresh_status_bar(&mut self) {
        let focused = self
            .sessions
            .focused_session_id()
            .and_then(|id| self.sessions.get(id))
            .or_else(|| self.sessions.active_session());
        let (state, host, res, dur, msg) = match focused {
            Some(e) => (
                e.state.code(),
                e.connection.endpoint(),
                if e.remote_size.0 > 0 {
                    format!("{} × {}", e.remote_size.0, e.remote_size.1)
                } else {
                    String::new()
                },
                e.connected_at
                    .map(|t| format_duration(t.elapsed().as_secs()))
                    .unwrap_or_default(),
                match &e.state {
                    SessionState::Connected => format!("{} · {}", e.connection.name, e.connection.display_user()),
                    other => other.label(),
                },
            ),
            None => (
                -1,
                String::new(),
                String::new(),
                String::new(),
                match self.connections.len() {
                    1 => "1 Verbindung".to_owned(),
                    n => format!("{n} Verbindungen"),
                },
            ),
        };
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            b.set_status_state(state);
            b.set_status_host(host.into());
            b.set_status_resolution(res.into());
            b.set_status_duration(dur.into());
            b.set_status_message(msg.into());
        });
    }

    fn tick(&mut self) {
        if let Some(until) = self.toast_until
            && Instant::now() >= until
        {
            self.hide_toast();
        }
        // Verbindungsdauer
        let dur = self
            .sessions
            .focused_session_id()
            .and_then(|id| self.sessions.get(id))
            .and_then(|e| e.connected_at)
            .map(|t| format_duration(t.elapsed().as_secs()))
            .unwrap_or_default();
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            if b.get_status_duration() != dur.as_str() {
                b.set_status_duration(dur.into());
            }
        });
    }

    fn toggle_folder(&mut self, id: FolderId) {
        if let Some(f) = self.folders.iter_mut().find(|f| f.id == id) {
            f.expanded = !f.expanded;
            let _ = self.conns.db().set_folder_expanded(id, f.expanded);
        }
        self.refresh_tree();
    }

    // ------------------------------------------------------------ Verbinden

    fn connect(&mut self, id: ConnectionId, pane: Option<usize>) {
        let Some(conn) = self.connection(id).cloned() else {
            return;
        };
        if self.settings.open_behavior == OpenBehavior::FocusExisting
            && let Some(existing) = self.sessions.find_by_connection(id)
        {
            let state_ok = self.sessions.get(existing).is_some_and(|e| e.state.is_active());
            if let Some(pane) = pane {
                self.sessions.assign_pane(pane, Some(existing));
            } else {
                let idx = self.sessions.index_of(existing);
                self.sessions.activate(idx);
            }
            if !state_ok {
                self.reconnect(existing, false);
            }
            self.refresh_sessions_ui();
            self.request_pane_focus(self.sessions.focused_pane);
            return;
        }
        self.begin_connect(conn, pane, None);
    }

    fn quick_connect(&mut self, input: &str) {
        let Some((host, port, user, domain)) = parse_quick_connect(input) else {
            self.toast(
                "Ungültige Adresse. Beispiel: server01, 10.0.0.5:3390 oder DOMÄNE\\benutzer@host",
                2,
            );
            return;
        };
        // Bekannte Verbindung mit gleicher Adresse bevorzugen
        if let Some(existing) = self
            .connections
            .iter()
            .find(|c| c.host.eq_ignore_ascii_case(&host) && c.port == port && (user.is_empty() || c.username == user))
            .map(|c| c.id)
        {
            self.connect(existing, None);
            return;
        }
        let mut conn = Connection::new(&host, &host);
        conn.port = port;
        conn.username = user;
        conn.domain = domain;
        conn.icon = "zap".into();
        conn.options.scale_mode = self.settings.default_scale_mode;
        conn.options.resolution = self.settings.default_resolution;
        self.begin_connect(conn, None, None);
    }

    /// Fragt bei Bedarf Zugangsdaten ab und öffnet dann die Sitzung.
    fn begin_connect(&mut self, conn: Connection, pane: Option<usize>, reconnect: Option<SessionId>) {
        let stored = (conn.id != 0).then(|| self.conns.password(conn.id)).flatten();
        let needs_prompt = conn.options.nla && (conn.username.is_empty() || stored.is_none());
        if needs_prompt {
            let title: SharedString = if conn.id == 0 {
                "Schnellverbindung".into()
            } else {
                format!("Anmelden bei {}", conn.name).into()
            };
            let host: SharedString = conn.endpoint().into();
            let user: SharedString = conn.username.as_str().into();
            let domain: SharedString = conn.domain.as_str().into();
            let can_save = conn.id != 0;
            self.pending_connect = Some(PendingConnect {
                connection: conn,
                pane,
                reconnect,
            });
            self.with_ui(|ui| {
                let d = ui.global::<Dialogs>();
                d.set_credentials_title(title);
                d.set_credentials_host(host);
                d.set_credentials_user(user);
                d.set_credentials_domain(domain);
                d.set_credentials_error(SharedString::default());
                d.set_credentials_can_save(can_save);
                d.set_credentials_open(true);
            });
            return;
        }
        self.open_session(conn, stored, pane, reconnect);
    }

    fn submit_credentials(&mut self, user: String, domain: String, password: Secret, save: bool) {
        let Some(mut pending) = self.pending_connect.take() else {
            return;
        };
        self.with_ui(|ui| ui.global::<Dialogs>().set_credentials_open(false));
        let user = user.trim().to_owned();
        let (domain, user) = match (domain.trim().is_empty(), user.split_once('\\')) {
            (true, Some((d, u))) => (d.to_owned(), u.to_owned()),
            _ => (domain.trim().to_owned(), user),
        };
        let changed = pending.connection.username != user || pending.connection.domain != domain;
        pending.connection.username = user;
        pending.connection.domain = domain;
        if pending.connection.id != 0 {
            if changed && let Err(e) = self.conns.db().update_connection(&pending.connection) {
                tracing::warn!(error = %e, "Benutzername konnte nicht gespeichert werden");
            }
            if save && !password.is_empty() {
                match self
                    .conns
                    .store_password(pending.connection.id, &pending.connection.username, &password)
                {
                    Ok(()) => self.toast("Passwort im Windows Credential Manager gespeichert.", 1),
                    Err(e) => self.toast(&format!("Passwort konnte nicht gespeichert werden: {e}"), 3),
                }
            }
            if changed || save {
                self.refresh_connections();
            }
        }
        self.open_session(pending.connection, Some(password), pending.pane, pending.reconnect);
    }

    fn desktop_size_for(&self, pane: usize) -> (u16, u16) {
        let (w, h) = self.pane_geometry.get(pane).copied().unwrap_or((0.0, 0.0));
        let scale = self.device_scale();
        let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
        if pw < 200 || ph < 200 {
            // Fallback, falls die Geometrie noch unbekannt ist
            return (1600, 900);
        }
        (((pw.min(8192)) & !1) as u16, ph.min(8192) as u16)
    }

    fn scale_percent(&self) -> u32 {
        ((self.device_scale() * 100.0).round() as u32).clamp(100, 500)
    }

    fn auto_reconnect_for(&self, conn: &Connection) -> u32 {
        let enabled = conn.options.auto_reconnect.unwrap_or(self.settings.auto_reconnect);
        if enabled { self.settings.reconnect_attempts } else { 0 }
    }

    fn open_session(
        &mut self,
        conn: Connection,
        password: Option<Secret>,
        pane: Option<usize>,
        reconnect: Option<SessionId>,
    ) {
        let gateway_password = (conn.id != 0 && conn.options.gateway.enabled)
            .then(|| self.conns.gateway_password(conn.id))
            .flatten();
        let timeout = Duration::from_secs(u64::from(self.settings.connect_timeout_secs));
        let attempts = self.auto_reconnect_for(&conn);
        let scale = self.scale_percent();

        if conn.id != 0 {
            let _ = self.conns.db().touch_last_used(conn.id);
        }

        if let Some(id) = reconnect {
            // Wiederverbindung mit (ggf. neuen) Zugangsdaten
            let pane = self.sessions.pane_of(id).unwrap_or(self.sessions.focused_pane);
            let size = self.desktop_size_for(pane);
            if let Some(e) = self.sessions.get_mut(id) {
                e.connection = conn;
                e.password = password;
                e.gateway_password = gateway_password;
            }
            self.frames.remove(&id);
            self.sessions.reconnect(id, size, scale, timeout, attempts);
        } else {
            let target_pane = pane.unwrap_or(self.sessions.focused_pane);
            let size = self.desktop_size_for(target_pane);
            let fullscreen = conn.options.fullscreen;
            let id = self
                .sessions
                .open(conn, password, gateway_password, size, scale, timeout, attempts);
            if let Some(p) = pane
                && self.sessions.layout != LayoutMode::Single
            {
                self.sessions.assign_pane(p, Some(id));
            }
            if fullscreen && !self.fullscreen() {
                self.toggle_fullscreen();
            }
        }
        self.refresh_connections();
        self.refresh_sessions_ui();
        self.request_pane_focus(self.sessions.focused_pane);
    }

    fn reconnect(&mut self, id: SessionId, automatic: bool) {
        self.reconnect_timers.remove(&id);
        let Some(entry) = self.sessions.get(id) else { return };
        let conn = entry.connection.clone();
        let password = entry
            .password
            .clone()
            .or_else(|| (conn.id != 0).then(|| self.conns.password(conn.id)).flatten());
        if password.is_none() && conn.options.nla && !automatic {
            self.begin_connect(conn, None, Some(id));
            return;
        }
        if let Some(e) = self.sessions.get_mut(id) {
            e.password = password.clone();
            if !automatic {
                e.app_reconnects = 0;
            }
        }
        self.open_session(conn, password, None, Some(id));
    }

    fn request_close_tab(&mut self, index: usize) {
        let Some(entry) = self.sessions.sessions().get(index) else {
            return;
        };
        if self.settings.confirm_close_active && entry.is_connected() {
            let name = entry.connection.name.clone();
            self.confirm(
                ConfirmAction::CloseTab(index),
                "Aktive Sitzung schließen?",
                &format!("Die Verbindung zu „{name}“ wird getrennt. Nicht gespeicherte Arbeit in der Remote-Sitzung bleibt auf dem Server erhalten, sofern die Sitzung dort nicht abgemeldet wird."),
                "Trennen & schließen",
                false,
            );
        } else {
            self.close_tab(index);
        }
    }

    fn close_tab(&mut self, index: usize) {
        if let Some(entry) = self.sessions.close(index) {
            // Tastaturziel folgt in refresh_sessions_ui()
            self.frames.remove(&entry.id);
            self.cursors.remove(&entry.id);
            self.resize_timers.remove(&entry.id);
            self.reconnect_timers.remove(&entry.id);
        }
        if self.sessions.is_empty() && self.fullscreen() {
            self.toggle_fullscreen();
        }
        self.refresh_sessions_ui();
    }

    fn fullscreen(&self) -> bool {
        self.ui
            .upgrade()
            .is_some_and(|ui| ui.global::<Bridge>().get_fullscreen())
    }

    fn toggle_fullscreen(&mut self) {
        let fs = !self.fullscreen();
        let hwnd = self.hwnd;
        self.with_ui(|ui| {
            let custom = ui.global::<Bridge>().get_custom_titlebar();
            ui.global::<Bridge>().set_fullscreen(fs);
            ui.global::<Bridge>().set_fullscreen_hint(false);
            // Im Vollbild keine Ziehfläche: Klicks oben gehen an die Remote-Sitzung.
            if fs {
                titlebar::set_enabled(hwnd, false, false);
            }
            ui.window().set_fullscreen(fs);
            if !fs && custom {
                titlebar::set_enabled(hwnd, true, true);
            }
        });
        keyboard::set_policy(self.settings.windows_keys, fs);
    }

    fn request_pane_focus(&self, pane: usize) {
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            b.set_focus_request_pane(pane as i32);
            b.set_focus_request(b.get_focus_request() + 1);
        });
    }

    fn on_local_action(&mut self, action: keyboard::LocalAction) {
        use keyboard::LocalAction;
        match action {
            LocalAction::ReleaseFocus => {
                self.captured_pane = None;
                self.sync_keyboard_target();
                self.with_ui(|ui| {
                    let b = ui.global::<Bridge>();
                    b.set_search_focus_request(b.get_search_focus_request() + 1);
                });
            }
            LocalAction::ToggleFullscreen => self.toggle_fullscreen(),
            LocalAction::PreviousTab | LocalAction::NextTab => {
                self.sessions.cycle(if action == LocalAction::NextTab { 1 } else { -1 });
                self.refresh_sessions_ui();
                self.request_pane_focus(self.sessions.focused_pane);
            }
            LocalAction::CtrlAltDel => {}
        }
    }

    // ------------------------------------------------------------ Eingabe

    fn session_in_pane(&self, pane: usize) -> Option<SessionId> {
        self.sessions.panes.get(pane).copied().flatten()
    }

    fn on_pane_geometry(&mut self, pane: usize, w: f32, h: f32) {
        if pane >= self.pane_geometry.len() {
            self.pane_geometry.resize(pane + 1, (0.0, 0.0));
        }
        if self.pane_geometry[pane] == (w, h) {
            return;
        }
        self.pane_geometry[pane] = (w, h);
        let Some(id) = self.session_in_pane(pane) else { return };
        self.schedule_resize(id, pane);
    }

    /// Dynamische Auflösung: nach kurzer Ruhephase neue Größe an den Server melden.
    fn schedule_resize(&mut self, id: SessionId, pane: usize) {
        let Some(entry) = self.sessions.get(id) else { return };
        if entry.connection.options.resolution != Resolution::FitToWindow || !entry.is_connected() {
            return;
        }
        let timer = Timer::default();
        timer.start(TimerMode::SingleShot, Duration::from_millis(450), move || {
            with_app(move |s| {
                s.resize_timers.remove(&id);
                if s.sessions.pane_of(id) != Some(pane) {
                    return;
                }
                let (w, h) = s.desktop_size_for(pane);
                let scale = s.scale_percent();
                if let Some(e) = s.sessions.get(id)
                    && let Some(handle) = &e.handle
                    && (e.remote_size.0.abs_diff(w) > 8 || e.remote_size.1.abs_diff(h) > 8)
                {
                    tracing::debug!(session = id, w, h, "Dynamische Auflösung angefordert");
                    handle.resize(w, h, scale);
                }
            });
        });
        self.resize_timers.insert(id, timer);
    }

    #[allow(clippy::too_many_arguments)]
    fn on_pointer(&mut self, pane: usize, kind: i32, button: i32, x: f32, y: f32, w: f32, h: f32) {
        if self.fullscreen() {
            let hint = y < 4.0;
            let far = y > 60.0;
            self.with_ui(|ui| {
                let b = ui.global::<Bridge>();
                if hint && !b.get_fullscreen_hint() {
                    b.set_fullscreen_hint(true);
                } else if far && b.get_fullscreen_hint() {
                    b.set_fullscreen_hint(false);
                }
            });
        }
        let Some(id) = self.session_in_pane(pane) else { return };
        let scale = self.device_scale();
        let Some(entry) = self.sessions.get(id) else { return };
        let Some(handle) = entry.handle.clone() else { return };
        if !entry.is_connected() || entry.remote_size.0 == 0 {
            return;
        }
        let (rw, rh) = entry.remote_size;
        let mode = effective_scale(entry.scale_mode, entry.connection.options.resolution);
        let rect = crate::rdp::input::view_rect(w, h, rw, rh, mode, scale);
        let (px, py) = crate::rdp::input::map_pointer(x, y, rect, rw, rh);
        handle.send_input(InputEvent::MouseMove { x: px, y: py });
        if kind == 0 || kind == 1 {
            let button = match button {
                0 => crate::rdp::input::MouseButton::Left,
                1 => crate::rdp::input::MouseButton::Right,
                2 => crate::rdp::input::MouseButton::Middle,
                3 => crate::rdp::input::MouseButton::X1,
                4 => crate::rdp::input::MouseButton::X2,
                _ => return,
            };
            handle.send_input(InputEvent::MouseButton {
                button,
                pressed: kind == 0,
            });
        }
    }

    fn on_scroll(&mut self, pane: usize, dx: f32, dy: f32) {
        let Some(id) = self.session_in_pane(pane) else { return };
        let Some(entry) = self.sessions.get_mut(id) else { return };
        if !entry.is_connected() {
            return;
        }
        let events = entry.wheel.feed(dx, dy);
        if let Some(handle) = &entry.handle {
            for ev in events {
                handle.send_input(ev);
            }
        }
    }

    fn on_capture(&mut self, pane: usize, focused: bool) {
        if focused {
            self.captured_pane = Some(pane);
        } else if self.captured_pane == Some(pane) {
            self.captured_pane = None;
        }
        self.sync_keyboard_target();
    }

    /// Leitet das Ziel der Tastatureingaben aus dem aktuellen Zustand ab: die Sitzung im Bereich
    /// mit Tastaturfokus, solange sie aktiv ist – mit ihrer *aktuellen* Verbindung. Wird nach
    /// jeder Änderung an Fokus, Bereichszuordnung oder Sitzungsstatus aufgerufen; sonst gingen
    /// Tasten nach einem Neuverbinden oder Tabwechsel an eine alte bzw. an gar keine Verbindung.
    fn sync_keyboard_target(&mut self) {
        let target = self
            .captured_pane
            .and_then(|pane| self.session_in_pane(pane))
            .and_then(|id| {
                let entry = self.sessions.get(id)?;
                if !entry.state.is_active() {
                    return None;
                }
                let handle = entry.handle.clone()?;
                Some(keyboard::KeyTarget { session: id, handle })
            });
        let captured = target.is_some();
        keyboard::set_target(target);
        self.with_ui(move |ui| ui.global::<Bridge>().set_rdp_captured(captured));
    }

    // ------------------------------------------------------------ Sitzungsereignisse

    fn process_events(&mut self) {
        let events = self.bus.drain();
        if events.is_empty() {
            return;
        }
        let mut structure_changed = false;
        for event in events {
            match event {
                AppEvent::Session(attempt, ev) => {
                    // Ereignisse veralteter Verbindungsversuche (nach Reconnect/Schließen) verwerfen
                    let Some(id) = self.sessions.resolve_attempt(attempt) else {
                        match ev {
                            SessionEvent::CertificatePrompt(p) => {
                                let _ = p.reply.send(CertDecision::Reject);
                            }
                            SessionEvent::Frame(f) => f.ack.done(),
                            _ => {}
                        }
                        continue;
                    };
                    if self.handle_session_event(id, ev) {
                        structure_changed = true;
                    }
                }
            }
        }
        if structure_changed {
            self.refresh_sessions_ui();
        }
    }

    /// Gibt `true` zurück, wenn Tabs/Status neu aufgebaut werden müssen.
    fn handle_session_event(&mut self, id: SessionId, event: SessionEvent) -> bool {
        match event {
            SessionEvent::Frame(frame) => {
                let image = Image::from_rgba8(frame.pixels);
                frame.ack.done();
                let known = self.sessions.get(id).is_some();
                if !known {
                    return false;
                }
                let first = !self.frames.contains_key(&id);
                self.frames.insert(id, image.clone());
                if let Some(pane) = self.sessions.pane_of(id)
                    && pane < self.panes_model.row_count()
                {
                    if first {
                        self.refresh_pane_of(id);
                    } else if let Some(mut item) = self.panes_model.row_data(pane) {
                        item.frame = image;
                        item.has_frame = true;
                        self.panes_model.set_row_data(pane, item);
                    }
                }
                false
            }
            SessionEvent::Cursor(update) => {
                let state = match update {
                    CursorUpdate::Default => CursorState::default(),
                    CursorUpdate::Hidden => CursorState {
                        mode: 1,
                        ..Default::default()
                    },
                    CursorUpdate::Bitmap(img) => CursorState {
                        mode: 2,
                        width: i32::from(img.width),
                        height: i32::from(img.height),
                        hx: i32::from(img.hotspot_x),
                        hy: i32::from(img.hotspot_y),
                        image: Image::from_rgba8(img.pixels),
                    },
                };
                self.cursors.insert(id, state);
                self.refresh_pane_of(id);
                false
            }
            SessionEvent::Resolution { width, height } => {
                if let Some(e) = self.sessions.get_mut(id) {
                    e.remote_size = (width, height);
                }
                self.refresh_pane_of(id);
                self.refresh_status_bar();
                false
            }
            SessionEvent::LoggedOn => {
                if let Some(e) = self.sessions.get_mut(id) {
                    e.logged_on = true;
                }
                false
            }
            SessionEvent::CertificatePrompt(prompt) => {
                self.pending_certs.push_back(prompt);
                if self.pending_certs.len() == 1 {
                    self.show_next_certificate();
                }
                false
            }
            SessionEvent::State(state) => {
                self.on_session_state(id, state);
                true
            }
        }
    }

    fn on_session_state(&mut self, id: SessionId, state: SessionState) {
        let Some(entry) = self.sessions.get_mut(id) else { return };
        tracing::info!(session = id, state = ?state.code(), "Sitzungsstatus");
        let was_connected = entry.ever_connected;
        match &state {
            SessionState::Connected => {
                if entry.connected_at.is_none() {
                    entry.connected_at = Some(Instant::now());
                }
                entry.ever_connected = true;
                entry.app_reconnects = 0;
            }
            SessionState::Disconnected { .. } | SessionState::Error { .. } => {
                entry.connected_at = None;
            }
            _ => {}
        }
        let terminal = matches!(state, SessionState::Disconnected { .. } | SessionState::Error { .. });
        entry.state = state.clone();

        // Anwendungsseitige Wiederverbindung nach unerwartetem Abbruch
        let auto = entry
            .connection
            .options
            .auto_reconnect
            .unwrap_or(self.settings.auto_reconnect);
        if let SessionState::Error { message } = &state
            && auto
            && was_connected
            && !entry.user_disconnected
            && entry.app_reconnects < self.settings.reconnect_attempts
            && !message.starts_with("Anmeldung fehlgeschlagen")
        {
            entry.app_reconnects += 1;
            let attempt = entry.app_reconnects;
            let max = self.settings.reconnect_attempts;
            entry.state = SessionState::Reconnecting { attempt, max };
            let delay = Duration::from_secs(u64::from(2 * attempt.min(5)));
            tracing::warn!(
                session = id,
                attempt,
                "Verbindung verloren – neuer Versuch in {:?}",
                delay
            );
            let timer = Timer::default();
            timer.start(TimerMode::SingleShot, delay, move || {
                with_app(move |s| {
                    let still = s
                        .sessions
                        .get(id)
                        .is_some_and(|e| matches!(e.state, SessionState::Reconnecting { .. }) && !e.user_disconnected);
                    if still {
                        s.reconnect(id, true);
                    }
                });
            });
            self.reconnect_timers.insert(id, timer);
        }

        if let SessionState::Error { message } = &state
            && self
                .sessions
                .get(id)
                .is_some_and(|e| matches!(e.state, SessionState::Error { .. }))
        {
            let name = self
                .sessions
                .get(id)
                .map(|e| e.connection.name.clone())
                .unwrap_or_default();
            self.toast(&format!("{name}: {message}"), 3);
        }
        if state == SessionState::Connected {
            // Neu verbunden: ggf. dynamische Auflösung an aktuelle Fläche anpassen
            if let Some(pane) = self.sessions.pane_of(id) {
                self.schedule_resize(id, pane);
            }
        }
        // Beendet → Tastatur freigeben; (Neu-)Aufbau → Tastatur an die aktuelle Verbindung
        self.sync_keyboard_target();
        if terminal {
            self.cursors.remove(&id);
            // Offene Zertifikatsabfragen dieser Sitzung sind hinfällig (Thread ist beendet)
        }
    }

    fn show_next_certificate(&mut self) {
        let next = self.pending_certs.front().map(|p| {
            (
                p.endpoint.clone(),
                p.fingerprint.clone(),
                p.previous_fingerprint.clone(),
                p.reason.clone(),
            )
        });
        self.with_ui(|ui| {
            let d = ui.global::<Dialogs>();
            match next {
                Some((endpoint, fp, prev, reason)) => {
                    d.set_cert_endpoint(endpoint.into());
                    d.set_cert_fingerprint(fp.into());
                    d.set_cert_changed(prev.is_some());
                    d.set_cert_previous(prev.unwrap_or_default().into());
                    d.set_cert_reason(reason.into());
                    d.set_certificate_open(true);
                }
                None => d.set_certificate_open(false),
            }
        });
    }

    // ------------------------------------------------------------ Dialoge

    fn confirm(&mut self, action: ConfirmAction, title: &str, message: &str, label: &str, danger: bool) {
        self.pending_confirm = Some(action);
        let (title, message, label): (SharedString, SharedString, SharedString) =
            (title.into(), message.into(), label.into());
        self.with_ui(|ui| {
            let d = ui.global::<Dialogs>();
            d.set_confirm_title(title);
            d.set_confirm_message(message);
            d.set_confirm_action(label);
            d.set_confirm_danger(danger);
            d.set_confirm_open(true);
        });
    }

    fn on_confirm(&mut self, accepted: bool) {
        self.with_ui(|ui| ui.global::<Dialogs>().set_confirm_open(false));
        let Some(action) = self.pending_confirm.take() else {
            return;
        };
        if !accepted {
            return;
        }
        match action {
            ConfirmAction::DeleteConnection(id) => {
                match self.conns.delete(id) {
                    Ok(()) => self.toast("Verbindung gelöscht.", 1),
                    Err(e) => self.toast(&format!("Löschen fehlgeschlagen: {e}"), 3),
                }
                self.refresh_connections();
            }
            ConfirmAction::DeleteFolder(id) => {
                if let Err(e) = self.conns.delete_folder(id) {
                    self.toast(&e.to_string(), 3);
                }
                self.refresh_connections();
            }
            ConfirmAction::CloseTab(index) => self.close_tab(index),
            ConfirmAction::InstallUpdate(path) => self.run_installer(&path),
            ConfirmAction::QuitApp => {
                self.quitting = true;
                let _ = slint::quit_event_loop();
            }
        }
    }

    fn text_dialog(&mut self, action: TextAction, title: &str, label: &str, value: &str, button: &str) {
        self.pending_text = Some(action);
        let (t, l, v, b): (SharedString, SharedString, SharedString, SharedString) =
            (title.into(), label.into(), value.into(), button.into());
        self.with_ui(|ui| {
            let d = ui.global::<Dialogs>();
            d.set_text_title(t);
            d.set_text_label(l);
            d.set_text_value(v);
            d.set_text_action(b);
            d.set_text_open(true);
        });
    }

    fn on_text_accept(&mut self, value: String) {
        let Some(action) = self.pending_text.take() else { return };
        self.with_ui(|ui| ui.global::<Dialogs>().set_text_open(false));
        let result = match action {
            TextAction::NewFolder(parent) => self.conns.db().create_folder(&value, parent).map(|_| ()),
            TextAction::RenameFolder(id) => self.conns.db().rename_folder(id, &value),
        };
        if let Err(e) = result {
            self.toast(&e.to_string(), 3);
        }
        self.refresh_connections();
    }

    fn open_editor(&mut self, id: Option<ConnectionId>, folder: Option<FolderId>) {
        let (conn, title) = match id.and_then(|i| self.connection(i).cloned()) {
            Some(c) => {
                let t = format!("„{}“ bearbeiten", c.name);
                (c, t)
            }
            None => {
                let mut c = Connection::new("", "");
                c.folder_id = folder;
                c.options.scale_mode = self.settings.default_scale_mode;
                c.options.resolution = self.settings.default_resolution;
                (c, "Neue Verbindung".to_owned())
            }
        };
        let stored = conn.id != 0 && self.conns.has_password(conn.id);
        let gw_stored = conn.id != 0 && self.conns.gateway_password(conn.id).is_some();
        let data = editor_from_connection(&conn, &self.folder_choices, stored, gw_stored);
        self.editor_base = Some(conn);
        self.with_ui(|ui| {
            let d = ui.global::<Dialogs>();
            d.set_editor(data);
            d.set_editor_title(title.into());
            d.set_editor_error(SharedString::default());
            d.set_editor_open(true);
        });
    }

    fn save_editor(&mut self, data: &EditorData) -> String {
        let base = self.editor_base.clone().unwrap_or_else(|| Connection::new("", ""));
        let conn = match apply_editor(data, &base, &self.folder_choices) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let password = match data.password_action {
            1 if !data.password.is_empty() => PasswordChange::Set(Secret::from(data.password.to_string())),
            2 => PasswordChange::Clear,
            _ => PasswordChange::Keep,
        };
        let gateway = if !data.gateway_password.is_empty() {
            PasswordChange::Set(Secret::from(data.gateway_password.to_string()))
        } else if !data.gateway_enabled {
            PasswordChange::Clear
        } else {
            PasswordChange::Keep
        };
        match self.conns.save(&conn, password, gateway) {
            Ok(_) => {
                self.editor_base = None;
                self.with_ui(|ui| ui.global::<Dialogs>().set_editor_open(false));
                self.toast(
                    if conn.id == 0 {
                        "Verbindung angelegt."
                    } else {
                        "Änderungen gespeichert."
                    },
                    1,
                );
                self.refresh_connections();
                // Offene Sitzungen dieser Verbindung erhalten die neuen Metadaten (Name/Farbe)
                if conn.id != 0 {
                    let updated = self.connection(conn.id).cloned();
                    if let Some(updated) = updated {
                        for idx in 0..self.sessions.len() {
                            let sid = self.sessions.sessions()[idx].id;
                            if let Some(e) = self.sessions.get_mut(sid)
                                && e.connection.id == updated.id
                            {
                                e.connection = updated.clone();
                            }
                        }
                        self.refresh_sessions_ui();
                    }
                }
                String::new()
            }
            Err(e) => e.to_string(),
        }
    }

    // ------------------------------------------------------------ Import / Export

    fn export_rdp(&mut self, id: ConnectionId) {
        let Some(conn) = self.connection(id).cloned() else {
            return;
        };
        let file_name = format!("{}.rdp", sanitize_file_name(&conn.name));
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Als RDP-Datei exportieren")
            .set_file_name(&file_name)
            .add_filter("Remotedesktopverbindung", &["rdp"]);
        let _ = slint::spawn_local(async move {
            if let Some(file) = dialog.save_file().await {
                let bytes = rdp_file::encode(&rdp_file::export(&conn));
                let path = file.path().to_path_buf();
                with_app(move |s| match std::fs::write(&path, bytes) {
                    Ok(()) => s.toast(&format!("Exportiert nach {}", path.display()), 1),
                    Err(e) => s.toast(&format!("Export fehlgeschlagen: {e}"), 3),
                });
            }
        });
    }

    /// Importiert `.rdp`-Dateien (ohne Duplikate gleicher Adresse) und verbindet die erste.
    fn open_rdp_files(&mut self, paths: Vec<std::path::PathBuf>) {
        let mut first = None;
        for path in paths {
            let name = path
                .file_stem()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parsed = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| rdp_file::import(&rdp_file::decode(&bytes), &name));
            let conn = match parsed {
                Ok(c) => c,
                Err(e) => {
                    self.toast(&format!("{}: {e}", path.display()), 3);
                    continue;
                }
            };
            let existing = self
                .connections
                .iter()
                .find(|c| c.host.eq_ignore_ascii_case(&conn.host) && c.port == conn.port && c.username == conn.username)
                .map(|c| c.id);
            let id = match existing {
                Some(id) => Some(id),
                None => match self.conns.db().insert_connection(&conn) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        self.toast(&e.to_string(), 3);
                        None
                    }
                },
            };
            self.refresh_connections();
            if first.is_none() {
                first = id;
            }
        }
        if let Some(id) = first {
            self.connect(id, None);
        }
    }

    fn import_rdp(&mut self) {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("RDP-Dateien importieren")
            .add_filter("Remotedesktopverbindung", &["rdp"]);
        let _ = slint::spawn_local(async move {
            let Some(files) = dialog.pick_files().await else { return };
            let paths: Vec<std::path::PathBuf> = files.iter().map(|f| f.path().to_path_buf()).collect();
            with_app(move |s| {
                let (mut ok, mut failed) = (0, Vec::new());
                for path in paths {
                    let name = path
                        .file_stem()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let result = std::fs::read(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|bytes| rdp_file::import(&rdp_file::decode(&bytes), &name))
                        .and_then(|c| s.conns.db().insert_connection(&c).map_err(|e| e.to_string()));
                    match result {
                        Ok(_) => ok += 1,
                        Err(e) => failed.push(format!("{name}: {e}")),
                    }
                }
                s.refresh_connections();
                if failed.is_empty() {
                    s.toast(&format!("{ok} Verbindung(en) importiert."), 1);
                } else {
                    s.toast(
                        &format!(
                            "{ok} importiert, {} fehlgeschlagen – {}",
                            failed.len(),
                            failed.join("; ")
                        ),
                        2,
                    );
                }
            });
        });
    }

    fn export_config(&mut self, encrypted: bool) {
        let stamp = chrono::Local::now().format("%Y-%m-%d");
        let name = if encrypted {
            format!("remora-vault-{stamp}.json")
        } else {
            format!("remora-export-{stamp}.json")
        };
        let dialog = rfd::AsyncFileDialog::new()
            .set_title(if encrypted {
                "Verschlüsselten Export speichern"
            } else {
                "Konfiguration exportieren"
            })
            .set_file_name(&name)
            .add_filter("JSON", &["json"]);
        let _ = slint::spawn_local(async move {
            let Some(file) = dialog.save_file().await else { return };
            let path = file.path().to_path_buf();
            with_app(move |s| {
                if encrypted {
                    s.pending_vault = Some(VaultAction::Export(path));
                    s.show_vault_dialog(
                        "Verschlüsselter Export",
                        "Der Export enthält alle gespeicherten Passwörter und wird mit dem Master-Passwort verschlüsselt.",
                        true,
                    );
                } else {
                    let result = config_export::export_plain(s.conns.db(), s.conns.credentials().as_ref())
                        .map_err(|e| e.to_string())
                        .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
                    match result {
                        Ok(()) => s.toast(
                            &format!("Konfiguration exportiert (ohne Passwörter): {}", path.display()),
                            1,
                        ),
                        Err(e) => s.toast(&format!("Export fehlgeschlagen: {e}"), 3),
                    }
                }
            });
        });
    }

    fn import_config(&mut self) {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Konfiguration importieren")
            .add_filter("JSON", &["json"]);
        let _ = slint::spawn_local(async move {
            let Some(file) = dialog.pick_file().await else { return };
            let path = file.path().to_path_buf();
            with_app(move |s| {
                let text = match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(e) => {
                        s.toast(&format!("Datei konnte nicht gelesen werden: {e}"), 3);
                        return;
                    }
                };
                if crate::security::vault::is_vault(&text) {
                    s.pending_vault = Some(VaultAction::Import(text));
                    s.show_vault_dialog(
                        "Verschlüsselten Export öffnen",
                        "Bitte das Master-Passwort des Exports eingeben.",
                        false,
                    );
                } else {
                    s.finish_import(&text, None);
                }
            });
        });
    }

    fn finish_import(&mut self, text: &str, master: Option<&str>) -> bool {
        match config_export::parse_bundle(text, master)
            .and_then(|b| config_export::import_bundle(self.conns.db(), self.conns.credentials().as_ref(), &b))
        {
            Ok(summary) => {
                self.toast(
                    &format!(
                        "Importiert: {} Ordner, {} Verbindungen, {} Passwörter.",
                        summary.folders, summary.connections, summary.passwords
                    ),
                    1,
                );
                self.refresh_connections();
                true
            }
            Err(e) => {
                if master.is_some() {
                    let msg: SharedString = e.to_string().into();
                    self.with_ui(|ui| ui.global::<Dialogs>().set_vault_error(msg));
                } else {
                    self.toast(&format!("Import fehlgeschlagen: {e}"), 3);
                }
                false
            }
        }
    }

    fn show_vault_dialog(&self, title: &str, message: &str, confirm: bool) {
        let (t, m): (SharedString, SharedString) = (title.into(), message.into());
        self.with_ui(|ui| {
            let d = ui.global::<Dialogs>();
            d.set_vault_title(t);
            d.set_vault_message(m);
            d.set_vault_confirm(confirm);
            d.set_vault_error(SharedString::default());
            d.set_vault_open(true);
        });
    }

    fn on_vault_accept(&mut self, password: Secret) {
        let Some(action) = self.pending_vault.take() else {
            return;
        };
        match action {
            VaultAction::Export(path) => {
                self.with_ui(|ui| ui.global::<Dialogs>().set_vault_open(false));
                let result =
                    config_export::export_vault(self.conns.db(), self.conns.credentials().as_ref(), password.expose())
                        .map_err(|e| e.to_string())
                        .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
                match result {
                    Ok(()) => self.toast(&format!("Verschlüsselter Export gespeichert: {}", path.display()), 1),
                    Err(e) => self.toast(&format!("Export fehlgeschlagen: {e}"), 3),
                }
            }
            VaultAction::Import(text) => {
                if self.finish_import(&text, Some(password.expose())) {
                    self.with_ui(|ui| ui.global::<Dialogs>().set_vault_open(false));
                } else {
                    // Erneut versuchen lassen
                    self.pending_vault = Some(VaultAction::Import(text));
                }
            }
        }
    }

    // ------------------------------------------------------------ Einstellungen

    fn on_settings_changed(&mut self, data: &SettingsData) {
        let old_autostart = self.settings.autostart;
        apply_settings_data(data, &mut self.settings);
        if self.settings.autostart != old_autostart
            && let Err(e) = autostart::set_enabled(self.settings.autostart)
        {
            self.settings.autostart = old_autostart;
            self.toast(&e.to_string(), 3);
            let d = settings_to_data(&self.settings);
            self.with_ui(|ui| ui.global::<Dialogs>().set_settings(d));
        }
        self.apply_theme();
        keyboard::set_policy(self.settings.windows_keys, self.fullscreen());
        self.save_settings();
    }

    fn on_close_requested(&mut self) -> bool {
        if self.quitting {
            return false;
        }
        let active = self.sessions.sessions().iter().filter(|s| s.is_connected()).count();
        if self.settings.confirm_close_active && active > 0 {
            self.confirm(
                ConfirmAction::QuitApp,
                "Remora beenden?",
                &format!(
                    "{active} aktive Sitzung(en) werden getrennt. Die Remote-Sitzungen bleiben auf den Servern bestehen, sofern sie nicht abgemeldet werden."
                ),
                "Beenden",
                false,
            );
            return true;
        }
        // Fenster wird geschlossen → Event-Loop beenden
        let _ = slint::quit_event_loop();
        false
    }
}

/// Bei "An Fenster anpassen" entspricht die Remote-Auflösung der Fläche → 1:1-Darstellung.
fn effective_scale(mode: ScaleMode, resolution: Resolution) -> ScaleMode {
    let _ = resolution;
    mode
}

fn sanitize_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.trim().is_empty() {
        "verbindung".into()
    } else {
        s.trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_file_name("  "), "verbindung");
    }
}
