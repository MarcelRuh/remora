//! Zwischenablage-Umleitung (CLIPRDR).
//!
//! Unter Windows wird die native Zwischenablage über `ironrdp-cliprdr-native::WinClipboard`
//! angebunden. Das Objekt besitzt ein unsichtbares Fenster und muss daher auf dem UI-Thread
//! (mit Nachrichtenschleife) erzeugt und dort auch wieder freigegeben werden. Die Nachrichten
//! werden über den Input-Kanal der jeweiligen Sitzung in die RDP-Sitzung eingespeist.

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use ironrdp::client::rdp::RdpInputSender;
    use ironrdp::cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy};

    /// Leitet Nachrichten der OS-Zwischenablage an die RDP-Sitzung weiter.
    #[derive(Clone, Debug)]
    pub struct ClipboardProxy {
        tx: RdpInputSender,
    }

    impl ClipboardProxy {
        pub fn new(tx: RdpInputSender) -> Self {
            Self { tx }
        }
    }

    impl ClipboardMessageProxy for ClipboardProxy {
        fn send_clipboard_message(&self, message: ClipboardMessage) {
            if self.tx.send_clipboard(message).is_err() {
                tracing::debug!("Zwischenablage-Nachricht verworfen: Sitzung ist beendet");
            }
        }
    }

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

    /// Fensterklasse der unsichtbaren Fenster von `WinClipboard` (eins je RDP-Sitzung).
    const CLIPBOARD_WINDOW_CLASS: &str = "IronRDPClipboardMonitor";
    const RELAY_SUBCLASS_ID: usize = 0x5245_4D43;
    const WM_CLIPBOARDUPDATE: u32 = 0x031D;
    const WM_ACTIVATEAPP: u32 = 0x001C;
    const WM_NCDESTROY: u32 = 0x0082;

    /// Reicht lokale Änderungen der Zwischenablage an die RDP-Sitzungen weiter, die
    /// `WinClipboard` verwirft.
    ///
    /// `WinClipboard` meldet eine lokale Änderung nur, solange Remora *nicht* die aktive Anwendung
    /// ist. Kopiert ein anderes Programm, während Remora vorn ist (Win+Umschalt+S, das Snipping
    /// Tool legt das Bild oft erst nach der Rückkehr zu Remora ab; Screenshot-Tools per Hotkey;
    /// Kopieren im SSH-Terminal), ginge der neue Inhalt nie an die Sitzung. Remora hört deshalb
    /// selbst auf Änderungen und stellt sie den Zwischenablage-Fenstern zu, als wäre Remora
    /// gerade inaktiv. Inhalte, die aus einer Sitzung stammen, werden nicht zurückgeschickt.
    pub fn install_local_change_relay(hwnd: isize) {
        use windows::Win32::System::DataExchange::AddClipboardFormatListener;
        use windows::Win32::UI::Shell::SetWindowSubclass;
        let h = HWND(hwnd as *mut _);
        // SAFETY: gültiges Fensterhandle des UI-Threads; die Prozedur bleibt für die Laufzeit gültig.
        let subclassed = unsafe { SetWindowSubclass(h, Some(relay_proc), RELAY_SUBCLASS_ID, 0) }.as_bool();
        // SAFETY: gültiges Fensterhandle des eigenen Threads.
        if !subclassed || unsafe { AddClipboardFormatListener(h) }.is_err() {
            tracing::error!("Weiterleitung lokaler Zwischenablage-Änderungen konnte nicht eingerichtet werden");
        }
    }

    unsafe extern "system" fn relay_proc(
        h: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        use windows::Win32::System::DataExchange::RemoveClipboardFormatListener;
        use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
        if msg == WM_CLIPBOARDUPDATE {
            relay_local_change();
        } else if msg == WM_NCDESTROY {
            // SAFETY: Abmelden beim Zerstören des eigenen Fensters.
            unsafe {
                let _ = RemoveClipboardFormatListener(h);
                let _ = RemoveWindowSubclass(h, Some(relay_proc), RELAY_SUBCLASS_ID);
            }
        }
        // SAFETY: Standardweiterleitung an die nächste Unterklasse bzw. Fensterprozedur.
        unsafe { DefSubclassProc(h, msg, wparam, lparam) }
    }

    fn relay_local_change() {
        use windows::Win32::System::DataExchange::GetClipboardOwner;
        // SAFETY: triviale API-Aufrufe ohne Vorbedingungen.
        let owner_class = unsafe { GetClipboardOwner() }.ok().map(class_name);
        if !needs_relay(remora_is_foreground(), owner_class.as_deref()) {
            return;
        }
        let windows = clipboard_windows();
        if windows.is_empty() {
            return;
        }
        tracing::debug!(
            sessions = windows.len(),
            "Lokale Zwischenablage-Änderung an Sitzungen weitergereicht"
        );
        for w in windows {
            // SAFETY: Fenster des eigenen Threads; `SendMessageW` ruft dessen Prozedur direkt auf.
            // `WinClipboard` wertet `WM_ACTIVATEAPP` nur als Aktiv-Kennzeichen aus; danach wird der
            // tatsächliche Zustand wiederhergestellt.
            unsafe {
                send(w, WM_ACTIVATEAPP, 0);
                send(w, WM_CLIPBOARDUPDATE, 0);
                send(w, WM_ACTIVATEAPP, usize::from(remora_is_foreground()));
            }
        }
    }

    /// Muss Remora die Änderung selbst weiterreichen? Nur wenn `WinClipboard` sie verwirft
    /// (Remora aktiv) und sie nicht aus einer Sitzung stammt.
    pub(crate) fn needs_relay(remora_foreground: bool, owner_class: Option<&str>) -> bool {
        remora_foreground && owner_class != Some(CLIPBOARD_WINDOW_CLASS)
    }

    unsafe fn send(w: HWND, msg: u32, wparam: usize) {
        use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
        let _ = unsafe { SendMessageW(w, msg, Some(WPARAM(wparam)), Some(LPARAM(0))) };
    }

    fn remora_is_foreground() -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        let mut pid = 0u32;
        // SAFETY: Ausgabeparameter ist beschreibbar; ohne Vordergrundfenster bleibt er 0.
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
        pid == std::process::id()
    }

    fn class_name(w: HWND) -> String {
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
        let mut buf = [0u16; 128];
        // SAFETY: Puffer ist beschreibbar, die Länge wird mitgegeben.
        let len = unsafe { GetClassNameW(w, &mut buf) };
        String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0)])
    }

    /// Die Zwischenablage-Fenster der Sitzungen (alle auf dem UI-Thread erzeugt).
    fn clipboard_windows() -> Vec<HWND> {
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::WindowsAndMessaging::EnumThreadWindows;
        unsafe extern "system" fn collect(w: HWND, out: LPARAM) -> windows::core::BOOL {
            if class_name(w) == CLIPBOARD_WINDOW_CLASS {
                // SAFETY: `out` zeigt auf den Vec aus dem Aufrufer, der während der Aufzählung lebt.
                unsafe { (*(out.0 as *mut Vec<HWND>)).push(w) };
            }
            true.into()
        }
        let mut found: Vec<HWND> = Vec::new();
        // SAFETY: Rückruf und Zeiger bleiben während des synchronen Aufrufs gültig.
        let _ = unsafe {
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(collect),
                LPARAM(&mut found as *mut Vec<HWND> as isize),
            )
        };
        found
    }

    /// Initialisiert OLE für den aktuellen (UI-)Thread. Mehrfache Aufrufe sind unschädlich.
    pub fn ensure_ole_initialized() -> anyhow::Result<()> {
        // SAFETY: OleInitialize darf pro Thread mehrfach aufgerufen werden (S_FALSE).
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }
            .map_err(|e| anyhow::anyhow!("OLE konnte nicht initialisiert werden: {}", e.message()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn relays_only_foreign_changes_while_remora_is_active() {
            // Remora inaktiv: WinClipboard meldet selbst.
            assert!(!needs_relay(false, Some("Chrome_WidgetWin_1")));
            // Remora aktiv, anderes Programm (oder Remoras SSH-Terminal) hat kopiert.
            assert!(needs_relay(true, Some("Chrome_WidgetWin_1")));
            assert!(needs_relay(true, None));
            // Inhalt stammt aus einer RDP-Sitzung: nicht zurückschicken.
            assert!(!needs_relay(true, Some(CLIPBOARD_WINDOW_CLASS)));
        }

        #[derive(Debug)]
        struct NoProxy;
        impl ClipboardMessageProxy for NoProxy {
            fn send_clipboard_message(&self, _message: ClipboardMessage) {}
        }

        /// Der Klassenname ist ein Detail von `ironrdp-cliprdr-native`: Dieser Test schlägt fehl,
        /// wenn ein IronRDP-Update ihn ändert und die Weiterleitung die Fenster nicht mehr findet.
        #[test]
        fn finds_the_window_of_win_clipboard() {
            assert!(clipboard_windows().is_empty());
            let clipboard = ironrdp_cliprdr_native::WinClipboard::new(NoProxy).unwrap();
            assert_eq!(clipboard_windows().len(), 1);
            drop(clipboard);
        }
    }
}
