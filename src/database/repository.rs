//! SQLite-Repository für Ordner, Verbindungen und vertrauenswürdige Zertifikate.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{Connection as SqlConnection, OptionalExtension, Row, params};

use super::migrations;
use super::models::{Connection, ConnectionId, ConnectionOptions, Folder, FolderId, KnownHost};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("Datenbankfehler: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("Serialisierungsfehler: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Eintrag nicht gefunden")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// Thread-sicherer Zugriff auf die Anwendungsdatenbank.
///
/// Alle Operationen sind kurze, lokale Abfragen; der Mutex serialisiert den Zugriff zwischen
/// UI-Thread und Sitzungs-Threads (z. B. Zertifikatsprüfung).
pub struct Database {
    conn: Mutex<SqlConnection>,
}

fn ts(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

const CONNECTION_COLUMNS: &str = "id, name, host, port, username, domain, description, folder_id, tags, color, icon, \
     favorite, options, sort_order, last_used, created_at, updated_at";

fn row_to_connection(row: &Row<'_>) -> rusqlite::Result<Connection> {
    let tags: String = row.get(8)?;
    let options: String = row.get(12)?;
    let last_used: Option<String> = row.get(14)?;
    let created_at: String = row.get(15)?;
    let updated_at: String = row.get(16)?;
    let port: i64 = row.get(3)?;
    Ok(Connection {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: u16::try_from(port).unwrap_or(3389),
        username: row.get(4)?,
        domain: row.get(5)?,
        description: row.get(6)?,
        folder_id: row.get(7)?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        color: row.get(9)?,
        icon: row.get(10)?,
        favorite: row.get::<_, i64>(11)? != 0,
        options: serde_json::from_str::<ConnectionOptions>(&options).unwrap_or_default(),
        sort_order: row.get(13)?,
        last_used: last_used.as_deref().map(parse_ts),
        created_at: parse_ts(&created_at),
        updated_at: parse_ts(&updated_at),
    })
}

fn row_to_folder(row: &Row<'_>) -> rusqlite::Result<Folder> {
    Ok(Folder {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        sort_order: row.get(3)?,
        expanded: row.get::<_, i64>(4)? != 0,
    })
}

impl Database {
    pub fn open(path: &Path) -> DbResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DbError::Invalid(e.to_string()))?;
        }
        let mut conn = SqlConnection::open(path)?;
        migrations::run(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> DbResult<Self> {
        let mut conn = SqlConnection::open_in_memory()?;
        migrations::run(&mut conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&mut SqlConnection) -> DbResult<T>) -> DbResult<T> {
        let mut guard = self.conn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    // ---------------------------------------------------------------- Ordner

    pub fn list_folders(&self) -> DbResult<Vec<Folder>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT id, name, parent_id, sort_order, expanded FROM folders ORDER BY sort_order, name")?;
            let rows = stmt.query_map([], row_to_folder)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    pub fn create_folder(&self, name: &str, parent_id: Option<FolderId>) -> DbResult<FolderId> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Invalid("Der Ordnername darf nicht leer sein.".into()));
        }
        self.with(|c| {
            let order: i64 = c.query_row(
                "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM folders WHERE parent_id IS ?1",
                params![parent_id],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO folders (name, parent_id, sort_order, expanded) VALUES (?1, ?2, ?3, 1)",
                params![name, parent_id, order],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn rename_folder(&self, id: FolderId, name: &str) -> DbResult<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Invalid("Der Ordnername darf nicht leer sein.".into()));
        }
        self.with(|c| {
            let n = c.execute("UPDATE folders SET name = ?1 WHERE id = ?2", params![name, id])?;
            if n == 0 { Err(DbError::NotFound) } else { Ok(()) }
        })
    }

    pub fn set_folder_expanded(&self, id: FolderId, expanded: bool) -> DbResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE folders SET expanded = ?1 WHERE id = ?2",
                params![expanded as i64, id],
            )?;
            Ok(())
        })
    }

    /// Verschiebt einen Ordner; verhindert Zyklen (Ordner in eigenen Unterordner).
    pub fn move_folder(&self, id: FolderId, new_parent: Option<FolderId>) -> DbResult<()> {
        self.with(|c| {
            let mut cursor = new_parent;
            while let Some(current) = cursor {
                if current == id {
                    return Err(DbError::Invalid(
                        "Ein Ordner kann nicht in sich selbst oder einen Unterordner verschoben werden.".into(),
                    ));
                }
                cursor = c
                    .query_row("SELECT parent_id FROM folders WHERE id = ?1", params![current], |r| {
                        r.get::<_, Option<FolderId>>(0)
                    })
                    .optional()?
                    .flatten();
            }
            let n = c.execute(
                "UPDATE folders SET parent_id = ?1 WHERE id = ?2",
                params![new_parent, id],
            )?;
            if n == 0 { Err(DbError::NotFound) } else { Ok(()) }
        })
    }

    /// Löscht einen Ordner samt Unterordnern. Enthaltene Verbindungen werden in den
    /// übergeordneten Ordner des gelöschten Ordners verschoben (nicht gelöscht).
    pub fn delete_folder(&self, id: FolderId) -> DbResult<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            let parent: Option<FolderId> = tx
                .query_row("SELECT parent_id FROM folders WHERE id = ?1", params![id], |r| r.get(0))
                .optional()?
                .ok_or(DbError::NotFound)?;
            tx.execute(
                "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT f.id FROM folders f JOIN sub ON f.parent_id = sub.id)
                 UPDATE connections SET folder_id = ?2 WHERE folder_id IN (SELECT id FROM sub)",
                params![id, parent],
            )?;
            tx.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(())
        })
    }

    // ----------------------------------------------------------- Verbindungen

    pub fn list_connections(&self) -> DbResult<Vec<Connection>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {CONNECTION_COLUMNS} FROM connections ORDER BY sort_order, name COLLATE NOCASE"
            ))?;
            let rows = stmt.query_map([], row_to_connection)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_connection(&self, id: ConnectionId) -> DbResult<Connection> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {CONNECTION_COLUMNS} FROM connections WHERE id = ?1"),
                params![id],
                row_to_connection,
            )
            .optional()?
            .ok_or(DbError::NotFound)
        })
    }

    /// Fügt eine Verbindung ein und gibt die neue ID zurück.
    pub fn insert_connection(&self, conn: &Connection) -> DbResult<ConnectionId> {
        conn.validate().map_err(DbError::Invalid)?;
        let tags = serde_json::to_string(&conn.tags)?;
        let options = serde_json::to_string(&conn.options)?;
        let now = Utc::now();
        self.with(|c| {
            let order: i64 = c.query_row("SELECT COALESCE(MAX(sort_order), 0) + 1 FROM connections", [], |r| {
                r.get(0)
            })?;
            c.execute(
                "INSERT INTO connections (name, host, port, username, domain, description, folder_id, tags, color, icon,
                    favorite, options, sort_order, last_used, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
                params![
                    conn.name.trim(),
                    conn.host.trim(),
                    conn.port,
                    conn.username,
                    conn.domain,
                    conn.description,
                    conn.folder_id,
                    tags,
                    conn.color,
                    conn.icon,
                    conn.favorite as i64,
                    options,
                    order,
                    conn.last_used.as_ref().map(ts),
                    ts(&now),
                ],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn update_connection(&self, conn: &Connection) -> DbResult<()> {
        conn.validate().map_err(DbError::Invalid)?;
        let tags = serde_json::to_string(&conn.tags)?;
        let options = serde_json::to_string(&conn.options)?;
        self.with(|c| {
            let n = c.execute(
                "UPDATE connections SET name = ?1, host = ?2, port = ?3, username = ?4, domain = ?5, description = ?6,
                    folder_id = ?7, tags = ?8, color = ?9, icon = ?10, favorite = ?11, options = ?12, updated_at = ?13
                 WHERE id = ?14",
                params![
                    conn.name.trim(),
                    conn.host.trim(),
                    conn.port,
                    conn.username,
                    conn.domain,
                    conn.description,
                    conn.folder_id,
                    tags,
                    conn.color,
                    conn.icon,
                    conn.favorite as i64,
                    options,
                    ts(&Utc::now()),
                    conn.id,
                ],
            )?;
            if n == 0 { Err(DbError::NotFound) } else { Ok(()) }
        })
    }

    pub fn delete_connection(&self, id: ConnectionId) -> DbResult<()> {
        self.with(|c| {
            let n = c.execute("DELETE FROM connections WHERE id = ?1", params![id])?;
            if n == 0 { Err(DbError::NotFound) } else { Ok(()) }
        })
    }

    /// Dupliziert eine Verbindung (ohne Passwort, ohne Favoritenstatus und Verlauf).
    pub fn duplicate_connection(&self, id: ConnectionId) -> DbResult<ConnectionId> {
        let mut copy = self.get_connection(id)?;
        copy.name = format!("{} (Kopie)", copy.name);
        copy.favorite = false;
        copy.last_used = None;
        self.insert_connection(&copy)
    }

    pub fn move_connection(&self, id: ConnectionId, folder_id: Option<FolderId>) -> DbResult<()> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE connections SET folder_id = ?1 WHERE id = ?2",
                params![folder_id, id],
            )?;
            if n == 0 { Err(DbError::NotFound) } else { Ok(()) }
        })
    }

    pub fn set_favorite(&self, id: ConnectionId, favorite: bool) -> DbResult<()> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE connections SET favorite = ?1 WHERE id = ?2",
                params![favorite as i64, id],
            )?;
            if n == 0 { Err(DbError::NotFound) } else { Ok(()) }
        })
    }

    pub fn touch_last_used(&self, id: ConnectionId) -> DbResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE connections SET last_used = ?1 WHERE id = ?2",
                params![ts(&Utc::now()), id],
            )?;
            Ok(())
        })
    }

    /// Zuletzt verwendete Verbindungen, neueste zuerst.
    pub fn recent_connections(&self, limit: usize) -> DbResult<Vec<Connection>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {CONNECTION_COLUMNS} FROM connections WHERE last_used IS NOT NULL ORDER BY last_used DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![limit as i64], row_to_connection)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    // -------------------------------------------------------- Zertifikate

    pub fn known_host(&self, endpoint: &str) -> DbResult<Option<KnownHost>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT endpoint, fingerprint, first_seen, last_seen FROM known_hosts WHERE endpoint = ?1",
                params![endpoint],
                |r| {
                    Ok(KnownHost {
                        endpoint: r.get(0)?,
                        fingerprint: r.get(1)?,
                        first_seen: parse_ts(&r.get::<_, String>(2)?),
                        last_seen: parse_ts(&r.get::<_, String>(3)?),
                    })
                },
            )
            .optional()?)
        })
    }

    pub fn trust_host(&self, endpoint: &str, fingerprint: &str) -> DbResult<()> {
        let now = ts(&Utc::now());
        self.with(|c| {
            c.execute(
                "INSERT INTO known_hosts (endpoint, fingerprint, first_seen, last_seen) VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(endpoint) DO UPDATE SET fingerprint = excluded.fingerprint,
                    first_seen = CASE WHEN known_hosts.fingerprint = excluded.fingerprint THEN known_hosts.first_seen ELSE excluded.first_seen END,
                    last_seen = excluded.last_seen",
                params![endpoint, fingerprint, now],
            )?;
            Ok(())
        })
    }

    pub fn touch_known_host(&self, endpoint: &str) -> DbResult<()> {
        self.with(|c| {
            c.execute(
                "UPDATE known_hosts SET last_seen = ?1 WHERE endpoint = ?2",
                params![ts(&Utc::now()), endpoint],
            )?;
            Ok(())
        })
    }

    pub fn list_known_hosts(&self) -> DbResult<Vec<KnownHost>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT endpoint, fingerprint, first_seen, last_seen FROM known_hosts ORDER BY endpoint")?;
            let rows = stmt.query_map([], |r| {
                Ok(KnownHost {
                    endpoint: r.get(0)?,
                    fingerprint: r.get(1)?,
                    first_seen: parse_ts(&r.get::<_, String>(2)?),
                    last_seen: parse_ts(&r.get::<_, String>(3)?),
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    pub fn forget_host(&self, endpoint: &str) -> DbResult<()> {
        self.with(|c| {
            c.execute("DELETE FROM known_hosts WHERE endpoint = ?1", params![endpoint])?;
            Ok(())
        })
    }
}
