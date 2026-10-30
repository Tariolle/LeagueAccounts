//! New-version check against the GitHub releases of the app's repository, and
//! download and integrity check of the release executable.
//!
//! Releases are trusted because only the repository's maintainer can publish
//! them; executables come only from its release assets over HTTPS, and must
//! match the SHA-256 digest GitHub computed when they were uploaded.

use crate::logging::Reason;
use scraper::{Html, Selector};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

/// `owner/name` of this build's GitHub origin, or the official repository for source archives.
pub const REPOSITORY: &str = env!("LEAGUEACCOUNTS_UPDATE_REPOSITORY");
/// The release asset that replaces the running executable.
const EXECUTABLE_ASSET: &str = "LeagueAccounts.exe";
const MAX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub name: String,
    pub url: String,
    pub notes: String,
    pub published_at: String,
    /// The executable, when the release carries one. Not sent to the UI.
    #[serde(skip)]
    pub executable: Option<Download>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub url: String,
    /// Lowercase hex SHA-256 from the asset's `digest`.
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug)]
pub enum UpdateError {
    Request(reqwest::Error),
    Io(io::Error),
    /// Not the release, asset or size that was announced.
    Response,
    /// The download does not match the release's SHA-256 digest.
    Checksum,
}

impl UpdateError {
    /// The log category; never the error text, which can contain URLs or paths.
    pub fn reason(&self) -> Reason {
        match self {
            Self::Request(error) => Reason::from_request(error),
            Self::Io(error) => Reason::from_error(error),
            Self::Response => Reason::ResponseBody,
            Self::Checksum => Reason::Checksum,
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Response => f.write_str("unexpected release download"),
            Self::Checksum => f.write_str("the download does not match the release"),
        }
    }
}

impl From<reqwest::Error> for UpdateError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<io::Error> for UpdateError {
    fn from(error: io::Error) -> Self {
        // Body reads wrap the request error (a timeout, a reset) in io::Error.
        if !error.get_ref().is_some_and(|inner| inner.is::<reqwest::Error>()) {
            return Self::Io(error);
        }
        match error.into_inner().map(|inner| inner.downcast::<reqwest::Error>()) {
            Some(Ok(inner)) => Self::Request(*inner),
            _ => Self::Response,
        }
    }
}

/// Parse "v3.1.0" / "3.1" / "3.1.0-beta" into comparable numbers.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Only release pages of `REPOSITORY` may be opened from update notices.
pub fn is_release_url(url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{REPOSITORY}/releases/"))
}

/// Executables are downloaded only from `REPOSITORY`'s release assets.
fn is_asset_url(url: &str) -> bool {
    if url.starts_with(&format!("https://github.com/{REPOSITORY}/releases/download/")) {
        return true;
    }
    #[cfg(debug_assertions)]
    if let Some(feed) = debug_feed() {
        return feed.rsplit_once('/').is_some_and(|(base, _)| url.starts_with(&format!("{base}/")));
    }
    false
}

/// Debug builds only: `LEAGUEACCOUNTS_UPDATE_FEED`, a releases/latest JSON URL,
/// lets a local server stand in for GitHub.
#[cfg(debug_assertions)]
fn debug_feed() -> Option<String> {
    std::env::var("LEAGUEACCOUNTS_UPDATE_FEED").ok().filter(|value| !value.trim().is_empty())
}

