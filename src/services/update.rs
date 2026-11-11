//! Update über die GitHub-Releases von Remora.
//!
//! * **Prüfung:** `releases/latest` des öffentlichen Repositorys (ohne Anmeldung, ohne
//!   Nutzerdaten; User-Agent nur `Remora-RDP/<Version>`).
//! * **Download:** Setup-Datei und `SHA256SUMS.txt` aus demselben Release, nur über HTTPS und nur
//!   von `github.com/MarcelRuh/remora/releases/download/…` (Weiterleitungen nur auf HTTPS).
//!   Das Setup wird ausschließlich ausgeführt, wenn seine SHA-256-Prüfsumme zur Liste passt.
//!   Die Binärdateien sind nicht signiert; die Prüfsumme schützt gegen beschädigte oder
//!   unvollständige Downloads, nicht gegen ein kompromittiertes GitHub-Konto.
//! * **Installation:** startet das Setup still (`/SILENT`, UAC-Abfrage durch Windows); das Setup
//!   beendet Remora und startet danach die neue Version mit normalen Benutzerrechten.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use sha2::{Digest, Sha256};

/// Repository, dessen Releases geprüft werden.
pub const REPOSITORY: &str = "MarcelRuh/remora";
const API_URL: &str = "https://api.github.com/repos/MarcelRuh/remora/releases/latest";
const RELEASE_PAGE_PREFIX: &str = "https://github.com/MarcelRuh/remora/releases/";
const DOWNLOAD_PREFIX: &str = "https://github.com/MarcelRuh/remora/releases/download/";
/// Name der Prüfsummenliste im Release.
pub const CHECKSUMS_NAME: &str = "SHA256SUMS.txt";
/// Obergrenze für die Setup-Datei (Schutz vor unsinnig großen Downloads).
const MAX_SETUP_BYTES: u64 = 300 * 1024 * 1024;

/// Eine herunterladbare Datei eines Releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// Eine verfügbare neuere Version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: Version,
    /// Release-Seite auf GitHub (nur URLs unterhalb von [`RELEASE_PAGE_PREFIX`]).
    pub url: String,
    /// Setup-Datei (`Remora-RDP-Setup-<Version>-x64.exe`), falls im Release vorhanden.
    pub installer: Option<Asset>,
    /// Prüfsummenliste, falls im Release vorhanden.
    pub checksums: Option<Asset>,
}

