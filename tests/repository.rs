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
