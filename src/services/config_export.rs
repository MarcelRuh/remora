//! Export und Import der gesamten Verbindungskonfiguration.
//!
//! * **Normaler Export** (`remora-export`, JSON): Ordner und Verbindungen, *ohne* Zugangsdaten.
//! * **Vault-Export** (`remora-vault`): derselbe Inhalt inklusive Passwörtern, verschlüsselt
//!   mit einem Master-Passwort (siehe [`crate::security::vault`]).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::database::{Connection, Database, Folder, FolderId};
use crate::security::credentials::{CredentialKey, CredentialStore, Secret};
use crate::security::vault;

pub const EXPORT_FORMAT: &str = "remora-export";
pub const EXPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedConnection {
    #[serde(flatten)]
    pub connection: Connection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_password: Option<String>,
}

impl Drop for ExportedConnection {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        if let Some(p) = self.password.as_mut() {
            p.zeroize();
        }
        if let Some(p) = self.gateway_password.as_mut() {
            p.zeroize();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub format: String,
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub application: String,
    pub folders: Vec<Folder>,
    pub connections: Vec<ExportedConnection>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Db(#[from] crate::database::DbError),
    #[error("Ungültige Datei: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Vault(#[from] vault::VaultError),
    #[error("Die Datei ist verschlüsselt; bitte Master-Passwort angeben.")]
    PasswordRequired,
    #[error("Nicht unterstütztes Exportformat.")]
    UnsupportedFormat,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub folders: usize,
    pub connections: usize,
    pub passwords: usize,
}

/// Erstellt das Export-Bündel. Passwörter werden nur bei `include_secrets` gelesen.
pub fn build_bundle(
    db: &Database,
    creds: &dyn CredentialStore,
    include_secrets: bool,
) -> Result<ExportBundle, ExportError> {
    let folders = db.list_folders()?;
    let connections = db
        .list_connections()?
        .into_iter()
        .map(|c| {
            let (password, gateway_password) = if include_secrets {
                (
                    creds
                        .get(CredentialKey::password(c.id))
                        .ok()
                        .flatten()
                        .map(|s| s.expose().to_owned()),
                    creds
                        .get(CredentialKey::gateway(c.id))
                        .ok()
                        .flatten()
                        .map(|s| s.expose().to_owned()),
                )
            } else {
                (None, None)
            };
            ExportedConnection {
                connection: c,
                password,
                gateway_password,
            }
        })
        .collect();
    Ok(ExportBundle {
        format: EXPORT_FORMAT.to_owned(),
        version: EXPORT_VERSION,
        exported_at: Utc::now(),
        application: format!("Remora {}", env!("CARGO_PKG_VERSION")),
        folders,
        connections,
    })
}

/// Normaler Export ohne Zugangsdaten.
pub fn export_plain(db: &Database, creds: &dyn CredentialStore) -> Result<String, ExportError> {
    let bundle = build_bundle(db, creds, false)?;
    Ok(serde_json::to_string_pretty(&bundle)?)
}

/// Verschlüsselter Export inkl. Zugangsdaten.
pub fn export_vault(db: &Database, creds: &dyn CredentialStore, master: &str) -> Result<String, ExportError> {
    let bundle = build_bundle(db, creds, true)?;
    let plain = Zeroizing::new(serde_json::to_vec(&bundle)?);
    let file = vault::encrypt(&plain, master)?;
    Ok(serde_json::to_string_pretty(&file)?)
}

/// Liest ein Bündel (normal oder Vault).
pub fn parse_bundle(text: &str, master: Option<&str>) -> Result<ExportBundle, ExportError> {
    let bundle: ExportBundle = if vault::is_vault(text) {
        let master = master.ok_or(ExportError::PasswordRequired)?;
        let file: vault::VaultFile = serde_json::from_str(text)?;
        let plain = vault::decrypt(&file, master)?;
        serde_json::from_slice(&plain)?
    } else {
        serde_json::from_str(text)?
    };
    if bundle.format != EXPORT_FORMAT || bundle.version > EXPORT_VERSION {
        return Err(ExportError::UnsupportedFormat);
    }
    Ok(bundle)
}

/// Importiert ein Bündel *zusätzlich* zu den vorhandenen Daten. Ordner werden mit neuen IDs
/// angelegt; die Hierarchie bleibt erhalten.
pub fn import_bundle(
    db: &Database,
    creds: &dyn CredentialStore,
    bundle: &ExportBundle,
) -> Result<ImportSummary, ExportError> {
    let mut summary = ImportSummary::default();
    let mut id_map: HashMap<FolderId, FolderId> = HashMap::new();

    // Ordner topologisch anlegen (Eltern vor Kindern).
    let mut pending: Vec<&Folder> = bundle.folders.iter().collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut rest = Vec::new();
        for f in pending {
            let parent = match f.parent_id {
                None => Some(None),
                Some(p) => id_map.get(&p).map(|np| Some(*np)),
            };
            match parent {
                Some(parent) => {
                    let new_id = db.create_folder(&f.name, parent)?;
                    db.set_folder_expanded(new_id, f.expanded)?;
                    id_map.insert(f.id, new_id);
                    summary.folders += 1;
                }
                None => rest.push(f),
            }
        }
        if rest.len() == before {
            // Verwaiste Ordner (Elternteil fehlt) auf oberster Ebene anlegen.
            for f in rest {
                let new_id = db.create_folder(&f.name, None)?;
                id_map.insert(f.id, new_id);
                summary.folders += 1;
            }
            break;
        }
        pending = rest;
    }

    for item in &bundle.connections {
        let mut c = item.connection.clone();
        c.folder_id = c.folder_id.and_then(|f| id_map.get(&f).copied());
        let new_id = db.insert_connection(&c)?;
        summary.connections += 1;
        if let Some(pw) = item.password.as_deref().filter(|p| !p.is_empty())
            && creds
                .set(CredentialKey::password(new_id), &c.username, &Secret::new(pw))
                .is_ok()
        {
            summary.passwords += 1;
        }
        if let Some(pw) = item.gateway_password.as_deref().filter(|p| !p.is_empty()) {
            let _ = creds.set(
                CredentialKey::gateway(new_id),
                &c.options.gateway.username,
                &Secret::new(pw),
            );
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::credentials::MemoryCredentialStore;

    fn seed() -> (Database, MemoryCredentialStore) {
        let db = Database::open_in_memory().unwrap();
        let creds = MemoryCredentialStore::default();
        let root = db.create_folder("Kunden", None).unwrap();
        let sub = db.create_folder("ACME", Some(root)).unwrap();
        let mut c = Connection::new("DC01", "dc01.acme.local");
        c.folder_id = Some(sub);
        c.username = "admin".into();
        let id = db.insert_connection(&c).unwrap();
        creds
            .set(CredentialKey::password(id), "admin", &Secret::new("geheim123"))
            .unwrap();
        (db, creds)
    }

    #[test]
    fn plain_export_excludes_passwords() {
        let (db, creds) = seed();
        let text = export_plain(&db, &creds).unwrap();
        assert!(!text.contains("geheim123"));
        assert!(!text.contains("\"password\""));
        let bundle = parse_bundle(&text, None).unwrap();
        assert_eq!(bundle.connections.len(), 1);
        assert_eq!(bundle.folders.len(), 2);
    }

    #[test]
    fn vault_roundtrip_restores_hierarchy_and_secrets() {
        let (db, creds) = seed();
        let text = export_vault(&db, &creds, "MasterPasswort!").unwrap();
        assert!(!text.contains("geheim123"));
        assert!(matches!(parse_bundle(&text, None), Err(ExportError::PasswordRequired)));
        assert!(parse_bundle(&text, Some("falsch-falsch")).is_err());

        let bundle = parse_bundle(&text, Some("MasterPasswort!")).unwrap();
        let target = Database::open_in_memory().unwrap();
        let target_creds = MemoryCredentialStore::default();
        let summary = import_bundle(&target, &target_creds, &bundle).unwrap();
        assert_eq!(
            summary,
            ImportSummary {
                folders: 2,
                connections: 1,
                passwords: 1
            }
        );

        let folders = target.list_folders().unwrap();
        let acme = folders.iter().find(|f| f.name == "ACME").unwrap();
        let kunden = folders.iter().find(|f| f.name == "Kunden").unwrap();
        assert_eq!(acme.parent_id, Some(kunden.id));
        let conn = &target.list_connections().unwrap()[0];
        assert_eq!(conn.folder_id, Some(acme.id));
        assert_eq!(
            target_creds
                .get(CredentialKey::password(conn.id))
                .unwrap()
                .unwrap()
                .expose(),
            "geheim123"
        );
    }
}
