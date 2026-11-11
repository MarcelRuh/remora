//! Sichere Ablage von Zugangsdaten im Credential-Store des Betriebssystems.
//!
//! Unter Windows wird der *Windows Credential Manager* (Generic Credentials) verwendet.
//! Passwörter verlassen diesen Store nur als [`Secret`], das seinen Inhalt beim Drop
//! überschreibt. Sie werden weder in SQLite noch in Konfigurationsdateien oder Logs geschrieben.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use zeroize::{Zeroize, Zeroizing};

use crate::database::ConnectionId;

/// Präfix aller Einträge im Credential Manager.
pub const TARGET_PREFIX: &str = "Remora RDP";

/// Ein geheimer Wert, der beim Freigeben genullt wird und nie im Debug-Output erscheint.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl From<String> for Secret {
    fn from(mut value: String) -> Self {
        let s = Self::new(value.as_str());
        value.zeroize();
        s
    }
}

/// Welcher geheime Wert einer Verbindung gemeint ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialKind {
    Password,
    GatewayPassword,
    /// Passwort für den Proxora-Import (nicht an eine Verbindung gebunden).
    ProxoraPassword,
}

/// Schlüssel eines gespeicherten Geheimnisses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CredentialKey {
    pub connection: ConnectionId,
    pub kind: CredentialKind,
}

impl CredentialKey {
    pub fn password(connection: ConnectionId) -> Self {
        Self {
            connection,
            kind: CredentialKind::Password,
        }
    }

    pub fn gateway(connection: ConnectionId) -> Self {
        Self {
            connection,
            kind: CredentialKind::GatewayPassword,
        }
    }

    /// Zugang zur Proxora-Instanz (Einstellungen → Proxora).
    pub fn proxora() -> Self {
        Self {
            connection: 0,
            kind: CredentialKind::ProxoraPassword,
        }
    }

    /// Ziel-Name im Credential Manager, z. B. `Remora RDP/connection/12/password`.
    pub fn target_name(&self) -> String {
        match self.kind {
            CredentialKind::Password => format!("{TARGET_PREFIX}/connection/{}/password", self.connection),
            CredentialKind::GatewayPassword => {
                format!("{TARGET_PREFIX}/connection/{}/gateway-password", self.connection)
            }
            CredentialKind::ProxoraPassword => format!("{TARGET_PREFIX}/proxora/password"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("Credential-Store nicht verfügbar: {0}")]
    Unavailable(String),
    #[error("Zugriff auf den Credential-Store fehlgeschlagen: {0}")]
    Backend(String),
}

/// Abstraktion des Credential-Stores (austauschbar, z. B. für Tests oder andere Plattformen).
pub trait CredentialStore: Send + Sync {
    fn get(&self, key: CredentialKey) -> Result<Option<Secret>, CredentialError>;
    /// `username` wird nur als Metadatum im Store abgelegt (Anzeige im Credential Manager).
    fn set(&self, key: CredentialKey, username: &str, secret: &Secret) -> Result<(), CredentialError>;
    fn delete(&self, key: CredentialKey) -> Result<(), CredentialError>;

    fn exists(&self, key: CredentialKey) -> bool {
        matches!(self.get(key), Ok(Some(_)))
    }
}

/// In-Memory-Store für Tests und als Fallback auf Plattformen ohne nativen Store.
#[derive(Default)]
pub struct MemoryCredentialStore {
    entries: Mutex<HashMap<CredentialKey, Secret>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, key: CredentialKey) -> Result<Option<Secret>, CredentialError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned())
    }

    fn set(&self, key: CredentialKey, _username: &str, secret: &Secret) -> Result<(), CredentialError> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, secret.clone());
        Ok(())
    }

    fn delete(&self, key: CredentialKey) -> Result<(), CredentialError> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        Ok(())
    }
}

/// Liefert den Standard-Store der Plattform.
pub fn platform_store() -> Box<dyn CredentialStore> {
    #[cfg(windows)]
    {
        Box::new(windows_store::WindowsCredentialStore)
    }
    #[cfg(not(windows))]
    {
        tracing::warn!(
            "Kein nativer Credential-Store implementiert; Passwörter werden nur für die Laufzeit im Speicher gehalten"
        );
        Box::new(MemoryCredentialStore::default())
    }
}

