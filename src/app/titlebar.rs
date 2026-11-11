//! In das Design integrierte Titelleiste (Windows).
//!
//! Das Fenster behält seinen normalen Windows-Rahmen (Schatten, abgerundete Ecken, Größe
//! ändern, Aero Snap, Animationen). Über eine Fenster-Subclass wird nur der Beschriftungs-
//! bereich in die Client-Fläche verlegt (`WM_NCCALCSIZE`), dort zeichnet Slint die eigene
//! Leiste (`ui/components/titlebar.slint`). `WM_NCHITTEST` meldet die Fläche links der
//! Fenster-Buttons als `HTCAPTION`, damit Ziehen, Doppelklick und Systemmenü nativ bleiben.

/// Höhe der Titelleiste in logischen Pixeln (muss zu `titlebar.slint` passen).
pub const CAPTION_HEIGHT: f32 = 34.0;
/// Breite der drei Fenster-Buttons in logischen Pixeln (3 × 46 px).
pub const BUTTONS_WIDTH: f32 = 3.0 * 46.0;

/// Zustandsänderungen des Fensters, die die Titelleiste anzeigt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    Maximized(bool),
    Active(bool),
}

/// Ergebnis der Trefferprüfung innerhalb der Client-Fläche.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Client,
    Caption,
    TopBorder,
}

