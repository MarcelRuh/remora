//! Proxora-Import (Einstellungen → Proxora): VMs laden, auswählen, als Verbindungen anlegen.

use std::collections::HashMap;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{AppState, ConfirmAction, TextAction, with_app};
use crate::core::connection_manager::PasswordChange;
use crate::database::{Connection, FolderId};
use crate::security::credentials::{CredentialKey, Secret};
use crate::services::proxora::{self, ProxoraError, ProxoraRequest, ProxoraVm, TotpAnswer};
use crate::ui::{Bridge, ProxoraVmItem};

/// Name des Ordners, in dem importierte VMs landen.
const ROOT_FOLDER: &str = "Proxora";

impl AppState {
    /// Anfangszustand der Proxora-Seite.
    pub(super) fn proxora_init_ui(&self) {
        let stored = self.conns.credentials().exists(CredentialKey::proxora());
        let fp = self.settings.proxora_fingerprint.clone();
        self.with_ui(|ui| {
            let b = ui.global::<Bridge>();
            b.set_proxora_password_stored(stored);
            b.set_proxora_fingerprint(fp.into());
        });
    }

    fn proxora_status(&self, text: &str) {
        let text = text.to_owned();
        self.with_ui(move |ui| ui.global::<Bridge>().set_proxora_status(text.into()));
    }

    /// Meldet sich an Proxora an und lädt die VM-Liste (im Hintergrund).
    pub(super) fn proxora_load(&mut self, totp: Option<TotpAnswer>) {
        if self.proxora_busy {
            return;
        }
        let mut typed = String::new();
        self.with_ui(|ui| typed = ui.global::<Bridge>().get_proxora_password().to_string());
        let password = if !typed.is_empty() {
            Some(Secret::from(typed))
        } else {
            self.conns.credentials().get(CredentialKey::proxora()).ok().flatten()
        };
        let Some(password) = password.or_else(|| totp.as_ref().map(|_| Secret::new(""))) else {
            self.proxora_status("Bitte das Proxora-Passwort eingeben.");
            return;
        };
        let request = ProxoraRequest {
            url: self.settings.proxora_url.clone(),
            username: self.settings.proxora_username.clone(),
            password,
            totp,
            pinned_fingerprint: self.settings.proxora_fingerprint.clone(),
        };
        let insecure = request.url.trim_start().to_ascii_lowercase().starts_with("http://");
        self.proxora_busy = true;
        self.with_ui(|ui| ui.global::<Bridge>().set_proxora_busy(true));
        self.proxora_status("Verbinde mit Proxora …");
        let spawned = std::thread::Builder::new()
            .name("remora-proxora".into())
            .spawn(move || {
                let result = proxora::fetch_vms(&request);
                let _ = slint::invoke_from_event_loop(move || with_app(move |s| s.proxora_finished(result, insecure)));
            });
        if let Err(e) = spawned {
            self.proxora_finished(Err(ProxoraError::Other(e.to_string())), insecure);
        }
    }

