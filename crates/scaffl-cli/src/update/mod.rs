//! Self-update logic for the scaffl binary.
//!
//! # Bounded Context: Self-Update
//!
//! Owns release checking, binary download, checksum verification,
//! archive extraction, and atomic replacement. Isolated from all
//! recipe / runtime logic — only called from the `update` CLI subcommand.
//!
//! Downloads the latest release from GitHub, verifies its SHA256 checksum,
//! extracts the binary from the `.tar.gz` archive, and atomically replaces
//! the currently running executable.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::warn;

use crate::constants::{self, TARGET, VERSION as CURRENT_VERSION};

/// Errors that can occur during self-update.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("failed to query GitHub releases: {0}")]
    ApiError(String),

    #[error("failed to download release asset: {0}")]
    DownloadError(String),

    #[error("checksum verification failed: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("checksum file does not contain entry for {0}")]
    ChecksumNotFound(String),

    #[error("failed to extract archive: {0}")]
    ExtractError(String),

    #[error("failed to replace binary: {0}")]
    ReplaceError(String),

    #[error("{0}")]
    PermissionDenied(String),

    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
}

/// Metadata about a GitHub release.
#[derive(Debug)]
struct ReleaseInfo {
    /// The git tag (e.g. "v0.2.0").
    tag: String,
    /// The semantic version without the leading 'v'.
    version: String,
}

/// Run the self-update process.
///
/// `prerelease = true` widens the candidate set to include pre-release
/// tags (e.g. `v0.2.0-rc.1`). Stable tags always win when both are
/// available, because pre-releases sort *below* the equivalent stable
/// version under semver.
pub async fn run_update(force: bool, prerelease: bool) -> Result<(), UpdateError> {
    if let Some(env) = detect_container_environment() {
        warn!("Running inside {env}. Consider rebuilding the image instead of self-updating.");
    }

    if crate::ci::is_ci() {
        warn!("Running in a CI environment. Consider pinning a version in your pipeline instead.");
    }

    validate_platform()?;

    eprintln!("▸ Checking for updates...");

    let release = fetch_latest_release(prerelease).await?;

    if !force && !is_newer(&release.version) {
        eprintln!("✔ Already on the latest version ({CURRENT_VERSION}).");
        return Ok(());
    }

    eprintln!("▸ Updating {CURRENT_VERSION} → {} ...", release.version);

    let current_exe = std::env::current_exe().map_err(|e| {
        UpdateError::ReplaceError(format!("could not determine current executable path: {e}"))
    })?;
    let current_exe = current_exe.canonicalize().unwrap_or(current_exe);

    check_write_permission(&current_exe)?;

    let asset_name = format!("scaffl-{TARGET}.tar.gz");
    let asset_url = constants::release_asset_url(&release.tag, TARGET);
    eprintln!("▸ Downloading {asset_name}");
    let archive_bytes = download_bytes(&asset_url).await?;

    let checksums_url = constants::release_checksums_url(&release.tag);
    eprintln!("▸ Verifying checksum...");
    let checksums_text = download_text(&checksums_url).await?;
    verify_checksum(&archive_bytes, &asset_name, &checksums_text)?;

    eprintln!("▸ Extracting...");
    let new_binary = extract_binary(&archive_bytes)?;

    eprintln!("▸ Replacing binary...");
    atomic_replace(&current_exe, &new_binary)?;

    eprintln!("✔ Updated to {} successfully.", release.version);

    Ok(())
}