/// Debug builds replace their own executable only when pointed at a test feed.
pub fn can_install() -> bool {
    #[cfg(debug_assertions)]
    if debug_feed().is_none() {
        return false;
    }
    true
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, UpdateError> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("LeagueAccounts/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()?)
}

/// The latest published (non-draft, non-prerelease) release, if any.
pub fn latest_release() -> Result<Option<Release>, UpdateError> {
    let api_url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    #[cfg(debug_assertions)]
    let debug_feed_url = debug_feed();
    #[cfg(not(debug_assertions))]
    let debug_feed_url: Option<String> = None;
    let url = debug_feed_url.clone().unwrap_or(api_url);
    let client = client(Duration::from_secs(15))?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()?;
    if debug_feed_url.is_none()
        && matches!(
            response.status(),
            reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::TOO_MANY_REQUESTS
        )
    {
        return latest_release_from_pages(&client);
    }
    // 404: the repository has no published release yet.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = response.error_for_status()?.text()?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| UpdateError::Response)?;
    Ok(parse_release(&json))
}

/// GitHub's unauthenticated REST quota is shared by many users behind the same
/// address. The public release pages expose the latest tag and uploaded asset
/// digest without consuming that API quota.
fn latest_release_from_pages(
    client: &reqwest::blocking::Client,
) -> Result<Option<Release>, UpdateError> {
    let latest_url = format!("https://github.com/{REPOSITORY}/releases/latest");
    let response = client.get(latest_url).send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release_url = response.url().as_str().to_owned();
    let page = response.error_for_status()?.text()?;
    let Some(tag) = release_tag(&release_url) else {
        return Err(UpdateError::Response);
    };
    let tag_url = format!("https://github.com/{REPOSITORY}/releases/tag/{tag}");
    let document = Html::parse_document(&page);
    let name = Selector::parse("h1")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|heading| heading.text().collect::<String>().trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| tag.to_owned());
    let notes = Selector::parse(".markdown-body")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|body| body.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect::<String>();
    let published_at = Selector::parse("relative-time[datetime]")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .and_then(|time| time.value().attr("datetime"))
        .unwrap_or_default()
        .to_owned();

    let assets_url = format!("https://github.com/{REPOSITORY}/releases/expanded_assets/{tag}");
    let assets_response = client.get(assets_url).send()?;
    let asset_rows = if assets_response.status().is_success() {
        Some(assets_response.text()?)
    } else {
        None
    };
    let executable = asset_rows
        .as_deref()
        .and_then(parse_expanded_executable)
        .and_then(|asset| {
            let response = client
                .head(&asset.url)
                .send()
                .ok()?
                .error_for_status()
                .ok()?;
            let size = response
                .content_length()
                .filter(|size| (1..=MAX_EXECUTABLE_BYTES).contains(size))?;
            Some(Download {
                url: asset.url,
                sha256: asset.sha256,
                size,
            })
        });

    Ok(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_owned(),
        name,
        url: tag_url,
        notes,
        published_at,
        executable,
    }))
}

fn release_tag(url: &str) -> Option<&str> {
    let prefix = format!("https://github.com/{REPOSITORY}/releases/tag/");
    let tag = url.strip_prefix(&prefix)?.split(['?', '#']).next()?;
    let safe = !tag.is_empty()
        && tag.len() <= 128
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'));
    (safe && parse_version(tag).is_some()).then_some(tag)
}

struct ExpandedExecutable {
    url: String,
    sha256: String,
}

fn parse_expanded_executable(html: &str) -> Option<ExpandedExecutable> {
    static ROWS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("li.Box-row").expect("valid release asset row selector"));
    static LINKS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("a[href]").expect("valid release asset link selector"));
    static DIGEST: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"sha256:([0-9a-fA-F]{64})").expect("valid release digest regex")
    });
    let document = Html::parse_document(html);
    for row in document.select(&ROWS) {
        let link = row
            .select(&LINKS)
            .find(|link| link.text().collect::<String>().trim() == EXECUTABLE_ASSET);
        let Some(href) = link.and_then(|link| link.value().attr("href")) else {
            continue;
        };
        let url = if href.starts_with("https://github.com/") {
            href.to_owned()
        } else if href.starts_with('/') {
            format!("https://github.com{href}")
        } else {
            continue;
        };
        if !is_asset_url(&url) {
            continue;
        }
        let text = row.text().collect::<String>();
        let sha256 = DIGEST
            .captures(&text)?
            .get(1)?
            .as_str()
            .to_ascii_lowercase();
        return Some(ExpandedExecutable { url, sha256 });
    }
    None
}

