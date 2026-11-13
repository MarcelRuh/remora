//! Integrationstests der Persistenzschicht gegen eine echte SQLite-Datei.

use remora::database::{Connection, Database, Resolution};

fn temp_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("test.sqlite3")).unwrap();
    (dir, db)
}

#[test]
fn crud_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("persist.sqlite3");
    let id = {
        let db = Database::open(&path).unwrap();
        let folder = db.create_folder("Produktion", None).unwrap();
        let mut c = Connection::new("Web 01", "web01.example");
        c.folder_id = Some(folder);
        c.tags = vec!["web".into(), "prod".into()];
        c.options.resolution = Resolution::Fixed {
            width: 1920,
            height: 1080,
        };
        c.options.custom_rdp_properties = "use multimon:i:0".into();
        db.insert_connection(&c).unwrap()
    };
    // Nach erneutem Öffnen ist alles noch da
    let db = Database::open(&path).unwrap();
    let c = db.get_connection(id).unwrap();
    assert_eq!(c.name, "Web 01");
    assert_eq!(c.tags, vec!["web".to_owned(), "prod".to_owned()]);
    assert_eq!(
        c.options.resolution,
        Resolution::Fixed {
            width: 1920,
            height: 1080
        }
    );
    assert_eq!(c.options.custom_rdp_properties, "use multimon:i:0");
    assert_eq!(db.list_folders().unwrap().len(), 1);
}

#[test]
fn update_delete_duplicate_favorite_recent() {
    let (_dir, db) = temp_db();
    let mut c = Connection::new("A", "a.example");
    c.id = db.insert_connection(&c).unwrap();
    c.description = "Beschreibung".into();
    c.port = 4000;
    db.update_connection(&c).unwrap();
    assert_eq!(db.get_connection(c.id).unwrap().port, 4000);

    db.set_favorite(c.id, true).unwrap();
    assert!(db.get_connection(c.id).unwrap().favorite);

    let dup = db.duplicate_connection(c.id).unwrap();
    let dup_conn = db.get_connection(dup).unwrap();
    assert_eq!(dup_conn.name, "A (Kopie)");
    assert!(!dup_conn.favorite);

    assert!(db.recent_connections(10).unwrap().is_empty());
    db.touch_last_used(dup).unwrap();
    let recent = db.recent_connections(10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].id, dup);

    db.delete_connection(c.id).unwrap();
    assert!(db.get_connection(c.id).is_err());
    assert_eq!(db.list_connections().unwrap().len(), 1);
}

#[test]
fn folder_hierarchy_rules() {
    let (_dir, db) = temp_db();
    let root = db.create_folder("Root", None).unwrap();
    let child = db.create_folder("Child", Some(root)).unwrap();
    let grandchild = db.create_folder("Grandchild", Some(child)).unwrap();

    // Zyklen werden verhindert
    assert!(db.move_folder(root, Some(grandchild)).is_err());
    assert!(db.move_folder(root, Some(root)).is_err());
    // Gültiges Verschieben
    db.move_folder(grandchild, None).unwrap();

    let mut c = Connection::new("In Child", "x");
    c.folder_id = Some(child);
    let cid = db.insert_connection(&c).unwrap();

    // Ordner löschen verschiebt Verbindungen nach oben statt sie zu löschen
    db.delete_folder(root).unwrap();
    let folders = db.list_folders().unwrap();
    assert_eq!(folders.len(), 1); // nur noch "Grandchild"
    assert_eq!(db.get_connection(cid).unwrap().folder_id, None);

    db.rename_folder(grandchild, "Neu").unwrap();
    assert_eq!(db.list_folders().unwrap()[0].name, "Neu");
    assert!(db.create_folder("   ", None).is_err());
}

#[test]
fn invalid_connections_are_rejected() {
    let (_dir, db) = temp_db();
    assert!(db.insert_connection(&Connection::new("", "host")).is_err());
    assert!(db.insert_connection(&Connection::new("Name", "")).is_err());
    assert!(db.insert_connection(&Connection::new("Name", "bad host")).is_err());
}

#[test]
fn known_hosts() {
    let (_dir, db) = temp_db();
    assert!(db.known_host("srv:3389").unwrap().is_none());
    db.trust_host("srv:3389", "AA:BB").unwrap();
    assert_eq!(db.known_host("srv:3389").unwrap().unwrap().fingerprint, "AA:BB");
    db.trust_host("srv:3389", "CC:DD").unwrap();
    assert_eq!(db.known_host("srv:3389").unwrap().unwrap().fingerprint, "CC:DD");
    assert_eq!(db.list_known_hosts().unwrap().len(), 1);
    db.forget_host("srv:3389").unwrap();
    assert!(db.known_host("srv:3389").unwrap().is_none());
}

#[test]
fn no_password_column_in_schema() {
    let (dir, db) = temp_db();
    drop(db);
    let conn = rusqlite_check(&dir.path().join("test.sqlite3"));
    assert!(
        !conn.to_lowercase().contains("password"),
        "Schema darf keine Passwortspalte enthalten: {conn}"
    );
}

fn rusqlite_check(path: &std::path::Path) -> String {
    // Schema als Text auslesen (über die öffentliche API nicht zugänglich → Rohdatei lesen)
    let bytes = std::fs::read(path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter(|l| l.contains("CREATE TABLE"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ssh_connections_round_trip() {
    use remora::database::{Protocol, SshAuth};
    let (_dir, db) = temp_db();
    let mut c = Connection::new_ssh("CT 101", "10.0.0.101");
    c.username = "root".into();
    c.options.ssh.auth = SshAuth::KeyFile;
    c.options.ssh.key_path = r"C:\Users\me\.ssh\id_ed25519".into();
    let id = db.insert_connection(&c).unwrap();
    let back = db.get_connection(id).unwrap();
    assert_eq!(back.protocol, Protocol::Ssh);
    assert_eq!(back.port, 22);
    assert_eq!(back.options.ssh, c.options.ssh);
    // Protokollwechsel wird gespeichert
    let mut changed = back.clone();
    changed.set_protocol(Protocol::Rdp);
    db.update_connection(&changed).unwrap();
    let again = db.get_connection(id).unwrap();
    assert_eq!((again.protocol, again.port), (Protocol::Rdp, 3389));
}

#[test]
fn version_1_database_is_migrated() {
    // Datenbank im Schema von 0.2.x (ohne Protokollspalte) anlegen
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alt.sqlite3");
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch(
            "CREATE TABLE folders (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                 parent_id INTEGER, sort_order INTEGER NOT NULL DEFAULT 0, expanded INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE connections (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, host TEXT NOT NULL,
                 port INTEGER NOT NULL DEFAULT 3389, username TEXT NOT NULL DEFAULT '', domain TEXT NOT NULL DEFAULT '',
                 description TEXT NOT NULL DEFAULT '', folder_id INTEGER, tags TEXT NOT NULL DEFAULT '[]',
                 color TEXT NOT NULL DEFAULT '#4c8dff', icon TEXT NOT NULL DEFAULT 'monitor',
                 favorite INTEGER NOT NULL DEFAULT 0, options TEXT NOT NULL DEFAULT '{}',
                 sort_order INTEGER NOT NULL DEFAULT 0, last_used TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE known_hosts (endpoint TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
                 first_seen TEXT NOT NULL, last_seen TEXT NOT NULL);
             INSERT INTO connections (name, host, created_at, updated_at)
                 VALUES ('DC01', 'dc01', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let db = Database::open(&path).unwrap();
    let all = db.list_connections().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].protocol, remora::database::Protocol::Rdp);
    assert_eq!(all[0].port, 3389);
}