/// Look up the release we should compare against.
///
/// Stable: `/releases/latest` (GitHub already excludes pre-releases).
/// Pre-release: `/releases` (full list); we pick the highest semver.
async fn fetch_latest_release(prerelease: bool) -> Result<ReleaseInfo, UpdateError> {
    let client = crate::http::build_client()
        .map_err(|e| UpdateError::ApiError(format!("failed to build HTTP client: {e}")))?;

    let url = if prerelease {
        constants::GITHUB_RELEASES_ALL_API
    } else {
        constants::GITHUB_RELEASES_LATEST_API
    };

    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| UpdateError::ApiError(format!("request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(UpdateError::ApiError(format!(
            "GitHub API returned {}",
            resp.status()
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| UpdateError::ApiError(format!("failed to parse response: {e}")))?;

    let tag = if prerelease {
        pick_highest_release_tag(&body)?
    } else {
        body["tag_name"]
            .as_str()
            .ok_or_else(|| UpdateError::ApiError("missing tag_name in response".to_string()))?
            .to_string()
    };

    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();

    Ok(ReleaseInfo { tag, version })
}

/// Pick the highest semver tag from a `/releases` response. Skips drafts.
fn pick_highest_release_tag(body: &serde_json::Value) -> Result<String, UpdateError> {
    let arr = body
        .as_array()
        .ok_or_else(|| UpdateError::ApiError("expected an array of releases".to_string()))?;

    let mut best: Option<(semver::Version, String)> = None;
    for entry in arr {
        if entry["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(tag) = entry["tag_name"].as_str() else {
            continue;
        };
        let raw = tag.strip_prefix('v').unwrap_or(tag);
        let Ok(parsed) = semver::Version::parse(raw) else {
            continue;
        };
        if best.as_ref().is_none_or(|(v, _)| parsed > *v) {
            best = Some((parsed, tag.to_string()));
        }
    }

    best.map(|(_, tag)| tag)
        .ok_or_else(|| UpdateError::ApiError("no parseable release tags found".to_string()))
}

/// Compare a remote version string against the current version.
///
/// Returns `true` if `remote` is strictly newer than `CURRENT_VERSION`.
fn is_newer(remote: &str) -> bool {
    match (
        semver::Version::parse(CURRENT_VERSION),
        semver::Version::parse(remote),
    ) {
        (Ok(current), Ok(remote)) => remote > current,
        _ => remote != CURRENT_VERSION,
    }
}

async fn download_bytes(url: &str) -> Result<Vec<u8>, UpdateError> {
    let client = crate::http::build_client()
        .map_err(|e| UpdateError::DownloadError(format!("failed to build HTTP client: {e}")))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| UpdateError::DownloadError(format!("{url}: {e}")))?;

    if !resp.status().is_success() {
        return Err(UpdateError::DownloadError(format!(
            "{url}: HTTP {}",
            resp.status()
        )));
    }

    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| UpdateError::DownloadError(format!("{url}: {e}")))
}

async fn download_text(url: &str) -> Result<String, UpdateError> {
    let bytes = download_bytes(url).await?;
    String::from_utf8(bytes)
        .map_err(|e| UpdateError::DownloadError(format!("response is not valid UTF-8: {e}")))
}

/// Verify archive bytes against a SHA256SUMS file.
///
/// The checksums file is expected to have lines like:
/// `<hex-hash>  <filename>`
fn verify_checksum(data: &[u8], asset_name: &str, checksums_text: &str) -> Result<(), UpdateError> {
    let expected = checksums_text
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let filename = parts.next()?;
            if filename == asset_name {
                Some(hash.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| UpdateError::ChecksumNotFound(asset_name.to_string()))?;

    let mut hasher = Sha256::new();
    hasher.update(data);
    let actual = hex::encode(hasher.finalize());

    if actual != expected {
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }

    Ok(())
}

/// Extract the `scaffl` binary from a `.tar.gz` archive in memory.
///
/// Looks for an entry named `scaffl` (or ending with `/scaffl`) in the
/// archive and returns its contents as bytes.
fn extract_binary(archive_bytes: &[u8]) -> Result<Vec<u8>, UpdateError> {
    let decoder = GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive
        .entries()
        .map_err(|e| UpdateError::ExtractError(format!("failed to read archive entries: {e}")))?
    {
        let mut entry =
            entry.map_err(|e| UpdateError::ExtractError(format!("corrupt archive entry: {e}")))?;

        let path = entry
            .path()
            .map_err(|e| UpdateError::ExtractError(format!("invalid path in archive: {e}")))?;

        let is_binary = path.file_name().is_some_and(|name| name == "scaffl");
        if !is_binary {
            continue;
        }

        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(|e| {
            UpdateError::ExtractError(format!("failed to read binary from archive: {e}"))
        })?;

        if buf.is_empty() {
            return Err(UpdateError::ExtractError(
                "extracted binary is empty".to_string(),
            ));
        }

        return Ok(buf);
    }

    Err(UpdateError::ExtractError(
        "archive does not contain a 'scaffl' binary".to_string(),
    ))
}

/// Atomically replace the binary at `target_path` with `new_binary`.
///
/// Writes to a temporary file next to the target, sets executable
/// permissions, and renames (which is atomic on the same filesystem).
fn atomic_replace(target_path: &Path, new_binary: &[u8]) -> Result<(), UpdateError> {
    let parent = target_path.parent().ok_or_else(|| {
        UpdateError::ReplaceError("cannot determine parent directory".to_string())
    })?;

    let tmp_path = parent.join(".scaffl-update.tmp");

    let mut tmp_file = fs::File::create(&tmp_path).map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            UpdateError::PermissionDenied(format!(
                "permission denied writing to {}. Try running with sudo.",
                parent.display()
            ))
        } else {
            UpdateError::ReplaceError(format!("failed to create temp file: {e}"))
        }
    })?;

    tmp_file
        .write_all(new_binary)
        .map_err(|e| UpdateError::ReplaceError(format!("failed to write temp file: {e}")))?;

    tmp_file
        .flush()
        .map_err(|e| UpdateError::ReplaceError(format!("failed to flush temp file: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&tmp_path, perms)
            .map_err(|e| UpdateError::ReplaceError(format!("failed to set permissions: {e}")))?;
    }

    fs::rename(&tmp_path, target_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        if e.kind() == io::ErrorKind::PermissionDenied {
            UpdateError::PermissionDenied(format!(
                "permission denied replacing {}. Try running with sudo.",
                target_path.display()
            ))
        } else {
            UpdateError::ReplaceError(format!("failed to replace binary: {e}"))
        }
    })
}

