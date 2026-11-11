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
