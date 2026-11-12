//! Tastaturerfassung für Remote-Sitzungen.
//!
//! Slint liefert Tastenereignisse nur als Text, RDP benötigt dagegen Scancodes (layout-
//! unabhängig, inklusive Modifier, F-Tasten, Win-Taste …). Unter Windows wird daher – wie bei
//! mstsc – ein Low-Level-Tastatur-Hook (`WH_KEYBOARD_LL`) verwendet. Er ist nur aktiv, wenn
//!
//! 1. das Remora-Fenster im Vordergrund ist **und**
//! 2. eine Remote-Anzeige den Tastaturfokus besitzt.
//!
//! Dann werden die Scancodes an genau diese Sitzung gesendet und lokal verschluckt.
//! Lokale Kombinationen (Strg+Alt+Pos1/Pause/Bild↑/Bild↓/Ende) werden abgefangen und *nicht*
//! weitergeleitet. Strg+Alt+Entf kann von keiner Anwendung abgefangen werden (Secure
//! Attention Sequence); dafür gibt es Strg+Alt+Ende bzw. den Menüeintrag.

use std::cell::RefCell;
use std::rc::Rc;

use crate::rdp::{InputEvent, RdpSession, SessionId};
use crate::settings::WindowsKeyMode;

/// Lokale Aktion, ausgelöst während eine Sitzung die Tastatur besitzt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAction {
    ReleaseFocus,
    ToggleFullscreen,
    PreviousTab,
    NextTab,
    CtrlAltDel,
}

/// Aktuelles Ziel der Tastatureingaben.
pub struct KeyTarget {
    pub session: SessionId,
    pub handle: Rc<dyn RdpSession>,
}

struct HookState {
    hwnd: isize,
    target: Option<KeyTarget>,
    on_local: Box<dyn Fn(LocalAction)>,
    windows_keys: WindowsKeyMode,
    fullscreen: bool,
    ctrl: bool,
    alt: bool,
    /// Taste, deren Loslassen verschluckt werden muss (lokale Kombination).
    swallow_release_vk: Option<u32>,
    /// Das Fenster war beim letzten Ereignis im Vordergrund.
    was_foreground: bool,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// Entscheidung für einen Tastendruck (plattformunabhängig, testbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDecision {
    /// An das System/die lokale UI durchreichen.
    PassThrough,
    /// Als Scancode an die Sitzung senden und lokal verschlucken.
    Forward { scancode: u8, extended: bool },
    /// Lokale Aktion ausführen und verschlucken.
    Local(LocalAction),
    /// Verschlucken, nichts senden.
    Swallow,
}

pub mod vk {
    pub const TAB: u32 = 0x09;
    pub const CANCEL: u32 = 0x03; // Strg+Pause
    pub const PAUSE: u32 = 0x13;
    pub const ESCAPE: u32 = 0x1B;
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LWIN: u32 = 0x5B;
    pub const RWIN: u32 = 0x5C;
    pub const NUMLOCK: u32 = 0x90;
    pub const LSHIFT: u32 = 0xA0;
    pub const RSHIFT: u32 = 0xA1;
    pub const LCONTROL: u32 = 0xA2;
    pub const RCONTROL: u32 = 0xA3;
    pub const LMENU: u32 = 0xA4;
    pub const RMENU: u32 = 0xA5;
    pub const CONTROL: u32 = 0x11;
    pub const MENU: u32 = 0x12;
}