#[cfg(windows)]
mod windows_store {
    use super::*;
    use windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME, GetLastError};
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
        CredWriteW,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    pub struct WindowsCredentialStore;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl CredentialStore for WindowsCredentialStore {
        fn get(&self, key: CredentialKey) -> Result<Option<Secret>, CredentialError> {
            let target = HSTRING::from(key.target_name());
            let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `target` lebt bis zum Ende des Aufrufs; `pcred` wird von der API befüllt
            // und unten mit `CredFree` freigegeben.
            let result = unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut pcred) };
            if let Err(err) = result {
                if err.code() == ERROR_NOT_FOUND.to_hresult() {
                    return Ok(None);
                }
                return Err(CredentialError::Backend(err.message().to_string()));
            }
            // SAFETY: Bei Erfolg zeigt `pcred` auf eine gültige CREDENTIALW-Struktur.
            let secret = unsafe {
                let cred = &*pcred;
                let len = cred.CredentialBlobSize as usize;
                let bytes = std::slice::from_raw_parts(cred.CredentialBlob, len);
                // Gespeichert als UTF-16LE ohne Nullterminator.
                let mut units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let value = String::from_utf16(&units);
                units.zeroize();
                CredFree(pcred as *const _);
                value
            };
            match secret {
                Ok(s) => Ok(Some(Secret::from(s))),
                Err(_) => Err(CredentialError::Backend(
                    "Ungültige Kodierung im Credential Manager".into(),
                )),
            }
        }

        fn set(&self, key: CredentialKey, username: &str, secret: &Secret) -> Result<(), CredentialError> {
            let mut target = wide(&key.target_name());
            let mut user = wide(username);
            let mut comment = wide("Gespeichert von Remora RDP Manager");
            let mut blob: Zeroizing<Vec<u8>> =
                Zeroizing::new(secret.expose().encode_utf16().flat_map(u16::to_le_bytes).collect());
            if blob.len() > 2560 {
                return Err(CredentialError::Backend("Passwort ist zu lang".into()));
            }
            let cred = CREDENTIALW {
                Flags: CRED_FLAGS(0),
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(target.as_mut_ptr()),
                Comment: PWSTR(comment.as_mut_ptr()),
                LastWritten: FILETIME::default(),
                CredentialBlobSize: blob.len() as u32,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: PWSTR::null(),
                UserName: PWSTR(user.as_mut_ptr()),
            };
            // SAFETY: Alle Zeiger in `cred` verweisen auf lokale Puffer, die den Aufruf überdauern.
            unsafe { CredWriteW(&cred, 0) }.map_err(|e| CredentialError::Backend(e.message().to_string()))
        }

        fn delete(&self, key: CredentialKey) -> Result<(), CredentialError> {
            let target = HSTRING::from(key.target_name());
            // SAFETY: `target` ist ein gültiger, nullterminierter String.
            match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
                Ok(()) => Ok(()),
                Err(err) if err.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
                Err(err) => {
                    // SAFETY: trivial
                    let _ = unsafe { GetLastError() };
                    Err(CredentialError::Backend(err.message().to_string()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn memory_store_roundtrip() {
        let store = MemoryCredentialStore::default();
        let key = CredentialKey::password(7);
        assert!(store.get(key).unwrap().is_none());
        store.set(key, "admin", &Secret::new("pw")).unwrap();
        assert_eq!(store.get(key).unwrap().unwrap().expose(), "pw");
        assert!(store.exists(key));
        assert!(!store.exists(CredentialKey::gateway(7)));
        store.delete(key).unwrap();
        assert!(!store.exists(key));
    }

    #[test]
    fn target_names_are_distinct() {
        assert_ne!(
            CredentialKey::password(1).target_name(),
            CredentialKey::gateway(1).target_name()
        );
        assert_eq!(
            CredentialKey::password(5).target_name(),
            "Remora RDP/connection/5/password"
        );
    }
}
