//! Kleine Plattform-Helfer.

/// Bevorzugt das System ein dunkles Design?
pub fn system_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
        use windows::core::w;
        let mut value: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: Puffergröße passt zum DWORD-Typ.
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut value as *mut u32).cast()),
                Some(&mut size),
            )
        };
        if result.is_ok() {
            return value == 0;
        }
        true
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Öffnet einen Ordner im Dateimanager.
pub fn open_folder(path: &std::path::Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = result {
        tracing::warn!(error = %e, "Ordner konnte nicht geöffnet werden");
    }
}

/// Dunkle Titelleiste (Windows 10 20H1+/11), passend zum Design.
pub fn set_dark_title_bar(hwnd: isize, dark: bool) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
        let value: i32 = i32::from(dark);
        // SAFETY: Gültiges Fensterhandle, Attributgröße passt.
        let _ = unsafe {
            DwmSetWindowAttribute(
                HWND(hwnd as *mut _),
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&value as *const i32).cast(),
                std::mem::size_of::<i32>() as u32,
            )
        };
    }
    #[cfg(not(windows))]
    let _ = (hwnd, dark);
}

/// Holt das Hauptfenster nach vorn (z. B. wenn eine zweite Instanz eine Datei übergibt).
pub fn bring_to_front(hwnd: isize) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
        let h = HWND(hwnd as *mut _);
        // SAFETY: gültiges Fensterhandle des eigenen Prozesses.
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
    }
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// Öffnet eine Web-Adresse im Standardbrowser.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        tracing::warn!(error = %e, "Adresse konnte nicht geöffnet werden");
    }
}

/// Text aus der Zwischenablage (`CF_UNICODETEXT`).
pub fn clipboard_text() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{HANDLE, HGLOBAL};
        use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
        use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        use windows::Win32::System::Ole::CF_UNICODETEXT;
        if !open_clipboard_retrying(|| unsafe { OpenClipboard(None) }.is_ok()) {
            return None;
        }
        // SAFETY: Zwischenablage ist geöffnet; der Speicher wird nur gelesen, solange er gesperrt ist.
        unsafe {
            let result = GetClipboardData(u32::from(CF_UNICODETEXT.0))
                .ok()
                .and_then(|HANDLE(h)| {
                    let global = HGLOBAL(h);
                    let ptr = GlobalLock(global) as *const u16;
                    if ptr.is_null() {
                        return None;
                    }
                    let max = GlobalSize(global) / 2;
                    let slice = std::slice::from_raw_parts(ptr, max);
                    let len = slice.iter().position(|&c| c == 0).unwrap_or(max);
                    let s = String::from_utf16_lossy(&slice[..len]);
                    let _ = GlobalUnlock(global);
                    Some(s)
                });
            let _ = CloseClipboard();
            result
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Schreibt Text in die Zwischenablage; `hwnd` wird ihr Besitzer (ohne Besitzer lehnen manche
/// Programme den Zugriff ab).
pub fn set_clipboard_text(hwnd: isize, text: &str) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::GlobalFree;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
        use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
        use windows::Win32::System::Ole::CF_UNICODETEXT;
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if !open_clipboard_retrying(|| {
            unsafe { OpenClipboard(Some(windows::Win32::Foundation::HWND(hwnd as *mut _))) }.is_ok()
        }) {
            return false;
        }
        // SAFETY: Zwischenablage ist geöffnet; der Speicherblock gehört nach erfolgreichem
        // SetClipboardData dem System, sonst wird er hier freigegeben.
        unsafe {
            let ok = (|| {
                EmptyClipboard().ok()?;
                let global = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
                let ptr = GlobalLock(global) as *mut u16;
                if ptr.is_null() {
                    let _ = GlobalFree(Some(global));
                    return None;
                }
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(global);
                if SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(global.0))).is_err() {
                    let _ = GlobalFree(Some(global));
                    return None;
                }
                Some(())
            })()
            .is_some();
            let _ = CloseClipboard();
            ok
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, text);
        false
    }
}

/// Andere Programme halten die Zwischenablage manchmal kurz geöffnet.
#[cfg(windows)]
fn open_clipboard_retrying(open: impl Fn() -> bool) -> bool {
    for _ in 0..10 {
        if open() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}
