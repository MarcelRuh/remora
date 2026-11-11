//! Nur eine Instanz pro Windows-Sitzung.
//!
//! Die erste Instanz hält ein benanntes Mutex (`Local\…`, gilt pro Anmeldesitzung) und lauscht
//! auf einer lokalen Named Pipe. Jede weitere Instanz – z. B. durch Doppelklick auf eine
//! `.rdp`-Datei – schickt ihre Dateipfade an die erste Instanz und beendet sich. Die erste
//! Instanz holt daraufhin ihr Fenster nach vorn und öffnet die Dateien.
//!
//! Schutz: Die Pipe wird mit `FILE_FLAG_FIRST_PIPE_INSTANCE` und genau einer Instanz angelegt
//! (niemand kann sie vorher oder dazwischen belegen), lehnt Netzwerkzugriffe ab und der Sender
//! prüft, dass der Empfänger in derselben Windows-Sitzung läuft. Übertragen werden nur
//! Dateipfade, keine Zugangsdaten.

use std::path::PathBuf;

/// Kodiert Dateipfade als Nachricht (UTF-8, ein Pfad pro Zeile; leer = nur nach vorn holen).
pub fn encode(paths: &[PathBuf]) -> Vec<u8> {
    paths
        .iter()
        .filter_map(|p| p.to_str())
        .filter(|p| !p.contains('\n'))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// Gegenstück zu [`encode`]; ungültige Daten ergeben eine leere Liste.
pub fn decode(data: &[u8]) -> Vec<PathBuf> {
    std::str::from_utf8(data)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default()
}

/// Ergebnis des Starts.
pub enum Startup {
    /// Diese Instanz ist die erste; der Listener nimmt Nachrichten weiterer Instanzen an.
    Primary(Listener),
    /// Eine laufende Instanz hat die Dateien übernommen; dieser Prozess soll sich beenden.
    Forwarded,
}

/// Prüft, ob bereits eine Instanz läuft, und übergibt ihr ggf. `files`.
pub fn acquire(files: &[PathBuf]) -> Startup {
    imp::acquire(files)
}

pub use imp::Listener;

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_MORE_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_WRITE,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING,
        PIPE_ACCESS_INBOUND, ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId, PIPE_READMODE_MESSAGE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
    use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    use windows::core::{HSTRING, PCWSTR};

    use super::{Startup, decode, encode};

    const BUFFER: u32 = 64 * 1024;

    fn session_id() -> u32 {
        let mut id = 0u32;
        // SAFETY: reine Abfrage für den eigenen Prozess.
        let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) };
        id
    }

    fn pipe_name() -> HSTRING {
        HSTRING::from(format!(r"\\.\pipe\RemoraRDP-Instance-{}", session_id()))
    }

    /// Hält Mutex und Pipe der ersten Instanz für die Laufzeit des Prozesses.
    pub struct Listener {
        pipe: Option<HANDLE>,
    }

    // SAFETY: Pipe-Handles dürfen zwischen Threads weitergegeben werden.
    unsafe impl Send for Listener {}

    impl Listener {
        /// Startet den Empfangs-Thread. `on_message` läuft in diesem Thread.
        pub fn spawn(self, on_message: impl Fn(Vec<PathBuf>) + Send + 'static) {
            let Some(pipe) = self.pipe else { return };
            let pipe = pipe.0 as usize;
            let spawned = std::thread::Builder::new()
                .name("remora-instance".into())
                .spawn(move || {
                    let pipe = HANDLE(pipe as *mut _);
                    loop {
                        // SAFETY: gültiges, von diesem Thread exklusiv genutztes Pipe-Handle.
                        let connected = unsafe { ConnectNamedPipe(pipe, None) };
                        if let Err(e) = connected
                            && e.code() != ERROR_PIPE_CONNECTED.to_hresult()
                        {
                            tracing::warn!(error = %e, "Instanz-Pipe: Verbindung fehlgeschlagen");
                            std::thread::sleep(Duration::from_millis(200));
                            continue;
                        }
                        let message = read_message(pipe);
                        // SAFETY: trennt den Client, das Handle bleibt für die nächste Verbindung.
                        let _ = unsafe { DisconnectNamedPipe(pipe) };
                        if let Some(data) = message {
                            on_message(decode(&data));
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "Instanz-Thread konnte nicht gestartet werden");
            }
        }
    }

    fn read_message(pipe: HANDLE) -> Option<Vec<u8>> {
        let mut data = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let mut read = 0u32;
            // SAFETY: Puffer und Längenzeiger sind gültig; synchroner Lesevorgang.
            let result = unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut read), None) };
            data.extend_from_slice(&buf[..read as usize]);
            if data.len() > BUFFER as usize {
                return None;
            }
            match result {
                Ok(()) => return Some(data),
                Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => continue,
                Err(_) => return (!data.is_empty()).then_some(data),
            }
        }
    }

    pub fn acquire(files: &[PathBuf]) -> Startup {
        let mutex_name = HSTRING::from("Local\\RemoraRDP-SingleInstance");
        // SAFETY: Name ist gültig; das Handle bleibt bewusst bis Prozessende offen.
        let mutex = unsafe { CreateMutexW(None, false, &mutex_name) };
        // SAFETY: direkt nach CreateMutexW.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if mutex.is_err() {
            tracing::warn!("Instanz-Mutex nicht verfügbar, starte ohne Einzelinstanz");
            return Startup::Primary(Listener { pipe: None });
        }
        if existed {
            match forward(files) {
                Ok(()) => return Startup::Forwarded,
                Err(e) => tracing::warn!(error = %e, "Laufende Instanz nicht erreichbar, starte eigenständig"),
            }
            return Startup::Primary(Listener { pipe: None });
        }
        let name = pipe_name();
        // SAFETY: Parameter gültig; genau eine Instanz, erste Instanz erzwungen.
        let pipe = unsafe {
            CreateNamedPipeW(
                &name,
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_INBOUND.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0),
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                BUFFER,
                0,
                None,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            tracing::warn!("Instanz-Pipe konnte nicht angelegt werden");
            return Startup::Primary(Listener { pipe: None });
        }
        Startup::Primary(Listener { pipe: Some(pipe) })
    }

    fn forward(files: &[PathBuf]) -> anyhow::Result<()> {
        let name = pipe_name();
        let pcw = PCWSTR(name.as_ptr());
        // Die erste Instanz startet evtl. gerade erst: bis zu 5 s auf die Pipe warten.
        let deadline = Instant::now() + Duration::from_secs(5);
        let handle = loop {
            // SAFETY: gültiger Name; nur Schreibzugriff.
            match unsafe {
                CreateFileW(
                    pcw,
                    GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    Default::default(),
                    None,
                )
            } {
                Ok(h) => break h,
                Err(e) if Instant::now() < deadline => {
                    if e.code() == ERROR_PIPE_BUSY.to_hresult() {
                        // SAFETY: gültiger Name.
                        let _ = unsafe { WaitNamedPipeW(pcw, 1000) };
                    } else {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                Err(e) => anyhow::bail!("Pipe nicht erreichbar: {e}"),
            }
        };
        let result = (|| {
            let mut pid = 0u32;
            let mut server_session = u32::MAX;
            // SAFETY: gültiges Handle, Ausgabezeiger gültig.
            unsafe {
                GetNamedPipeServerProcessId(handle, &mut pid)?;
                ProcessIdToSessionId(pid, &mut server_session)?;
            }
            anyhow::ensure!(server_session == session_id(), "Pipe gehört zu einer anderen Sitzung");
            // Der ersten Instanz erlauben, ihr Fenster in den Vordergrund zu holen.
            // SAFETY: reiner Systemaufruf.
            let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
            let data = encode(files);
            let mut written = 0u32;
            // SAFETY: Puffer gültig, synchroner Schreibvorgang.
            unsafe { WriteFile(handle, Some(&data), Some(&mut written), None)? };
            Ok(())
        })();
        // SAFETY: Handle stammt von CreateFileW.
        let _ = unsafe { CloseHandle(handle) };
        result
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    use super::Startup;

    pub struct Listener;

    impl Listener {
        pub fn spawn(self, _on_message: impl Fn(Vec<PathBuf>) + Send + 'static) {}
    }

    pub fn acquire(_files: &[PathBuf]) -> Startup {
        Startup::Primary(Listener)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let paths = vec![PathBuf::from(r"C:\Daten\Server 1.rdp"), PathBuf::from(r"D:\ä\x.rdp")];
        assert_eq!(decode(&encode(&paths)), paths);
        assert!(decode(&encode(&[])).is_empty());
        assert!(decode(&[0xff, 0xfe]).is_empty());
    }
}
