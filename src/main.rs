// Im Release-Build kein Konsolenfenster öffnen.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use anyhow::Context as _;
use remora::database::Database;
use remora::security::credentials;
use remora::settings::{AppPaths, AppSettings};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

fn init_logging(paths: &AppPaths) -> tracing_appender::non_blocking::WorkerGuard {
    let _ = std::fs::create_dir_all(&paths.log_dir);
    let file = tracing_appender::rolling::daily(&paths.log_dir, "remora.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    // Standard: eigene Module auf "info", IronRDP auf "warn". Überschreibbar mit REMORA_LOG.
    let filter = EnvFilter::try_from_env("REMORA_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,ironrdp=warn,ironrdp_client=warn,sspi=warn,wgpu=warn,winit=warn"));
    let registry = tracing_subscriber::registry().with(filter).with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_target(true),
    );
    #[cfg(debug_assertions)]
    let registry = registry.with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    registry.init();
    guard
}

fn main() -> anyhow::Result<()> {
    let paths = AppPaths::resolve()?;
    let _log_guard = init_logging(&paths);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), data = %paths.data_dir.display(), "Remora startet");

    std::panic::set_hook(Box::new(|info| {
        tracing::error!(%info, "Unerwarteter Fehler (Panic)");
    }));

    // `remora.exe verbindung.rdp` importiert und öffnet die Datei (absolute Pfade, damit eine
    // bereits laufende Instanz sie unabhängig vom Arbeitsverzeichnis findet)
    let startup_files: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rdp")))
        .map(|p| std::path::absolute(&p).unwrap_or(p))
        .collect();

    // Nur eine Instanz pro Windows-Sitzung: sonst Dateien an die laufende übergeben und enden.
    let instance = match remora::app::instance::acquire(&startup_files) {
        remora::app::instance::Startup::Primary(listener) => listener,
        remora::app::instance::Startup::Forwarded => {
            tracing::info!(files = startup_files.len(), "An laufende Instanz übergeben");
            return Ok(());
        }
    };

    let settings = AppSettings::load(&paths.settings());
    let db = Arc::new(
        Database::open(&paths.database())
            .with_context(|| format!("Datenbank {} konnte nicht geöffnet werden", paths.database().display()))?,
    );
    let creds: Arc<dyn credentials::CredentialStore> = Arc::from(credentials::platform_store());

    let result = remora::app::run(paths, settings, db, creds, startup_files, instance);
    if let Err(e) = &result {
        tracing::error!(error = %format!("{e:#}"), "Anwendung mit Fehler beendet");
    }
    tracing::info!("Remora beendet");
    result
}