fn parse_release(json: &serde_json::Value) -> Option<Release> {
    let tag = json["tag_name"].as_str().unwrap_or_default();
    let url = json["html_url"].as_str().unwrap_or_default();
    if parse_version(tag).is_none() || !is_release_url(url) {
        return None;
    }
    Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_owned(),
        name: json["name"].as_str().unwrap_or(tag).to_owned(),
        url: url.to_owned(),
        notes: json["body"].as_str().unwrap_or_default().chars().take(2000).collect(),
        published_at: json["published_at"].as_str().unwrap_or_default().to_owned(),
        executable: executable_download(json),
    })
}

fn executable_download(json: &serde_json::Value) -> Option<Download> {
    let executable = json["assets"].as_array()?.iter().find(|asset| {
        asset["name"].as_str() == Some(EXECUTABLE_ASSET)
            && asset["state"].as_str().is_none_or(|state| state == "uploaded")
    })?;
    let url = executable["browser_download_url"].as_str().filter(|url| is_asset_url(url))?;
    let size = executable["size"]
        .as_u64()
        .filter(|size| (1..=MAX_EXECUTABLE_BYTES).contains(size))?;
    let sha256 = executable["digest"]
        .as_str()?
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))?
        .to_ascii_lowercase();
    Some(Download {
        url: url.to_owned(),
        sha256,
        size,
    })
}

/// Stream the executable into `file`, reporting the bytes received so far.
pub fn download(download: &Download, file: &mut File, mut progress: impl FnMut(u64)) -> Result<(), UpdateError> {
    // Per read, not for the whole transfer: slow connections still finish.
    let mut response = client(Duration::from_secs(30))?
        .get(&download.url)
        .send()?
        .error_for_status()?;
    if response.content_length().is_some_and(|length| length != download.size) {
        return Err(UpdateError::Response);
    }
    let mut buffer = vec![0; 64 * 1024];
    let mut received = 0;
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        received += read as u64;
        if received > download.size {
            return Err(UpdateError::Response);
        }
        file.write_all(&buffer[..read])?;
        progress(received);
    }
    if received != download.size {
        return Err(UpdateError::Response);
    }
    file.sync_all()?;
    Ok(())
}

/// Check the file on disk, the bytes that will be installed, against the
/// release's SHA-256 digest.
pub fn verify_file(path: &Path, sha256: &str) -> Result<(), UpdateError> {
    verify(File::open(path)?, sha256)
}