    fn proxora_finished(&mut self, result: Result<Vec<ProxoraVm>, ProxoraError>, insecure: bool) {
        self.proxora_busy = false;
        self.with_ui(|ui| ui.global::<Bridge>().set_proxora_busy(false));
        match result {
            Ok(vms) => {
                self.proxora_save_typed_password();
                let with_ip = vms.iter().filter(|v| v.preferred_address().is_some()).count();
                let total = vms.len();
                // Bereits importierte VMs vorauswählen (Import aktualisiert dann ihre Adressen)
                self.proxora_selected = vms
                    .iter()
                    .map(ProxoraVm::marker)
                    .filter(|m| self.proxora_imported(m))
                    .collect();
                self.proxora_vms = vms;
                self.proxora_refresh_list();
                let mut status = format!("{total} VMs gefunden, {with_ip} davon mit IP-Adresse.");
                if insecure {
                    status.push_str(" Achtung: Verbindung zu Proxora unverschlüsselt (http).");
                }
                self.proxora_status(&status);
            }
            Err(ProxoraError::UntrustedCertificate { fingerprint }) => {
                let previous = self.settings.proxora_fingerprint.clone();
                let changed = !previous.is_empty();
                let message = if changed {
                    format!(
                        "Das Zertifikat von Proxora hat sich geändert!\n\nBisher: {previous}\nJetzt:  {fingerprint}\n\nNur bestätigen, wenn das Zertifikat auf dem Proxora-Server bewusst erneuert wurde."
                    )
                } else {
                    format!(
                        "Proxora verwendet ein Zertifikat, das Windows nicht kennt (z. B. selbstsigniert).\n\nSHA-256: {fingerprint}\n\nNur bestätigen, wenn der Fingerabdruck mit dem Zertifikat deines Proxora-Servers übereinstimmt. Danach wird ausschließlich dieses Zertifikat akzeptiert."
                    )
                };
                self.proxora_status("Zertifikat muss bestätigt werden.");
                self.confirm(
                    ConfirmAction::TrustProxora(fingerprint),
                    if changed {
                        "Proxora-Zertifikat geändert"
                    } else {
                        "Proxora-Zertifikat vertrauen?"
                    },
                    &message,
                    "Zertifikat vertrauen",
                    changed,
                );
            }
            Err(ProxoraError::TotpRequired { ticket }) => {
                self.proxora_status("Zwei-Faktor-Code erforderlich.");
                self.text_dialog(
                    TextAction::ProxoraTotp(ticket),
                    "Proxora: Zwei-Faktor-Anmeldung",
                    "Code aus der Authenticator-App (oder Wiederherstellungscode)",
                    "",
                    "Anmelden",
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "Proxora-Import fehlgeschlagen");
                self.proxora_status(&e.to_string());
            }
        }
    }

    /// Ein im Formular eingegebenes Passwort nach erfolgreicher Anmeldung sicher ablegen.
    fn proxora_save_typed_password(&mut self) {
        let mut typed = String::new();
        self.with_ui(|ui| typed = ui.global::<Bridge>().get_proxora_password().to_string());
        if typed.is_empty() {
            return;
        }
        let secret = Secret::from(typed);
        match self
            .conns
            .credentials()
            .set(CredentialKey::proxora(), &self.settings.proxora_username, &secret)
        {
            Ok(()) => self.with_ui(|ui| {
                let b = ui.global::<Bridge>();
                b.set_proxora_password("".into());
                b.set_proxora_password_stored(true);
            }),
            Err(e) => self.toast(&format!("Passwort konnte nicht gespeichert werden: {e}"), 2),
        }
    }

    pub(super) fn proxora_trust(&mut self, fingerprint: String) {
        self.settings.proxora_fingerprint = fingerprint.clone();
        self.save_settings();
        self.with_ui(|ui| ui.global::<Bridge>().set_proxora_fingerprint(fingerprint.into()));
        self.proxora_load(None);
    }

    pub(super) fn proxora_forget_certificate(&mut self) {
        self.settings.proxora_fingerprint.clear();
        self.save_settings();
        self.with_ui(|ui| ui.global::<Bridge>().set_proxora_fingerprint("".into()));
        self.proxora_status("Bestätigtes Zertifikat entfernt.");
    }

    fn proxora_imported(&self, marker: &str) -> bool {
        self.connections
            .iter()
            .any(|c| proxora::marker_in(&c.description) == Some(marker))
    }

    fn proxora_refresh_list(&self) {
        let items: Vec<ProxoraVmItem> = self
            .proxora_vms
            .iter()
            .map(|vm| {
                let marker = vm.marker();
                ProxoraVmItem {
                    name: vm.name.clone().into(),
                    address: vm.preferred_address().unwrap_or_default().into(),
                    location: format!("{} · {} · VM {}", vm.host_name, vm.node, vm.vmid).into(),
                    running: vm.status == "running",
                    selected: self.proxora_selected.contains(&marker),
                    imported: self.proxora_imported(&marker),
                    key: marker.into(),
                }
            })
            .collect();
        self.with_ui(move |ui| {
            ui.global::<Bridge>()
                .set_proxora_vms(ModelRc::from(Rc::new(VecModel::from(items))));
        });
    }