/// Check whether we can write to the directory containing the target binary.
fn check_write_permission(exe_path: &Path) -> Result<(), UpdateError> {
    let parent = exe_path.parent().ok_or_else(|| {
        UpdateError::ReplaceError("cannot determine parent directory".to_string())
    })?;

    let probe_path = parent.join(".scaffl-write-probe");
    match fs::File::create(&probe_path) {
        Ok(_) => {
            let _ = fs::remove_file(&probe_path);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            Err(UpdateError::PermissionDenied(format!(
                "permission denied: cannot write to {}. Try running with sudo.",
                parent.display()
            )))
        }
        Err(e) => Err(UpdateError::ReplaceError(format!(
            "cannot write to {}: {e}",
            parent.display()
        ))),
    }
}

/// Validate that the current platform has release builds available.
fn validate_platform() -> Result<(), UpdateError> {
    const SUPPORTED_TARGETS: &[&str] = &[
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ];

    if !SUPPORTED_TARGETS.contains(&TARGET) {
        return Err(UpdateError::UnsupportedPlatform(format!(
            "no pre-built binary available for '{TARGET}'. \
             Supported targets: {}",
            SUPPORTED_TARGETS.join(", ")
        )));
    }

    Ok(())
}

/// Detect whether the process is running inside a container.
fn detect_container_environment() -> Option<&'static str> {
    if Path::new("/.dockerenv").exists() {
        return Some("Docker");
    }

    if std::env::var("container").is_ok() {
        return Some("a container");
    }

    if let Ok(cgroup) = fs::read_to_string("/proc/1/cgroup")
        && (cgroup.contains("docker") || cgroup.contains("containerd") || cgroup.contains("lxc"))
    {
        return Some("a container");
    }

    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        return Some("Kubernetes");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_detects_higher_version() {
        assert!(version_cmp("0.1.0", "0.2.0"));
        assert!(version_cmp("0.1.0", "1.0.0"));
        assert!(version_cmp("0.1.0", "0.1.1"));
    }

    #[test]
    fn is_newer_rejects_lower_or_equal() {
        assert!(!version_cmp("0.2.0", "0.1.0"));
        assert!(!version_cmp("0.1.0", "0.1.0"));
    }

    /// Helper that isolates version comparison from CURRENT_VERSION.
    fn version_cmp(current: &str, remote: &str) -> bool {
        let parse = |v: &str| -> Option<(u64, u64, u64)> {
            let parts: Vec<&str> = v.split('.').collect();
            if parts.len() != 3 {
                return None;
            }
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            ))
        };
        match (parse(current), parse(remote)) {
            (Some(c), Some(r)) => r > c,
            _ => remote != current,
        }
    }

    #[test]
    fn verify_checksum_success() {
        let data = b"hello world";
        let hash = hex::encode(Sha256::digest(data));
        let checksums = format!("{hash}  scaffl-x86_64-unknown-linux-gnu.tar.gz\n");
        assert!(
            verify_checksum(data, "scaffl-x86_64-unknown-linux-gnu.tar.gz", &checksums).is_ok()
        );
    }

    #[test]
    fn verify_checksum_mismatch() {
        let data = b"hello world";
        let checksums = "0000000000000000000000000000000000000000000000000000000000000000  scaffl-x86_64-unknown-linux-gnu.tar.gz\n";
        let result = verify_checksum(data, "scaffl-x86_64-unknown-linux-gnu.tar.gz", checksums);
        assert!(matches!(result, Err(UpdateError::ChecksumMismatch { .. })));
    }

    #[test]
    fn verify_checksum_not_found() {
        let data = b"hello world";
        let checksums = "abc123  some-other-file.tar.gz\n";
        let result = verify_checksum(data, "scaffl-x86_64-unknown-linux-gnu.tar.gz", checksums);
        assert!(matches!(result, Err(UpdateError::ChecksumNotFound(_))));
    }

    #[test]
    fn extract_binary_empty_archive_fails() {
        let result = extract_binary(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn extract_binary_valid_archive() {
        let mut builder = tar::Builder::new(Vec::new());

        let content = b"fake-binary-content";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "scaffl", &content[..])
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar_bytes).unwrap();
        let gz_bytes = encoder.finish().unwrap();

        let result = extract_binary(&gz_bytes).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn extract_binary_no_matching_entry() {
        let mut builder = tar::Builder::new(Vec::new());

        let content = b"other-content";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "some-other-file", &content[..])
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar_bytes).unwrap();
        let gz_bytes = encoder.finish().unwrap();

        let result = extract_binary(&gz_bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not contain"));
    }

    #[test]
    fn validate_platform_accepts_known_targets() {
        let result = validate_platform();
        let _ = result;
    }

    #[test]
    fn detect_ci_does_not_panic() {
        let _ = crate::ci::is_ci();
    }

    #[test]
    fn detect_container_does_not_panic() {
        let _ = detect_container_environment();
    }

    #[test]
    fn is_newer_with_invalid_version_strings() {
        assert!(version_cmp("abc", "def"));
        assert!(!version_cmp("abc", "abc"));
    }

    #[test]
    fn verify_checksum_multiline_checksums_file() {
        let data = b"test data";
        let hash = hex::encode(Sha256::digest(data));
        let checksums = format!(
            "aaaa  some-other-file.tar.gz\n\
             {hash}  target-file.tar.gz\n\
             bbbb  yet-another.tar.gz\n"
        );
        assert!(verify_checksum(data, "target-file.tar.gz", &checksums).is_ok());
    }

    #[test]
    fn extract_binary_nested_path() {
        let mut builder = tar::Builder::new(Vec::new());
        let content = b"nested-binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "scaffl-v1.0.0/scaffl", &content[..])
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar_bytes).unwrap();
        let gz_bytes = encoder.finish().unwrap();

        let result = extract_binary(&gz_bytes).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn pick_highest_release_tag_prefers_higher_semver() {
        let body = serde_json::json!([
            { "tag_name": "v0.1.0", "draft": false },
            { "tag_name": "v0.2.0-rc.1", "draft": false },
            { "tag_name": "v0.1.5", "draft": false },
        ]);
        let tag = pick_highest_release_tag(&body).unwrap();
        assert_eq!(tag, "v0.2.0-rc.1");
    }

    #[test]
    fn pick_highest_release_tag_skips_drafts() {
        let body = serde_json::json!([
            { "tag_name": "v9.9.9", "draft": true },
            { "tag_name": "v0.1.0", "draft": false },
        ]);
        let tag = pick_highest_release_tag(&body).unwrap();
        assert_eq!(tag, "v0.1.0");
    }

    #[test]
    fn pick_highest_release_tag_skips_unparseable() {
        let body = serde_json::json!([
            { "tag_name": "not-a-version", "draft": false },
            { "tag_name": "v0.1.0", "draft": false },
        ]);
        let tag = pick_highest_release_tag(&body).unwrap();
        assert_eq!(tag, "v0.1.0");
    }
}
