//! Update-Prüfung, Download und Installation (Oberflächenseite, siehe `services::update`).

use std::path::Path;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use super::{AppState, ConfirmAction, platform, with_app};
use crate::services::update::{self, UpdateInfo};
use crate::ui::Bridge;

impl AppState {
    fn set_update_status(&self, text: &str) {
        let text = text.to_owned();
        self.with_ui(move |ui| ui.global::<Bridge>().set_update_status(text.into()));
    }

    fn set_update_busy(&mut self, busy: bool) {
        self.update_busy = busy;
        self.with_ui(|ui| ui.global::<Bridge>().set_update_checking(busy));
    }

    /// Prüft im Hintergrund auf eine neuere Version. `manual`: Ergebnis auch anzeigen, wenn
    /// keine neuere Version existiert oder die Prüfung fehlschlägt.
    pub(super) fn start_update_check(&mut self, manual: bool) {
        if self.update_busy {
            return;
        }
        self.set_update_busy(true);
        self.set_update_status("");
        let spawned = std::thread::Builder::new().name("remora-update".into()).spawn(move || {
            let result = update::check(env!("CARGO_PKG_VERSION"));
            // Nur eine per Setup installierte Kopie kann sich selbst aktualisieren
            let installed = update::running_installed_copy();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(move |s| s.finish_update_check(result, installed, manual));
            });
        });
        if let Err(e) = spawned {
            self.finish_update_check(Err(e.into()), false, manual);
        }
    }

    fn finish_update_check(&mut self, result: anyhow::Result<Option<UpdateInfo>>, installed: bool, manual: bool) {
        self.set_update_busy(false);
        let checked_at = chrono::Local::now().format("%H:%M").to_string();
        match result {
            Ok(Some(info)) => {
                tracing::info!(version = %info.version, installable = info.installable(), "Neue Version verfügbar");
                let installable = installed && info.installable();
                let version = info.version.to_string();
                let status = if installable {
                    format!("Geprüft um {checked_at}. „Jetzt installieren“ lädt das Setup und prüft seine Prüfsumme.")
                } else if !installed {
                    format!(
                        "Geprüft um {checked_at}. Diese Kopie ist nicht per Setup installiert – Download über die Release-Seite."
                    )
                } else {
                    format!(
                        "Geprüft um {checked_at}. Das Release enthält kein Setup mit Prüfsumme – Download über die Release-Seite."
                    )
                };
                self.update_info = Some(info);
                self.with_ui(|ui| {
                    let b = ui.global::<Bridge>();
                    b.set_update_version(version.into());
                    b.set_update_installable(installable);
                });
                self.set_update_status(&status);
            }
            Ok(None) => {
                self.update_info = None;
                self.with_ui(|ui| {
                    let b = ui.global::<Bridge>();
                    b.set_update_version("".into());
                    b.set_update_installable(false);
                });
                self.set_update_status(&format!("Remora ist aktuell (geprüft um {checked_at})."));
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "Update-Prüfung fehlgeschlagen");
                if manual {
                    self.set_update_status(&format!("Prüfung fehlgeschlagen: {e}"));
                }
            }
        }
    }

    pub(super) fn open_update(&mut self) {
        if let Some(info) = &self.update_info {
            platform::open_url(&info.url);
        }
    }

    /// Lädt das Setup herunter, prüft es und fragt dann nach dem Start der Installation.
    pub(super) fn install_update(&mut self) {
        if self.update_busy {
            return;
        }
        let Some(info) = self.update_info.clone().filter(UpdateInfo::installable) else {
            self.open_update();
            return;
        };
        self.set_update_busy(true);
        self.set_update_status("Lade Update …");
        let dir = std::env::temp_dir().join("Remora-Update");
        let spawned = std::thread::Builder::new()
            .name("remora-update-dl".into())
            .spawn(move || {
                let mut last = Instant::now() - Duration::from_secs(1);
                let result = update::download(&info, &dir, |loaded, total| {
                    // Fortschritt höchstens fünfmal pro Sekunde melden
                    if last.elapsed() < Duration::from_millis(200) {
                        return;
                    }
                    last = Instant::now();
                    let text = match (loaded * 100).checked_div(total) {
                        Some(percent) => format!("Lade Update … {percent} %"),
                        None => format!("Lade Update … {} MB", loaded / (1024 * 1024)),
                    };
                    let _ = slint::invoke_from_event_loop(move || with_app(move |s| s.set_update_status(&text)));
                });
                let version = info.version.to_string();
                let _ = slint::invoke_from_event_loop(move || with_app(move |s| s.finish_download(result, version)));
            });
        if let Err(e) = spawned {
            self.finish_download(Err(e.into()), String::new());
        }
    }

    fn finish_download(&mut self, result: anyhow::Result<std::path::PathBuf>, version: String) {
        self.set_update_busy(false);
        match result {
            Ok(path) => {
                tracing::info!(file = %path.display(), "Update geladen und geprüft");
                self.set_update_status("Update geladen, Prüfsumme stimmt.");
                let active = self.sessions.len();
                let sessions = match active {
                    0 => String::new(),
                    1 => "\n\nDie offene Sitzung wird dabei getrennt.".to_owned(),
                    n => format!("\n\nDie {n} offenen Sitzungen werden dabei getrennt."),
                };
                self.confirm(
                    ConfirmAction::InstallUpdate(path),
                    &format!("Remora {version} installieren?"),
                    &format!(
                        "Remora wird beendet, das Setup installiert die neue Version (Windows fragt nach Administratorrechten) und startet Remora danach wieder.{sessions}"
                    ),
                    "Installieren",
                    false,
                );
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "Update-Download fehlgeschlagen");
                self.set_update_status(&format!("Update fehlgeschlagen: {e}"));
            }
        }
    }

    pub(super) fn run_installer(&mut self, path: &Path) {
        match update::launch_installer(path) {
            Ok(()) => {
                tracing::info!("Setup gestartet, Remora wird beendet");
                self.quitting = true;
                let _ = slint::quit_event_loop();
            }
            Err(e) => {
                // z. B. UAC-Abfrage abgelehnt
                tracing::warn!(error = %format!("{e:#}"), "Setup konnte nicht gestartet werden");
                self.set_update_status(&format!("Setup nicht gestartet: {e}"));
            }
        }
    }
}
