//! Test gegen den echten Windows Credential Manager (legt einen Testeintrag an und entfernt ihn).

#[cfg(windows)]
#[test]
fn windows_credential_manager_roundtrip() {
    use remora::security::credentials::{CredentialKey, Secret, platform_store};

    let store = platform_store();
    // Negative ID kollidiert nie mit echten Verbindungen
    let key = CredentialKey::password(-424_242);
    let _ = store.delete(key);
    assert!(store.get(key).unwrap().is_none());

    store.set(key, "testuser", &Secret::new("Pässwört-ÄÖÜ-€-123")).unwrap();
    let value = store.get(key).unwrap().expect("Eintrag muss existieren");
    assert_eq!(value.expose(), "Pässwört-ÄÖÜ-€-123");

    store.delete(key).unwrap();
    assert!(store.get(key).unwrap().is_none());
    // Doppeltes Löschen ist kein Fehler
    store.delete(key).unwrap();
}
