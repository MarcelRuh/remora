//! Remora RDP Manager – Bibliotheksteil (für Tests und die Binärdatei).
//!
//! Schichten:
//! * [`database`] – SQLite-Persistenz (Ordner, Verbindungen, vertraute Zertifikate)
//! * [`security`] – Credential Manager, TOFU-Zertifikatsprüfung, verschlüsselter Vault
//! * [`settings`] – Anwendungseinstellungen (TOML)
//! * [`services`] – Import/Export (`.rdp`, JSON, Vault)
//! * [`rdp`] – austauschbare RDP-Schicht, IronRDP-Backend, Rendering, Eingabe
//! * [`core`] – Fachlogik (Verbindungs- und Sitzungsverwaltung, Ereignisbus)
//! * [`app`] – Slint-Oberfläche und Verdrahtung

pub mod app;
pub mod core;
pub mod database;
pub mod rdp;
pub mod security;
pub mod services;
pub mod settings;

pub mod ui {
    slint::include_modules!();
}
