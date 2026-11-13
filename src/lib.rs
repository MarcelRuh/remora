//! Remora – Remote Connection Manager für RDP und SSH. Bibliotheksteil (für Tests und die Binärdatei).
//!
//! Schichten:
//! * [`database`] – SQLite-Persistenz (Ordner, Verbindungen, vertraute Zertifikate)
//! * [`security`] – Credential Manager, TOFU-Zertifikatsprüfung, verschlüsselter Vault
//! * [`settings`] – Anwendungseinstellungen (TOML)
//! * [`services`] – Import/Export (`.rdp`, JSON, Vault)
//! * [`rdp`] – austauschbare Sitzungsschicht, IronRDP-Backend, Rendering, Eingabe
//! * [`ssh`] – SSH-Sitzungen (russh) mit eingebautem Terminal (alacritty_terminal)
//! * [`core`] – Fachlogik (Verbindungs- und Sitzungsverwaltung, Ereignisbus)
//! * [`app`] – Slint-Oberfläche und Verdrahtung

pub mod app;
pub mod core;
pub mod database;
pub mod rdp;
pub mod security;
pub mod services;
pub mod settings;
pub mod ssh;

pub mod ui {
    slint::include_modules!();
}