fn verify(mut data: impl Read, sha256: &str) -> Result<(), UpdateError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = data.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if format!("{:x}", hasher.finalize()) != sha256 {
        return Err(UpdateError::Checksum);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// SHA-256 of "League Accounts".
    const DIGEST: &str = "8c73b4c826f32e9405326642db2d25f5013533bed5b4ec63d63535bdcfb7df65";

    #[test]
    fn compares_versions() {
        assert!(is_newer("v3.1.0", "3.0.0"));
        assert!(is_newer("3.0.1", "3.0.0"));
        assert!(is_newer("4", "3.9.9"));
        assert!(!is_newer("v3.0.0", "3.0.0"));
        assert!(!is_newer("2.0.3", "3.0.0"));
        assert!(!is_newer("3.1.0-beta", "3.1.0"));
        assert!(!is_newer("latest", "3.0.0"));
    }

    #[test]
    fn only_own_release_pages_are_allowed() {
        assert!(is_release_url(&format!("https://github.com/{REPOSITORY}/releases/tag/v3.1.0")));
        assert!(!is_release_url("https://github.com/someone/else/releases/tag/v9"));
        assert!(!is_release_url("https://evil.example/https://github.com/"));
    }

    #[test]
    fn accepts_only_a_safe_version_tag_from_the_own_latest_release_redirect() {
        assert_eq!(
            release_tag(&format!(
                "https://github.com/{REPOSITORY}/releases/tag/v3.4.1"
            )),
            Some("v3.4.1")
        );
        assert!(release_tag("https://github.com/someone/else/releases/tag/v3.4.1").is_none());
        assert!(release_tag(&format!(
            "https://github.com/{REPOSITORY}/releases/tag/v3.4.1/else"
        ))
        .is_none());
    }

    #[test]
    fn parses_the_executable_digest_from_the_rate_limit_fallback_asset_page() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let page = format!(
            r#"<ul><li class="Box-row"><a href="/{REPOSITORY}/releases/download/v3.4.1/LeagueAccounts.exe">LeagueAccounts.exe</a><span>{digest}</span></li><li class="Box-row"><a href="/{REPOSITORY}/releases/download/v3.4.1/Source.zip">Source.zip</a><span>{digest}</span></li></ul>"#
        );
        let parsed = parse_expanded_executable(&page).unwrap();
        assert_eq!(
            parsed.url,
            format!("https://github.com/{REPOSITORY}/releases/download/v3.4.1/LeagueAccounts.exe")
        );
        assert_eq!(parsed.sha256, "a".repeat(64));
        assert!(parse_expanded_executable(&page.replace(&digest, "sha512:bad")).is_none());
    }

    fn asset(name: &str, url: &str, size: u64, digest: &str) -> serde_json::Value {
        json!({ "name": name, "state": "uploaded", "size": size, "browser_download_url": url, "digest": digest })
    }

    fn release(assets: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
            "tag_name": "v3.1.0",
            "name": "v3.1.0",
            "html_url": format!("https://github.com/{REPOSITORY}/releases/tag/v3.1.0"),
            "body": "Notes",
            "published_at": "2026-10-12T00:00:00Z",
            "assets": assets,
        })
    }

    #[test]
    fn selects_the_executable_and_its_digest_from_own_release_assets() {
        let base = format!("https://github.com/{REPOSITORY}/releases/download/v3.1.0");
        let parsed = parse_release(&release(vec![
            asset("LeagueAccounts-setup.exe", &format!("{base}/LeagueAccounts-setup.exe"), 9, "sha256:00"),
            asset(EXECUTABLE_ASSET, &format!("{base}/{EXECUTABLE_ASSET}"), 12_000_000, &format!("sha256:{}", DIGEST.to_uppercase())),
        ]))
        .unwrap();
        assert_eq!(parsed.version, "3.1.0");
        assert_eq!(
            parsed.executable,
            Some(Download {
                url: format!("{base}/{EXECUTABLE_ASSET}"),
                sha256: DIGEST.to_owned(),
                size: 12_000_000,
            })
        );
        // The UI never receives download URLs.
        assert!(!serde_json::to_string(&parsed).unwrap().contains("download"));
    }

    #[test]
    fn releases_without_a_verifiable_executable_only_notify() {
        let base = format!("https://github.com/{REPOSITORY}/releases/download/v3.1.0");
        let url = format!("{base}/{EXECUTABLE_ASSET}");
        let digest = format!("sha256:{DIGEST}");
        let mut uploading = asset(EXECUTABLE_ASSET, &url, 12_000_000, &digest);
        uploading["state"] = json!("new");
        let mut undigested = asset(EXECUTABLE_ASSET, &url, 12_000_000, &digest);
        undigested.as_object_mut().unwrap().remove("digest");
        for executable in [
            asset(EXECUTABLE_ASSET, "https://evil.example/LeagueAccounts.exe", 12_000_000, &digest),
            asset(EXECUTABLE_ASSET, &url, 0, &digest),
            asset(EXECUTABLE_ASSET, &url, 12_000_000, &format!("sha512:{DIGEST}")),
            asset(EXECUTABLE_ASSET, &url, 12_000_000, "sha256:not-hex"),
            uploading,
            undigested,
        ] {
            let parsed = parse_release(&release(vec![executable])).unwrap();
            assert_eq!(parsed.version, "3.1.0");
            assert_eq!(parsed.executable, None);
        }
    }

    #[test]
    fn installs_only_a_download_matching_the_digest() {
        verify(&b"League Accounts"[..], DIGEST).unwrap();
        assert!(matches!(verify(&b"League Accounts!"[..], DIGEST), Err(UpdateError::Checksum)));
        assert!(matches!(verify(&b""[..], DIGEST), Err(UpdateError::Checksum)));
    }
}
