//! Update-Prüfung über die GitHub-Releases von Remora.
//!
//! Fragt `releases/latest` des öffentlichen Repositorys ab (ohne Anmeldung, ohne Nutzerdaten;
//! User-Agent nur `Remora-RDP/<Version>`). Bei einer neueren Version wird nur der Link zur
//! Release-Seite angeboten – heruntergeladen oder ausgeführt wird nichts automatisch.

use std::time::Duration;

use semver::Version;

/// Repository, dessen Releases geprüft werden.
pub const REPOSITORY: &str = "MarcelRuh/remora";
const API_URL: &str = "https://api.github.com/repos/MarcelRuh/remora/releases/latest";
const RELEASE_PAGE_PREFIX: &str = "https://github.com/MarcelRuh/remora/releases/";

/// Eine verfügbare neuere Version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: Version,
    /// Release-Seite auf GitHub (nur URLs unterhalb von [`RELEASE_PAGE_PREFIX`]).
    pub url: String,
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Liest eine Versionsangabe wie `v0.2.0` oder `0.2.0`.
pub fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
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
    Ok((version > *current && version.pre.is_empty()).then_some(UpdateInfo {
        version,
        url: release.html_url,
    }))
}

/// Prüft (blockierend, mit eigener kleiner Laufzeit) auf eine neuere Version.
/// `Ok(None)`: aktuell oder noch kein Release veröffentlicht.
pub fn check(current_version: &str) -> anyhow::Result<Option<UpdateInfo>> {
    let current = Version::parse(current_version)?;
    // reqwest nutzt rustls ohne eigenen Kryptoanbieter; aws-lc-rs ist bereits im Projekt
    // (sspi/IronRDP). Ist schon ein Anbieter installiert, bleibt dieser erhalten.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let body = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent(concat!("Remora-RDP/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        let response = client
            .get(API_URL)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok::<_, anyhow::Error>(None);
        }
        let response = response.error_for_status()?;
        Ok(Some(response.text().await?))
    })?;
    match body {
        Some(json) => evaluate(&current, &json),
        None => Ok(None),
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
}