/// Reine Trefferlogik (testbar): `x`/`y` in physischen Client-Koordinaten.
pub fn hit_test(x: i32, y: i32, client_width: i32, scale: f32, frame: i32, maximized: bool) -> Hit {
    if y < 0 {
        return Hit::Client;
    }
    if !maximized && y < frame {
        return Hit::TopBorder;
    }
    if (y as f32) < CAPTION_HEIGHT * scale {
        if (x as f32) >= client_width as f32 - BUTTONS_WIDTH * scale {
            Hit::Client
        } else {
            Hit::Caption
        }
    } else {
        Hit::Client
    }
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
        HTTOPRIGHT, IsZoomed, NCCALCSIZE_PARAMS, PostMessageW, SM_CXPADDEDBORDER, SM_CYSIZEFRAME, SW_MAXIMIZE,
        SW_MINIMIZE, SW_RESTORE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
        ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_SIZE,
    };

    use super::{Hit, WindowEvent, hit_test};

    const SUBCLASS_ID: usize = 0x5245_4d54; // "REMT"
    const SIZE_MINIMIZED: usize = 1;
    const SIZE_MAXIMIZED: usize = 2;

    /// Aus im Vollbild: Dann gibt es weder Titelleiste noch Ziehfläche.
    static ENABLED: AtomicBool = AtomicBool::new(false);

    type EventCallback = Box<dyn Fn(WindowEvent)>;

    thread_local! {
        static CALLBACK: RefCell<Option<EventCallback>> = const { RefCell::new(None) };
    }

    fn hwnd(h: isize) -> HWND {
        HWND(h as *mut _)
    }

    fn notify(event: WindowEvent) {
        CALLBACK.with(|c| {
            if let Some(cb) = c.borrow().as_ref() {
                cb(event);
            }
        });
    }

    /// Dicke des Größenrahmens (oben) in physischen Pixeln.
    fn frame_thickness(h: HWND) -> i32 {
        // SAFETY: Gültiges Fensterhandle; reine Abfragen.
        unsafe {
            let dpi = GetDpiForWindow(h);
            GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
    }

    fn refresh_frame(h: HWND) {
        // SAFETY: Löst nur eine Neuberechnung des Rahmens aus.
        let _ = unsafe {
            SetWindowPos(
                h,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    unsafe extern "system" fn subclass_proc(
        h: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        // SAFETY (gesamte Funktion): Aufruf durch Windows mit gültigen Nachrichtenparametern.
        unsafe {
            match msg {
                WM_NCCALCSIZE if wparam.0 != 0 && ENABLED.load(Ordering::Relaxed) => {
                    let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                    let top = params.rgrc[0].top;
                    let result = DefSubclassProc(h, msg, wparam, lparam);
                    // Seiten und unten behält der Standardrahmen, oben entfällt die Beschriftung.
                    // Maximierte Fenster ragen um die Rahmendicke über den Bildschirm hinaus.
                    let inset = if IsZoomed(h).as_bool() { frame_thickness(h) } else { 0 };
                    params.rgrc[0].top = top + inset;
                    return result;
                }
                WM_NCHITTEST if ENABLED.load(Ordering::Relaxed) => {
                    let base = DefSubclassProc(h, msg, wparam, lparam);
                    let code = base.0 as u32;
                    if [
                        HTLEFT,
                        HTRIGHT,
                        HTBOTTOM,
                        HTBOTTOMLEFT,
                        HTBOTTOMRIGHT,
                        HTTOPLEFT,
                        HTTOPRIGHT,
                    ]
                    .contains(&code)
                    {
                        return base;
                    }
                    let mut pt = POINT {
                        x: (lparam.0 & 0xffff) as i16 as i32,
                        y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                    };
                    let _ = ScreenToClient(h, &mut pt);
                    let mut rc = RECT::default();
                    let _ = GetClientRect(h, &mut rc);
                    if pt.x < 0 || pt.x >= rc.right || pt.y >= rc.bottom {
                        return base;
                    }
                    let scale = GetDpiForWindow(h) as f32 / 96.0;
                    let hit = hit_test(pt.x, pt.y, rc.right, scale, frame_thickness(h), IsZoomed(h).as_bool());
                    return LRESULT(match hit {
                        Hit::TopBorder => HTTOP,
                        Hit::Caption => HTCAPTION,
                        Hit::Client => HTCLIENT,
                    } as isize);
                }
                WM_SIZE => match wparam.0 {
                    SIZE_MINIMIZED => {}
                    SIZE_MAXIMIZED => notify(WindowEvent::Maximized(true)),
                    _ => notify(WindowEvent::Maximized(false)),
                },
                WM_ACTIVATE => notify(WindowEvent::Active((wparam.0 & 0xffff) as u32 != WA_INACTIVE)),
                WM_NCDESTROY => {
                    let _ = RemoveWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID);
                    ENABLED.store(false, Ordering::Relaxed);
                }
                _ => {}
            }
            DefSubclassProc(h, msg, wparam, lparam)
        }
    }

    pub fn install(handle: isize, on_event: Box<dyn Fn(WindowEvent)>) -> bool {
        let h = hwnd(handle);
        CALLBACK.with(|c| *c.borrow_mut() = Some(on_event));
        // SAFETY: Fensterhandle gehört zu diesem Thread (UI-Thread).
        let ok = unsafe { SetWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID, 0) }.as_bool();
        if ok {
            ENABLED.store(true, Ordering::Relaxed);
            refresh_frame(h);
            // SAFETY: reine Abfrage.
            notify(WindowEvent::Maximized(unsafe { IsZoomed(h) }.as_bool()));
        }
        ok
    }

    pub fn set_enabled(handle: isize, enabled: bool, refresh: bool) {
        ENABLED.store(enabled, Ordering::Relaxed);
        if refresh {
            refresh_frame(hwnd(handle));
        }
    }

    pub fn minimize(handle: isize) {
        // SAFETY: gültiges Fensterhandle.
        let _ = unsafe { ShowWindow(hwnd(handle), SW_MINIMIZE) };
    }

    pub fn toggle_maximize(handle: isize) {
        let h = hwnd(handle);
        // SAFETY: gültiges Fensterhandle.
        unsafe {
            let cmd = if IsZoomed(h).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
            let _ = ShowWindow(h, cmd);
        }
    }

    pub fn close(handle: isize) {
        // Geht denselben Weg wie das Schließen über Alt+F4 (inkl. Rückfrage bei aktiven Sitzungen).
        // SAFETY: gültiges Fensterhandle.
        let _ = unsafe { PostMessageW(Some(hwnd(handle)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

#[cfg(windows)]
pub use imp::{close, install, minimize, set_enabled, toggle_maximize};

#[cfg(not(windows))]
pub fn install(_handle: isize, _on_event: Box<dyn Fn(WindowEvent)>) -> bool {
    false
}
#[cfg(not(windows))]
pub fn set_enabled(_handle: isize, _enabled: bool, _refresh: bool) {}
#[cfg(not(windows))]
pub fn minimize(_handle: isize) {}
#[cfg(not(windows))]
pub fn toggle_maximize(_handle: isize) {}
#[cfg(not(windows))]
pub fn close(_handle: isize) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_area_and_buttons() {
        // 100 % Skalierung, 1000 px breit, Rahmen 8 px
        assert_eq!(hit_test(10, 3, 1000, 1.0, 8, false), Hit::TopBorder);
        assert_eq!(hit_test(10, 3, 1000, 1.0, 8, true), Hit::Caption);
        assert_eq!(hit_test(500, 20, 1000, 1.0, 8, false), Hit::Caption);
        assert_eq!(hit_test(1000 - 138, 20, 1000, 1.0, 8, false), Hit::Client);
        assert_eq!(hit_test(999, 20, 1000, 1.0, 8, false), Hit::Client);
        assert_eq!(hit_test(500, 34, 1000, 1.0, 8, false), Hit::Client);
    }

    #[test]
    fn caption_scales_with_dpi() {
        // 150 %: Leiste 51 px hoch, Buttons 207 px breit
        assert_eq!(hit_test(500, 50, 1500, 1.5, 11, false), Hit::Caption);
        assert_eq!(hit_test(500, 51, 1500, 1.5, 11, false), Hit::Client);
        assert_eq!(hit_test(1500 - 208, 30, 1500, 1.5, 11, false), Hit::Caption);
        assert_eq!(hit_test(1500 - 207, 30, 1500, 1.5, 11, false), Hit::Client);
    }
}
