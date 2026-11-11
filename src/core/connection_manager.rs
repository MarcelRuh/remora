//! Fachlogik der Verbindungsverwaltung: CRUD inkl. Zugangsdaten, Baumdarstellung, Suche.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use crate::database::{Connection, ConnectionId, Database, DbResult, Folder, FolderId};
use crate::security::credentials::{CredentialKey, CredentialStore, Secret};

/// Was mit dem gespeicherten Passwort beim Speichern passieren soll.
#[derive(Debug, Clone)]
pub enum PasswordChange {
    Keep,
    Set(Secret),
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// Ordnerhierarchie, innerhalb alphabetisch.
    #[default]
    Folders,
    /// Flache, alphabetische Liste.
    Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRowKind {
    Folder,
    Connection,
}

/// Eine Zeile der (abgeflachten) Sidebar-Ansicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub kind: TreeRowKind,
    pub id: i64,
    pub depth: u32,
    pub name: String,
    pub subtitle: String,
    pub expanded: bool,
    pub child_count: usize,
}

pub struct ConnectionManager {
    db: Arc<Database>,
    creds: Arc<dyn CredentialStore>,
}

impl ConnectionManager {
    pub fn new(db: Arc<Database>, creds: Arc<dyn CredentialStore>) -> Self {
        Self { db, creds }
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn credentials(&self) -> &Arc<dyn CredentialStore> {
        &self.creds
    }

    pub fn has_password(&self, id: ConnectionId) -> bool {
        self.creds.exists(CredentialKey::password(id))
    }

    pub fn password(&self, id: ConnectionId) -> Option<Secret> {
        match self.creds.get(CredentialKey::password(id)) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "Passwort konnte nicht gelesen werden");
                None
            }
        }
    }

    pub fn gateway_password(&self, id: ConnectionId) -> Option<Secret> {
        self.creds.get(CredentialKey::gateway(id)).ok().flatten()
    }

    /// Speichert eine Verbindung (neu bei `id == 0`) und gibt die ID zurück.
    pub fn save(
        &self,
        conn: &Connection,
        password: PasswordChange,
        gateway_password: PasswordChange,
    ) -> anyhow::Result<ConnectionId> {
        let id = if conn.id == 0 {
            self.db.insert_connection(conn)?
        } else {
            self.db.update_connection(conn)?;
            conn.id
        };
        self.apply_secret(CredentialKey::password(id), &conn.username, password)?;
        self.apply_secret(
            CredentialKey::gateway(id),
            &conn.options.gateway.username,
            gateway_password,
        )?;
        Ok(id)
    }

    fn apply_secret(&self, key: CredentialKey, user: &str, change: PasswordChange) -> anyhow::Result<()> {
        match change {
            PasswordChange::Keep => Ok(()),
            PasswordChange::Set(s) if s.is_empty() => Ok(self.creds.delete(key)?),
            PasswordChange::Set(s) => Ok(self.creds.set(key, user, &s)?),
            PasswordChange::Clear => Ok(self.creds.delete(key)?),
        }
    }

    pub fn store_password(&self, id: ConnectionId, username: &str, secret: &Secret) -> anyhow::Result<()> {
        Ok(self.creds.set(CredentialKey::password(id), username, secret)?)
    }

    /// Löscht die Verbindung samt gespeicherter Zugangsdaten.
    pub fn delete(&self, id: ConnectionId) -> anyhow::Result<()> {
        self.db.delete_connection(id)?;
        let _ = self.creds.delete(CredentialKey::password(id));
        let _ = self.creds.delete(CredentialKey::gateway(id));
        Ok(())
    }

    /// Dupliziert eine Verbindung inklusive gespeichertem Passwort.
    pub fn duplicate(&self, id: ConnectionId) -> anyhow::Result<ConnectionId> {
        let new_id = self.db.duplicate_connection(id)?;
        let conn = self.db.get_connection(new_id)?;
        if let Some(pw) = self.password(id) {
            let _ = self.creds.set(CredentialKey::password(new_id), &conn.username, &pw);
        }
        if let Some(pw) = self.gateway_password(id) {
            let _ = self
                .creds
                .set(CredentialKey::gateway(new_id), &conn.options.gateway.username, &pw);
        }
        Ok(new_id)
    }

    pub fn delete_folder(&self, id: FolderId) -> DbResult<()> {
        self.db.delete_folder(id)
    }

    pub fn all_tags(connections: &[Connection]) -> Vec<String> {
        let set: BTreeSet<String> = connections.iter().flat_map(|c| c.tags.iter().cloned()).collect();
        set.into_iter().collect()
    }
}