/// Entscheidet über einen Tastendruck. `ctrl`/`alt` geben den Zustand *vor* diesem Ereignis an.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    vk_code: u32,
    scan: u32,
    extended: bool,
    pressed: bool,
    ctrl: bool,
    alt: bool,
    windows_keys: WindowsKeyMode,
    fullscreen: bool,
) -> KeyDecision {
    if pressed && ctrl && alt {
        match vk_code {
            vk::HOME => return KeyDecision::Local(LocalAction::ReleaseFocus),
            vk::PAUSE | vk::CANCEL => return KeyDecision::Local(LocalAction::ToggleFullscreen),
            vk::PRIOR => return KeyDecision::Local(LocalAction::PreviousTab),
            vk::NEXT => return KeyDecision::Local(LocalAction::NextTab),
            vk::END => return KeyDecision::Local(LocalAction::CtrlAltDel),
            _ => {}
        }
    }

    let forward_system_keys = match windows_keys {
        WindowsKeyMode::Always => true,
        WindowsKeyMode::FullscreenOnly => fullscreen,
        WindowsKeyMode::Never => false,
    };
    if !forward_system_keys {
        let is_win = matches!(vk_code, vk::LWIN | vk::RWIN);
        let is_alt_tab = alt && matches!(vk_code, vk::TAB | vk::ESCAPE);
        let is_ctrl_esc = ctrl && vk_code == vk::ESCAPE;
        if is_win || is_alt_tab || is_ctrl_esc {
            return KeyDecision::PassThrough;
        }
    }

    // Sonderfälle der Scancode-Zuordnung
    let (code, ext) = match vk_code {
        vk::NUMLOCK => (0x45, false),
        // Pause benötigt eine Sonderfolge (E1 1D 45), die als einzelner Scancode nicht
        // darstellbar ist → Strg+NumLock-Äquivalent senden wäre falsch; verschlucken.
        vk::PAUSE => return KeyDecision::Swallow,
        _ => ((scan & 0xFF) as u8, extended),
    };
    if code == 0 {
        return KeyDecision::PassThrough;
    }
    let _ = pressed;
    KeyDecision::Forward {
        scancode: code,
        extended: ext,
    }
}

/// Installiert den Hook für das angegebene Fenster (nur Windows).
pub fn install(hwnd: isize, on_local: impl Fn(LocalAction) + 'static) {
    STATE.with(|s| {
        *s.borrow_mut() = Some(HookState {
            hwnd,
            target: None,
            on_local: Box::new(on_local),
            windows_keys: WindowsKeyMode::FullscreenOnly,
            fullscreen: false,
            ctrl: false,
            alt: false,
            swallow_release_vk: None,
            was_foreground: false,
        });
    });
    #[cfg(windows)]
    {
        platform::install_hook();
        platform::install_window_fallback(hwnd);
    }
}

/// Zerlegt eine Tastennachricht des Fensters (`WM_KEYDOWN` usw.) in
/// `(virtueller Code, Scancode, erweitert, gedrückt)`; `None` für andere Nachrichten.
pub fn key_from_message(msg: u32, wparam: usize, lparam: isize) -> Option<(u32, u32, bool, bool)> {
    const WM_KEYDOWN: u32 = 0x0100;
    const WM_KEYUP: u32 = 0x0101;
    const WM_SYSKEYDOWN: u32 = 0x0104;
    const WM_SYSKEYUP: u32 = 0x0105;
    let pressed = match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return None,
    };
    let bits = lparam as usize;
    Some((
        wparam as u32,
        ((bits >> 16) & 0xFF) as u32,
        bits & (1 << 24) != 0,
        pressed,
    ))
}

/// Setzt das Ziel der Tastatureingaben (`None` = lokal). Beim Wechsel werden auf der alten
/// Sitzung alle Tasten losgelassen und auf der neuen die Feststelltasten synchronisiert.
pub fn set_target(target: Option<KeyTarget>) {
    STATE.with(|s| {
        let mut guard = s.borrow_mut();
        let Some(state) = guard.as_mut() else { return };
        if same_target(state.target.as_ref(), target.as_ref()) {
            return;
        }
        if let Some(old) = state.target.take() {
            old.handle.send_input(InputEvent::ReleaseAll);
        }
        if let Some(new) = &target {
            let (caps, num, scroll) = lock_states();
            new.handle.send_input(InputEvent::SyncLocks { caps, num, scroll });
        }
        state.target = target;
        state.ctrl = false;
        state.alt = false;
    });
}

/// Gleiches Ziel = gleiche Sitzung **und** gleiche Verbindung. Nach einem Neuaufbau behält die
/// Sitzung ihre ID, bekommt aber eine neue Verbindung; die alte nimmt keine Eingaben mehr an.
fn same_target(a: Option<&KeyTarget>, b: Option<&KeyTarget>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.session == b.session && Rc::ptr_eq(&a.handle, &b.handle),
        (None, None) => true,
        _ => false,
    }
}