    pub(super) fn proxora_toggle(&mut self, index: usize) {
        if let Some(vm) = self.proxora_vms.get(index) {
            let marker = vm.marker();
            if !self.proxora_selected.remove(&marker) {
                self.proxora_selected.insert(marker);
            }
            self.proxora_refresh_list();
        }
    }

    pub(super) fn proxora_select_all(&mut self, all: bool) {
        self.proxora_selected = if all {
            self.proxora_vms.iter().map(ProxoraVm::marker).collect()
        } else {
            Default::default()
        };
        self.proxora_refresh_list();
    }

    /// Legt die ausgewählten VMs an bzw. aktualisiert ihre Adresse.
    pub(super) fn proxora_import(&mut self) {
        let selected: Vec<ProxoraVm> = self
            .proxora_vms
            .iter()
            .filter(|v| self.proxora_selected.contains(&v.marker()))
            .cloned()
            .collect();
        if selected.is_empty() {
            self.toast("Keine VM ausgewählt.", 2);
            return;
        }
        match self.proxora_apply(&selected) {
            Ok((created, updated, unchanged)) => {
                self.refresh_connections();
                self.proxora_refresh_list();
                let msg = format!("Proxora: {created} angelegt, {updated} aktualisiert, {unchanged} unverändert.");
                self.proxora_status(&msg);
                self.toast(&msg, 1);
            }
            Err(e) => {
                self.refresh_connections();
                self.toast(&format!("Import fehlgeschlagen: {e:#}"), 3);
            }
        }
    }

    fn proxora_apply(&mut self, vms: &[ProxoraVm]) -> anyhow::Result<(usize, usize, usize)> {
        let db = self.conns.db().clone();
        let folders = db.list_folders()?;
        let root = match folders.iter().find(|f| f.parent_id.is_none() && f.name == ROOT_FOLDER) {
            Some(f) => f.id,
            None => db.create_folder(ROOT_FOLDER, None)?,
        };
        let mut host_folders: HashMap<String, FolderId> = folders
            .iter()
            .filter(|f| f.parent_id == Some(root))
            .map(|f| (f.name.clone(), f.id))
            .collect();

        let (mut created, mut updated, mut unchanged) = (0, 0, 0);
        for vm in vms {
            let marker = vm.marker();
            let address = vm.preferred_address();
            let existing = self
                .connections
                .iter()
                .find(|c| proxora::marker_in(&c.description) == Some(marker.as_str()))
                .cloned();
            match existing {
                Some(mut conn) => match address {
                    Some(addr) if conn.host != addr => {
                        conn.host = addr;
                        self.conns.save(&conn, PasswordChange::Keep, PasswordChange::Keep)?;
                        updated += 1;
                    }
                    _ => unchanged += 1,
                },
                None => {
                    let folder = match host_folders.get(&vm.host_name) {
                        Some(id) => *id,
                        None => {
                            let id = db.create_folder(&vm.host_name, Some(root))?;
                            host_folders.insert(vm.host_name.clone(), id);
                            id
                        }
                    };
                    let mut conn = Connection::new(vm.name.clone(), address.unwrap_or_else(|| vm.name.clone()));
                    conn.folder_id = Some(folder);
                    conn.description = format!(
                        "Proxmox-VM {} auf {} ({}), importiert aus Proxora\n{marker}",
                        vm.vmid, vm.host_name, vm.node
                    );
                    conn.tags = vm.tags.clone();
                    if !conn.tags.iter().any(|t| t == "proxora") {
                        conn.tags.push("proxora".into());
                    }
                    conn.icon = "server".into();
                    conn.color = "#8338ec".into();
                    conn.options.scale_mode = self.settings.default_scale_mode;
                    conn.options.resolution = self.settings.default_resolution;
                    self.conns.save(&conn, PasswordChange::Keep, PasswordChange::Keep)?;
                    created += 1;
                }
            }
        }
        Ok((created, updated, unchanged))
    }
}
