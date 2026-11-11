//! Optionaler Autostart über `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

const VALUE_NAME: &str = "Remora RDP Manager";

#[cfg(windows)]
pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::core::HSTRING;

    let subkey = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let name = HSTRING::from(VALUE_NAME);
    let mut key = HKEY::default();
    // SAFETY: Gültige, nullterminierte Strings; `key` wird unten geschlossen.
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, None, KEY_SET_VALUE, &mut key) }.ok()?;

    let result = if enabled {
        let exe = std::env::current_exe()?;
        let value = format!("\"{}\"", exe.display());
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` lebt während des Aufrufs.
        let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
        unsafe { RegSetValueExW(key, &name, None, REG_SZ, Some(bytes)) }.ok()
    } else {
        match unsafe { RegDeleteValueW(key, &name) }.ok() {
            Ok(()) => Ok(()),
            // Nicht vorhanden ist kein Fehler.
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => Ok(()),
            Err(e) => Err(e),
        }
    };
    // SAFETY: `key` wurde erfolgreich geöffnet.
    unsafe {
        let _ = RegCloseKey(key);
    }
    result.map_err(|e| anyhow::anyhow!("Autostart konnte nicht geändert werden: {}", e.message()))
}

#[cfg(not(windows))]
pub fn set_enabled(_enabled: bool) -> anyhow::Result<()> {
    anyhow::bail!("Autostart ist auf dieser Plattform noch nicht implementiert")
}