/// Prüft, ob eine Verbindung zum Suchbegriff passt (Name, Host, Benutzer, Beschreibung, Tags).
pub fn matches_query(conn: &Connection, folder_path: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {} {} {}",
        conn.name,
        conn.host,
        conn.display_user(),
        conn.description,
        conn.tags.join(" "),
        folder_path,
        conn.port
    )
    .to_lowercase();
    query.split_whitespace().all(|term| {
        if let Some(tag) = term.strip_prefix('#') {
            conn.tags.iter().any(|t| t.to_lowercase() == tag)
        } else {
            haystack.contains(term)
        }
    })
}

/// Pfad eines Ordners, z. B. `Kunden / ACME`.
pub fn folder_path(folders: &HashMap<FolderId, &Folder>, mut id: Option<FolderId>) -> String {
    let mut parts = Vec::new();
    let mut guard = 0;
    while let Some(fid) = id {
        let Some(f) = folders.get(&fid) else { break };
        parts.push(f.name.clone());
        id = f.parent_id;
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    parts.reverse();
    parts.join(" / ")
}

/// Baut die abgeflachte Sidebar-Ansicht.
///
/// Bei aktiver Suche werden nur passende Verbindungen und ihre Ordner angezeigt (Ordner
/// dann immer aufgeklappt).
pub fn build_tree(folders: &[Folder], connections: &[Connection], query: &str, sort: SortMode) -> Vec<TreeRow> {
    let by_id: HashMap<FolderId, &Folder> = folders.iter().map(|f| (f.id, f)).collect();
    let searching = !query.trim().is_empty();
    let subtitle = |c: &Connection| {
        let user = c.display_user();
        if user.is_empty() {
            c.host.clone()
        } else {
            format!("{user} @ {}", c.host)
        }
    };

    let mut visible: Vec<&Connection> = connections
        .iter()
        .filter(|c| matches_query(c, &folder_path(&by_id, c.folder_id), query))
        .collect();
    visible.sort_by_key(|c| c.name.to_lowercase());

    if sort == SortMode::Name {
        return visible
            .into_iter()
            .map(|c| TreeRow {
                kind: TreeRowKind::Connection,
                id: c.id,
                depth: 0,
                name: c.name.clone(),
                subtitle: subtitle(c),
                expanded: false,
                child_count: 0,
            })
            .collect();
    }

    // Ordner → Kinder
    let mut child_folders: HashMap<Option<FolderId>, Vec<&Folder>> = HashMap::new();
    for f in folders {
        // Verwaiste Ordner (ungültiger Elternteil) auf oberster Ebene anzeigen
        let parent = f.parent_id.filter(|p| by_id.contains_key(p));
        child_folders.entry(parent).or_default().push(f);
    }
    for list in child_folders.values_mut() {
        list.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }
    let mut conns_in: HashMap<Option<FolderId>, Vec<&Connection>> = HashMap::new();
    for c in &visible {
        let folder = c.folder_id.filter(|f| by_id.contains_key(f));
        conns_in.entry(folder).or_default().push(c);
    }

    // Anzahl (sichtbarer) Verbindungen pro Teilbaum
    fn count(
        id: Option<FolderId>,
        child_folders: &HashMap<Option<FolderId>, Vec<&Folder>>,
        conns_in: &HashMap<Option<FolderId>, Vec<&Connection>>,
        depth: u32,
    ) -> usize {
        if depth > 64 {
            return 0;
        }
        let own = conns_in.get(&id).map_or(0, Vec::len);
        own + child_folders.get(&id).map_or(0, |fs| {
            fs.iter()
                .map(|f| count(Some(f.id), child_folders, conns_in, depth + 1))
                .sum()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        parent: Option<FolderId>,
        depth: u32,
        searching: bool,
        child_folders: &HashMap<Option<FolderId>, Vec<&Folder>>,
        conns_in: &HashMap<Option<FolderId>, Vec<&Connection>>,
        subtitle: &dyn Fn(&Connection) -> String,
        out: &mut Vec<TreeRow>,
    ) {
        if depth > 64 {
            return;
        }
        if let Some(fs) = child_folders.get(&parent) {
            for f in fs {
                let n = count(Some(f.id), child_folders, conns_in, depth);
                if searching && n == 0 {
                    continue;
                }
                let expanded = searching || f.expanded;
                out.push(TreeRow {
                    kind: TreeRowKind::Folder,
                    id: f.id,
                    depth,
                    name: f.name.clone(),
                    subtitle: String::new(),
                    expanded,
                    child_count: n,
                });
                if expanded {
                    walk(Some(f.id), depth + 1, searching, child_folders, conns_in, subtitle, out);
                }
            }
        }
        if let Some(cs) = conns_in.get(&parent) {
            for c in cs {
                out.push(TreeRow {
                    kind: TreeRowKind::Connection,
                    id: c.id,
                    depth,
                    name: c.name.clone(),
                    subtitle: subtitle(c),
                    expanded: false,
                    child_count: 0,
                });
            }
        }
    }

    let mut out = Vec::new();
    walk(None, 0, searching, &child_folders, &conns_in, &subtitle, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::credentials::MemoryCredentialStore;

    fn folder(id: i64, name: &str, parent: Option<i64>, expanded: bool) -> Folder {
        Folder {
            id,
            name: name.into(),
            parent_id: parent,
            sort_order: 0,
            expanded,
        }
    }

    fn conn(id: i64, name: &str, host: &str, folder: Option<i64>) -> Connection {
        let mut c = Connection::new(name, host);
        c.id = id;
        c.folder_id = folder;
        c
    }

    #[test]
    fn tree_structure_and_collapse() {
        let folders = vec![folder(1, "Kunden", None, true), folder(2, "ACME", Some(1), false)];
        let conns = vec![
            conn(10, "Zeta", "z", None),
            conn(11, "Alpha", "a", Some(1)),
            conn(12, "DC01", "dc", Some(2)),
        ];
        let rows = build_tree(&folders, &conns, "", SortMode::Folders);
        let names: Vec<_> = rows.iter().map(|r| (r.name.as_str(), r.depth)).collect();
        // ACME ist eingeklappt → DC01 nicht sichtbar
        assert_eq!(names, vec![("Kunden", 0), ("ACME", 1), ("Alpha", 1), ("Zeta", 0)]);
        assert_eq!(rows[0].child_count, 2);
    }

    #[test]
    fn search_expands_matching_folders() {
        let folders = vec![folder(1, "Kunden", None, false), folder(2, "Leer", None, true)];
        let conns = vec![conn(11, "DC01", "dc01.acme", Some(1)), conn(12, "Web", "web", None)];
        let rows = build_tree(&folders, &conns, "dc01", SortMode::Folders);
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Kunden", "DC01"]);
    }

    #[test]
    fn flat_sort_by_name() {
        let folders = vec![folder(1, "F", None, true)];
        let conns = vec![conn(1, "beta", "b", Some(1)), conn(2, "Alpha", "a", None)];
        let rows = build_tree(&folders, &conns, "", SortMode::Name);
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta"]);
    }

    #[test]
    fn query_matches_tags_and_terms() {
        let mut c = conn(1, "Mail Server", "mx01.corp", None);
        c.tags = vec!["prod".into(), "exchange".into()];
        c.username = "admin".into();
        assert!(matches_query(&c, "", "mail mx01"));
        assert!(matches_query(&c, "", "#prod"));
        assert!(!matches_query(&c, "", "#test"));
        assert!(matches_query(&c, "Kunden / ACME", "acme"));
        assert!(matches_query(&c, "", "ADMIN"));
        assert!(!matches_query(&c, "", "mail db"));
    }

    #[test]
    fn manager_save_delete_cleans_credentials() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let creds: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
        let mgr = ConnectionManager::new(db.clone(), creds.clone());
        let mut c = Connection::new("Srv", "srv");
        c.username = "u".into();
        let id = mgr
            .save(&c, PasswordChange::Set(Secret::new("pw")), PasswordChange::Keep)
            .unwrap();
        assert!(mgr.has_password(id));
        // Passwort steht nicht in der Datenbank
        let stored = db.get_connection(id).unwrap();
        assert!(!format!("{stored:?}").contains("pw\""));

        let dup = mgr.duplicate(id).unwrap();
        assert!(mgr.has_password(dup));
        assert_eq!(db.get_connection(dup).unwrap().name, "Srv (Kopie)");

        c.id = id;
        mgr.save(&c, PasswordChange::Clear, PasswordChange::Keep).unwrap();
        assert!(!mgr.has_password(id));
        mgr.delete(dup).unwrap();
        assert!(!mgr.has_password(dup));
        assert!(db.get_connection(dup).is_err());
    }
}