pub fn current_target() -> Option<SessionId> {
    STATE.with(|s| s.borrow().as_ref().and_then(|st| st.target.as_ref().map(|t| t.session)))
}

pub fn set_policy(windows_keys: WindowsKeyMode, fullscreen: bool) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.windows_keys = windows_keys;
            state.fullscreen = fullscreen;
        }
    });
}

/// Prüft periodisch, ob das Fenster den Vordergrund verloren hat, und lässt dann alle Tasten
/// der Zielsitzung los (verhindert "klemmende" Modifier nach Alt+Tab).
pub fn check_foreground() {
    #[cfg(windows)]
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            let fg = platform::foreground_window() == state.hwnd;
            if state.was_foreground && !fg {
                if let Some(t) = &state.target {
                    t.handle.send_input(InputEvent::ReleaseAll);
                }
                state.ctrl = false;
                state.alt = false;
            }
            state.was_foreground = fg;
        }
    });
}

fn lock_states() -> (bool, bool, bool) {
    #[cfg(windows)]
    {
        platform::lock_states()
    }
    #[cfg(not(windows))]
    {
        (false, false, false)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetForegroundWindow, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, SetWindowsHookExW,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    thread_local! {
        static HOOK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    }

    pub fn install_hook() {
        if HOOK.with(|h| h.get()) != 0 {
            return;
        }
        // SAFETY: Hook-Prozedur ist eine gültige `extern "system"`-Funktion; der Hook wird vom
        // installierenden (UI-)Thread über dessen Nachrichtenschleife bedient.
        let result = unsafe {
            let module = GetModuleHandleW(None).ok();
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module.map(|m| m.into()), 0)
        };
        match result {
            Ok(h) => HOOK.with(|c| c.set(h.0 as isize)),
            Err(e) => tracing::error!(error = %e, "Tastatur-Hook konnte nicht installiert werden"),
        }
    }

    const SUBCLASS_ID: usize = 0x5245_4b42; // "REKB"

    /// Zweiter Weg neben dem Hook: Tastennachrichten des Remora-Fensters selbst.
    ///
    /// Der Low-Level-Hook wird von Windows nicht in jeder Umgebung aufgerufen (beobachtet:
    /// Hook installiert, aber nie aufgerufen, während die Tasten normal im Fenster ankommen).
    /// Hat der Hook eine Taste bereits weitergeleitet, hat er sie verschluckt – sie erreicht das
    /// Fenster dann nicht, wird also nie doppelt gesendet. Systemkombinationen wie Win oder
    /// Alt+Tab kann nur der Hook abfangen.
    pub fn install_window_fallback(hwnd: isize) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::SetWindowSubclass;
        // SAFETY: gültiges Fensterhandle des UI-Threads; die Prozedur bleibt für die Laufzeit gültig.
        let ok = unsafe { SetWindowSubclass(HWND(hwnd as *mut _), Some(subclass_proc), SUBCLASS_ID, 0) };
        if !ok.as_bool() {
            tracing::error!("Tastatur-Weiterleitung über Fensternachrichten konnte nicht eingerichtet werden");
        }
    }

    unsafe extern "system" fn subclass_proc(
        h: windows::Win32::Foundation::HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
        const WM_NCDESTROY: u32 = 0x0082;
        if let Some((vk, scan, extended, pressed)) = key_from_message(msg, wparam.0, lparam.0)
            && handle(vk, scan, extended, pressed)
        {
            return LRESULT(0);
        }
        if msg == WM_NCDESTROY {
            // SAFETY: Entfernen der eigenen Unterklasse beim Zerstören des Fensters
            let _ = unsafe { RemoveWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID) };
        }
        // SAFETY: Standardweiterleitung an die nächste Unterklasse bzw. Fensterprozedur
        unsafe { DefSubclassProc(h, msg, wparam, lparam) }
    }

    pub fn foreground_window() -> isize {
        // SAFETY: trivialer API-Aufruf
        unsafe { GetForegroundWindow().0 as isize }
    }

    pub fn lock_states() -> (bool, bool, bool) {
        // SAFETY: trivialer API-Aufruf
        unsafe {
            (
                GetKeyState(0x14) & 1 != 0, // VK_CAPITAL
                GetKeyState(0x90) & 1 != 0, // VK_NUMLOCK
                GetKeyState(0x91) & 1 != 0, // VK_SCROLL
            )
        }
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: Für WH_KEYBOARD_LL zeigt lparam auf eine KBDLLHOOKSTRUCT.
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let pressed = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            if handle(kb.vkCode, kb.scanCode, (kb.flags & LLKHF_EXTENDED).0 != 0, pressed) {
                return LRESULT(1);
            }
        }
        // SAFETY: Weiterreichen an die nächste Hook-Prozedur
        unsafe { CallNextHookEx(Some(HHOOK(std::ptr::null_mut())), code, wparam, lparam) }
    }

    /// Gibt `true` zurück, wenn das Ereignis verschluckt werden soll.
    fn handle(vk_code: u32, scan: u32, extended: bool, pressed: bool) -> bool {
        let mut local_action = None;
        let swallow = STATE.with(|s| {
            // Kann während eines modalen Dialogs (Dateiauswahl) reentrant aufgerufen werden.
            let Ok(mut guard) = s.try_borrow_mut() else {
                return false;
            };
            let Some(state) = guard.as_mut() else { return false };
            if foreground_window() != state.hwnd {
                return false;
            }
            let Some(target) = &state.target else { return false };

            if !pressed && state.swallow_release_vk == Some(vk_code) {
                state.swallow_release_vk = None;
                return true;
            }

            let decision = decide(
                vk_code,
                scan,
                extended,
                pressed,
                state.ctrl,
                state.alt,
                state.windows_keys,
                state.fullscreen,
            );

            // Modifier-Zustand nachführen
            match vk_code {
                vk::LCONTROL | vk::RCONTROL | vk::CONTROL => state.ctrl = pressed,
                vk::LMENU | vk::RMENU | vk::MENU => state.alt = pressed,
                _ => {}
            }

            match decision {
                KeyDecision::PassThrough => false,
                KeyDecision::Swallow => true,
                KeyDecision::Forward { scancode, extended } => {
                    target.handle.send_input(InputEvent::Key {
                        scancode,
                        extended,
                        pressed,
                    });
                    true
                }
                KeyDecision::Local(action) => {
                    state.swallow_release_vk = Some(vk_code);
                    // Bereits gesendete Strg/Alt in der Sitzung wieder loslassen
                    target.handle.send_input(InputEvent::ReleaseAll);
                    if action == LocalAction::CtrlAltDel {
                        target.handle.send_input(InputEvent::CtrlAltDel);
                    }
                    local_action = Some(action);
                    true
                }
            }
        });
        if let Some(action) = local_action {
            // Außerhalb des Hooks ausführen (Hook muss schnell zurückkehren).
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                STATE.with(|s| {
                    if let Ok(guard) = s.try_borrow()
                        && let Some(state) = guard.as_ref()
                    {
                        (state.on_local)(action);
                    }
                });
            });
        }
        swallow
    }

    #[allow(dead_code)]
    pub(super) fn shift_vks() -> [u32; 2] {
        [vk::LSHIFT, vk::RSHIFT]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_key_messages_carry_scancode() {
        // WM_KEYDOWN 'A': Scancode 0x1E in Bits 16–23
        assert_eq!(
            key_from_message(0x0100, 0x41, 0x001E_0001),
            Some((0x41, 0x1E, false, true))
        );
        // WM_KEYUP rechte Strg-Taste: Scancode 0x1D, erweitert (Bit 24)
        assert_eq!(
            key_from_message(0x0101, 0x11, 0xC11D_0001_u32 as i32 as isize),
            Some((0x11, 0x1D, true, false))
        );
        // WM_SYSKEYDOWN Alt
        assert_eq!(
            key_from_message(0x0104, 0x12, 0x2038_0001),
            Some((0x12, 0x38, false, true))
        );
        // WM_CHAR ist keine Tastennachricht
        assert_eq!(key_from_message(0x0102, 0x61, 0x001E_0001), None);
    }

    struct NullSession;
    impl RdpSession for NullSession {
        fn send_input(&self, _: InputEvent) {}
        fn resize(&self, _: u16, _: u16, _: u32) {}
        fn set_visible(&self, _: bool) {}
        fn disconnect(&self) {}
    }

    #[test]
    fn reconnected_session_is_a_new_target() {
        let first: Rc<dyn RdpSession> = Rc::new(NullSession);
        let second: Rc<dyn RdpSession> = Rc::new(NullSession);
        let t = |session, handle: &Rc<dyn RdpSession>| KeyTarget {
            session,
            handle: handle.clone(),
        };
        assert!(same_target(Some(&t(1, &first)), Some(&t(1, &first))));
        // Gleiche Sitzungs-ID, neue Verbindung (Neuaufbau) → Ziel muss wechseln
        assert!(!same_target(Some(&t(1, &first)), Some(&t(1, &second))));
        assert!(!same_target(Some(&t(1, &first)), Some(&t(2, &first))));
        assert!(!same_target(Some(&t(1, &first)), None));
        assert!(same_target(None, None));
    }

    #[test]
    fn local_combos_are_intercepted() {
        let d = decide(vk::HOME, 0x47, true, true, true, true, WindowsKeyMode::Always, false);
        assert_eq!(d, KeyDecision::Local(LocalAction::ReleaseFocus));
        let d = decide(vk::END, 0x4F, true, true, true, true, WindowsKeyMode::Always, false);
        assert_eq!(d, KeyDecision::Local(LocalAction::CtrlAltDel));
        // Ohne Strg+Alt wird Pos1 weitergeleitet
        let d = decide(vk::HOME, 0x47, true, true, false, false, WindowsKeyMode::Always, false);
        assert_eq!(
            d,
            KeyDecision::Forward {
                scancode: 0x47,
                extended: true
            }
        );
    }

    #[test]
    fn windows_keys_policy() {
        // Nur im Vollbild: Win-Taste bleibt lokal
        assert_eq!(
            decide(
                vk::LWIN,
                0x5B,
                true,
                true,
                false,
                false,
                WindowsKeyMode::FullscreenOnly,
                false
            ),
            KeyDecision::PassThrough
        );
        assert_eq!(
            decide(
                vk::LWIN,
                0x5B,
                true,
                true,
                false,
                false,
                WindowsKeyMode::FullscreenOnly,
                true
            ),
            KeyDecision::Forward {
                scancode: 0x5B,
                extended: true
            }
        );
        // Alt+Tab bei "Nie" lokal
        assert_eq!(
            decide(vk::TAB, 0x0F, false, true, false, true, WindowsKeyMode::Never, true),
            KeyDecision::PassThrough
        );
        // Tab allein immer weiterleiten
        assert!(matches!(
            decide(vk::TAB, 0x0F, false, true, false, false, WindowsKeyMode::Never, false),
            KeyDecision::Forward { .. }
        ));
    }

    #[test]
    fn special_scancodes() {
        // NumLock kommt im Hook mit Extended-Flag, RDP erwartet 0x45 ohne
        assert_eq!(
            decide(
                vk::NUMLOCK,
                0x45,
                true,
                true,
                false,
                false,
                WindowsKeyMode::Always,
                false
            ),
            KeyDecision::Forward {
                scancode: 0x45,
                extended: false
            }
        );
        assert_eq!(
            decide(
                vk::PAUSE,
                0x45,
                false,
                true,
                false,
                false,
                WindowsKeyMode::Always,
                false
            ),
            KeyDecision::Swallow
        );
        // AltGr erzeugt ein künstliches LCtrl mit Scancode 0x21D → 0x1D
        assert_eq!(
            decide(
                vk::LCONTROL,
                0x21D,
                false,
                true,
                false,
                false,
                WindowsKeyMode::Always,
                false
            ),
            KeyDecision::Forward {
                scancode: 0x1D,
                extended: false
            }
        );
    }
}