impl UpdateInfo {
    /// Kann dieses Update direkt installiert werden (Setup und Prüfsummen vorhanden)?
    pub fn installable(&self) -> bool {
        self.installer.is_some() && self.checksums.is_some()
    }
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(serde::Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Liest eine Versionsangabe wie `v0.2.0` oder `0.2.0`.
pub fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
}

/// Dateiname des Setups für eine Version.
pub fn installer_name(version: &Version) -> String {
    format!("Remora-RDP-Setup-{version}-x64.exe")
}

fn asset(release: &Release, name: &str) -> Option<Asset> {
    release
        .assets
        .iter()
        .find(|a| a.name == name && a.browser_download_url.starts_with(DOWNLOAD_PREFIX))
        .map(|a| Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
}

/// Bewertet die API-Antwort: `Some`, wenn sie eine neuere, reguläre Version beschreibt.
fn evaluate(current: &Version, json: &str) -> anyhow::Result<Option<UpdateInfo>> {
    let release: Release = serde_json::from_str(json)?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(version) = parse_tag(&release.tag_name) else {
        anyhow::bail!("unbekanntes Versionsformat: {}", release.tag_name);
    };
    if !release.html_url.starts_with(RELEASE_PAGE_PREFIX) {
        anyhow::bail!("unerwartete Release-Adresse");
    }
    if version <= *current || !version.pre.is_empty() {
        return Ok(None);
    }
    Ok(Some(UpdateInfo {
        installer: asset(&release, &installer_name(&version)),
        checksums: asset(&release, CHECKSUMS_NAME),
        url: release.html_url,
        version,
    }))
}

/// Sucht die Prüfsumme einer Datei in einer `sha256sum`-Liste (`<hex>  <name>` bzw. `<hex> *<name>`).
pub fn checksum_for(list: &str, file_name: &str) -> Option<String> {
    list.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == file_name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn client(timeout: Duration) -> anyhow::Result<reqwest::Client> {
    // reqwest nutzt rustls ohne eigenen Kryptoanbieter; aws-lc-rs ist bereits im Projekt
    // (sspi/IronRDP). Ist schon ein Anbieter installiert, bleibt dieser erhalten.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    // Weiterleitungen (GitHub → Objektspeicher) nur auf HTTPS
    let redirects = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            attempt.error("Weiterleitung ohne HTTPS abgelehnt")
        } else if attempt.previous().len() > 5 {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Remora-RDP/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .redirect(redirects)
        .https_only(true)
        .build()?)
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

/// Prüft (blockierend, mit eigener kleiner Laufzeit) auf eine neuere Version.
/// `Ok(None)`: aktuell oder noch kein Release veröffentlicht.
pub fn check(current_version: &str) -> anyhow::Result<Option<UpdateInfo>> {
    let current = Version::parse(current_version)?;
    let body = runtime()?.block_on(async {
        let response = client(Duration::from_secs(15))?
            .get(API_URL)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok::<_, anyhow::Error>(None);
        }
        Ok(Some(response.error_for_status()?.text().await?))
    })?;
    match body {
        Some(json) => evaluate(&current, &json),
        None => Ok(None),
    }
}

/// Lädt das Setup nach `dir`, prüft die SHA-256-Summe und liefert den Pfad.
/// `progress(geladen, gesamt)` wird während des Downloads aufgerufen.
pub fn download(info: &UpdateInfo, dir: &Path, mut progress: impl FnMut(u64, u64)) -> anyhow::Result<PathBuf> {
    let (Some(installer), Some(checksums)) = (&info.installer, &info.checksums) else {
        anyhow::bail!("Das Release enthält kein Setup mit Prüfsumme");
    };
    std::fs::create_dir_all(dir)?;
    let target = dir.join(&installer.name);
    let partial = dir.join(format!("{}.part", installer.name));
    let result = runtime()?.block_on(async {
        let http = client(Duration::from_secs(600))?;
        let list = http
            .get(&checksums.url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let Some(expected) = checksum_for(&list, &installer.name) else {
            anyhow::bail!("Keine Prüfsumme für {} im Release", installer.name);
        };

        let mut response = http.get(&installer.url).send().await?.error_for_status()?;
        let total = response.content_length().unwrap_or(installer.size);
        if total > MAX_SETUP_BYTES {
            anyhow::bail!("Setup ist unerwartet groß ({total} Bytes)");
        }
        let mut file = std::fs::File::create(&partial)?;
        let mut hasher = Sha256::new();
        let mut loaded = 0u64;
        while let Some(chunk) = response.chunk().await? {
            loaded += chunk.len() as u64;
            if loaded > MAX_SETUP_BYTES {
                anyhow::bail!("Download überschreitet die erwartete Größe");
            }
            hasher.update(&chunk);
            file.write_all(&chunk)?;
            progress(loaded, total);
        }
        file.flush()?;
        drop(file);
        let actual = hex(&hasher.finalize());
        if actual != expected {
            anyhow::bail!("Prüfsumme stimmt nicht (erwartet {expected}, erhalten {actual})");
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(&target);
            std::fs::rename(&partial, &target)?;
            Ok(target)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// Startet das heruntergeladene Setup still; Windows fragt per UAC nach Administratorrechten.
/// `/RELAUNCH` lässt das Setup Remora danach wieder starten (siehe `installer/remora.iss`).
pub fn launch_installer(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};

        let file = HSTRING::from(path.as_os_str());
        let params = HSTRING::from("/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /RELAUNCH");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: Struktur vollständig initialisiert, Zeichenketten leben bis nach dem Aufruf.
        unsafe { ShellExecuteExW(&mut info)? };
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        anyhow::bail!("Automatische Installation nur unter Windows")
    }
}

/// Ist diese EXE eine per Setup installierte Kopie? (Sonst: portabel/Entwicklung → kein
/// automatisches Update, nur Link zur Release-Seite.)
pub fn running_installed_copy() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
        };
        use windows::core::w;

        let Ok(exe) = std::env::current_exe() else { return false };
        let Some(dir) = exe.parent().and_then(|d| d.canonicalize().ok()) else {
            return false;
        };
        let read = |root: HKEY| -> Option<PathBuf> {
            let mut buf = vec![0u16; 1024];
            let mut size = (buf.len() * 2) as u32;
            // SAFETY: Puffer und Größe passen zusammen; nur Lesezugriff.
            let result = unsafe {
                RegGetValueW(
                    root,
                    w!(
                        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{CAA54C2C-CCBC-48D9-9C2A-06929D662F8C}_is1"
                    ),
                    w!("InstallLocation"),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buf.as_mut_ptr().cast()),
                    Some(&mut size),
                )
            };
            if result.is_err() {
                return None;
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            PathBuf::from(String::from_utf16_lossy(&buf[..len])).canonicalize().ok()
        };
        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
            .into_iter()
            .filter_map(read)
            .any(|location| location == dir)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn release(tag: &str, url: &str, prerelease: bool) -> String {
        serde_json::json!({ "tag_name": tag, "html_url": url, "draft": false, "prerelease": prerelease }).to_string()
    }

    #[test]
    fn newer_release_is_offered() {
        let url = "https://github.com/MarcelRuh/remora/releases/tag/v0.2.0";
        let info = evaluate(&v("0.1.0"), &release("v0.2.0", url, false)).unwrap().unwrap();
        assert_eq!(info.version, v("0.2.0"));
        assert_eq!(info.url, url);
        assert!(!info.installable(), "ohne Assets nur Link zur Release-Seite");
    }

    #[test]
    fn installer_and_checksums_are_picked_from_assets() {
        let dl = "https://github.com/MarcelRuh/remora/releases/download/v0.3.0/";
        let json = serde_json::json!({
            "tag_name": "v0.3.0",
            "html_url": "https://github.com/MarcelRuh/remora/releases/tag/v0.3.0",
            "assets": [
                { "name": "Remora-RDP-v0.3.0-portable-x64.zip", "browser_download_url": format!("{dl}x.zip"), "size": 1 },
                { "name": "Remora-RDP-Setup-0.3.0-x64.exe", "browser_download_url": format!("{dl}Remora-RDP-Setup-0.3.0-x64.exe"), "size": 1234 },
                { "name": "SHA256SUMS.txt", "browser_download_url": format!("{dl}SHA256SUMS.txt"), "size": 100 }
            ]
        })
        .to_string();
        let info = evaluate(&v("0.2.1"), &json).unwrap().unwrap();
        assert!(info.installable());
        assert_eq!(info.installer.unwrap().size, 1234);
    }

    #[test]
    fn foreign_asset_urls_are_ignored() {
        let json = serde_json::json!({
            "tag_name": "v0.3.0",
            "html_url": "https://github.com/MarcelRuh/remora/releases/tag/v0.3.0",
            "assets": [
                { "name": "Remora-RDP-Setup-0.3.0-x64.exe", "browser_download_url": "https://evil.example/setup.exe", "size": 1 },
                { "name": "SHA256SUMS.txt", "browser_download_url": "https://github.com/MarcelRuh/remora/releases/download/v0.3.0/SHA256SUMS.txt", "size": 1 }
            ]
        })
        .to_string();
        let info = evaluate(&v("0.2.1"), &json).unwrap().unwrap();
        assert!(info.installer.is_none());
        assert!(!info.installable());
    }

    #[test]
    fn same_older_or_prerelease_is_ignored() {
        let url = "https://github.com/MarcelRuh/remora/releases/tag/x";
        assert_eq!(evaluate(&v("0.2.0"), &release("v0.2.0", url, false)).unwrap(), None);
        assert_eq!(evaluate(&v("0.2.0"), &release("0.1.9", url, false)).unwrap(), None);
        assert_eq!(evaluate(&v("0.1.0"), &release("v0.3.0", url, true)).unwrap(), None);
        assert_eq!(
            evaluate(&v("0.1.0"), &release("v0.3.0-beta.1", url, false)).unwrap(),
            None
        );
    }

    #[test]
    fn foreign_urls_and_bad_tags_are_rejected() {
        assert!(evaluate(&v("0.1.0"), &release("v9.0.0", "https://example.com/evil", false)).is_err());
        assert!(
            evaluate(
                &v("0.1.0"),
                &release("latest", "https://github.com/MarcelRuh/remora/releases/x", false)
            )
            .is_err()
        );
        assert_eq!(parse_tag(" V1.2.3 "), Some(v("1.2.3")));
    }

    #[test]
    fn checksum_list_parsing() {
        let hash = "a".repeat(64);
        let list = format!(
            "{}  Remora-RDP-v1-portable-x64.zip\n{} *Remora-RDP-Setup-1.0.0-x64.exe\nkaputt\n",
            "b".repeat(64),
            hash.to_uppercase()
        );
        assert_eq!(checksum_for(&list, "Remora-RDP-Setup-1.0.0-x64.exe"), Some(hash));
        assert_eq!(checksum_for(&list, "fehlt.exe"), None);
        assert_eq!(checksum_for("abc  x.exe", "x.exe"), None, "zu kurze Summe");
    }
}
