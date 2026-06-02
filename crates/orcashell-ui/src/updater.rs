use std::cmp::Ordering;
use std::time::Duration;

use base64::Engine as _;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Deserialize;
use url::Url;

const DEFAULT_UPDATE_METADATA_URL: &str = "https://orcashell.com/updates/latest.json";
const HTTP_TIMEOUT: Duration = Duration::from_secs(4);
const ORCASHELL_UPDATE_MANIFEST_PUBLIC_KEY: Option<&str> =
    option_env!("ORCASHELL_UPDATE_MANIFEST_PUBLIC_KEY");
pub(crate) const UPDATE_MANIFEST_VERSION: u64 = 1;
pub(crate) const MAX_UPDATE_MANIFEST_BYTES: usize = 256 * 1024;
pub(crate) const MAX_UPDATE_SIGNATURE_BYTES: usize = 4 * 1024;
pub(crate) const ALLOWED_UPDATE_HOSTS: &[&str] = &["orcashell.com", "github.com"];

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseMetadata {
    pub version: String,
    pub release_notes_url: Option<String>,
    pub downloads: ReleaseDownloads,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseDownloads {
    pub macos_arm64: Option<String>,
    pub linux_x86_64: Option<String>,
    pub windows_x86_64: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UpdateManifest {
    pub(crate) manifest_version: u64,
    pub(crate) version: String,
    pub(crate) published_at: String,
    pub(crate) release_notes_url: String,
    pub(crate) downloads: ReleaseDownloads,
    pub(crate) artifacts: ReleaseArtifacts,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReleaseArtifacts {
    pub(crate) macos_arm64: Option<ReleaseArtifact>,
    pub(crate) linux_x86_64: Option<ReleaseArtifact>,
    pub(crate) windows_x86_64: Option<ReleaseArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReleaseArtifact {
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub current_version: String,
    pub latest_version: String,
    pub download_url: String,
    pub download_sha256: String,
    pub download_size_bytes: u64,
    pub release_notes_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckResult {
    UpdateAvailable(AvailableUpdate),
    UpToDate { current_version: String },
    Failed { message: String },
}

pub fn current_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub fn check_for_updates() -> UpdateCheckResult {
    let metadata_url = std::env::var("ORCASHELL_UPDATE_METADATA_URL")
        .unwrap_or_else(|_| DEFAULT_UPDATE_METADATA_URL.to_string());
    let public_key = match ORCASHELL_UPDATE_MANIFEST_PUBLIC_KEY {
        Some(public_key) if !public_key.trim().is_empty() => public_key,
        _ => {
            return UpdateCheckResult::Failed {
                message: "Could not verify update metadata. Please download OrcaShell from the official release page.".to_string(),
            };
        }
    };

    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .https_only(true)
        .build();
    let agent: ureq::Agent = config.into();
    check_for_updates_with_fetcher(
        &current_app_version(),
        &metadata_url,
        public_key,
        |url, limit| fetch_url_bytes(&agent, url, limit),
    )
}

fn check_for_updates_with_fetcher<F>(
    current_version: &str,
    metadata_url: &str,
    public_key_base64: &str,
    fetch: F,
) -> UpdateCheckResult
where
    F: FnMut(&str, usize) -> Result<Vec<u8>, String>,
{
    match fetch_verified_update_manifest_with_fetcher(metadata_url, public_key_base64, fetch) {
        Ok(manifest) => update_result_from_manifest(current_version, manifest),
        Err(_) => UpdateCheckResult::Failed {
            message: "Could not verify update metadata. Please download OrcaShell from the official release page.".to_string(),
        },
    }
}

fn fetch_verified_update_manifest_with_fetcher<F>(
    metadata_url: &str,
    public_key_base64: &str,
    mut fetch: F,
) -> Result<UpdateManifest, String>
where
    F: FnMut(&str, usize) -> Result<Vec<u8>, String>,
{
    let signature_url = update_signature_url(metadata_url)?;
    let manifest_bytes = fetch(metadata_url, MAX_UPDATE_MANIFEST_BYTES)?;
    let signature_bytes = fetch(&signature_url, MAX_UPDATE_SIGNATURE_BYTES)?;
    parse_verified_update_manifest(&manifest_bytes, &signature_bytes, public_key_base64)
}

pub(crate) fn parse_verified_update_manifest(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    public_key_base64: &str,
) -> Result<UpdateManifest, String> {
    enforce_payload_limit(
        "update metadata",
        manifest_bytes.len(),
        MAX_UPDATE_MANIFEST_BYTES,
    )?;
    enforce_payload_limit(
        "update metadata signature",
        signature_bytes.len(),
        MAX_UPDATE_SIGNATURE_BYTES,
    )?;
    verify_update_manifest_signature(manifest_bytes, signature_bytes, public_key_base64)?;
    let manifest = serde_json::from_slice::<UpdateManifest>(manifest_bytes)
        .map_err(|error| format!("invalid update metadata: {error}"))?;
    validate_update_manifest(&manifest)?;
    Ok(manifest)
}

fn verify_update_manifest_signature(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    public_key_base64: &str,
) -> Result<(), String> {
    let public_key_bytes = decode_fixed_base64::<32>(
        public_key_base64.trim().as_bytes(),
        "invalid update metadata public key",
    )?;
    let signature_bytes = decode_fixed_base64::<64>(
        trim_ascii(signature_bytes),
        "invalid update metadata signature",
    )?;
    UnparsedPublicKey::new(&ED25519, public_key_bytes)
        .verify(manifest_bytes, &signature_bytes)
        .map_err(|_| "update metadata signature verification failed".to_string())
}

fn validate_update_manifest(manifest: &UpdateManifest) -> Result<(), String> {
    if manifest.manifest_version != UPDATE_MANIFEST_VERSION {
        return Err("unsupported update metadata manifest version".to_string());
    }
    if manifest.version.trim().is_empty() {
        return Err("update metadata is missing a version".to_string());
    }
    if manifest.published_at.trim().is_empty() {
        return Err("update metadata is missing a publication timestamp".to_string());
    }
    validate_update_url(&manifest.release_notes_url, "release notes URL")?;

    for (platform, download_url, artifact) in [
        (
            "macos_arm64",
            manifest.downloads.macos_arm64.as_deref(),
            manifest.artifacts.macos_arm64.as_ref(),
        ),
        (
            "linux_x86_64",
            manifest.downloads.linux_x86_64.as_deref(),
            manifest.artifacts.linux_x86_64.as_ref(),
        ),
        (
            "windows_x86_64",
            manifest.downloads.windows_x86_64.as_deref(),
            manifest.artifacts.windows_x86_64.as_ref(),
        ),
    ] {
        let download_url = download_url
            .ok_or_else(|| format!("update metadata is missing a download for {platform}"))?;
        validate_update_url(download_url, "download URL")?;
        let artifact = artifact
            .ok_or_else(|| format!("update metadata is missing artifact data for {platform}"))?;
        validate_artifact_metadata(artifact)?;
    }

    Ok(())
}

fn validate_artifact_metadata(artifact: &ReleaseArtifact) -> Result<(), String> {
    if artifact.size_bytes == 0 {
        return Err("update metadata artifact size must be greater than zero".to_string());
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("update metadata artifact hash must be lowercase SHA256 hex".to_string());
    }
    Ok(())
}

fn validate_update_url(value: &str, label: &str) -> Result<(), String> {
    let parsed = Url::parse(value).map_err(|_| format!("invalid update metadata {label}"))?;
    if parsed.scheme() != "https" {
        return Err(format!("update metadata {label} must use https"));
    }
    match parsed.host_str() {
        Some(host) if ALLOWED_UPDATE_HOSTS.contains(&host) => Ok(()),
        _ => Err(format!("update metadata {label} host is not allowed")),
    }
}

fn update_signature_url(metadata_url: &str) -> Result<String, String> {
    validate_update_url(metadata_url, "metadata URL")?;
    Ok(format!("{metadata_url}.sig"))
}

fn decode_fixed_base64<const N: usize>(bytes: &[u8], message: &str) -> Result<[u8; N], String> {
    if bytes.is_empty() {
        return Err(message.to_string());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(bytes)
        .map_err(|_| message.to_string())?;
    decoded.try_into().map_err(|_| message.to_string())
}

fn enforce_payload_limit(label: &str, len: usize, max: usize) -> Result<(), String> {
    if len > max {
        Err(format!("{label} exceeds the maximum allowed size"))
    } else {
        Ok(())
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn fetch_url_bytes(agent: &ureq::Agent, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| format!("update check failed: {error}"))?;
    response
        .body_mut()
        .with_config()
        .limit(limit as u64)
        .read_to_vec()
        .map_err(|error| format!("invalid update metadata: {error}"))
}

fn update_result_from_manifest(
    current_version: &str,
    manifest: UpdateManifest,
) -> UpdateCheckResult {
    let current_version = current_version.to_string();
    if !compare_versions(&manifest.version, &current_version).is_gt() {
        return UpdateCheckResult::UpToDate { current_version };
    }

    match platform_update_artifact(&manifest) {
        Some(update) => UpdateCheckResult::UpdateAvailable(AvailableUpdate {
            current_version,
            latest_version: manifest.version,
            download_url: update.download_url,
            download_sha256: update.sha256,
            download_size_bytes: update.size_bytes,
            release_notes_url: Some(manifest.release_notes_url),
        }),
        None => UpdateCheckResult::Failed {
            message: "Update metadata is missing a download for this platform.".to_string(),
        },
    }
}

struct PlatformUpdateArtifact {
    download_url: String,
    sha256: String,
    size_bytes: u64,
}

fn platform_update_artifact(manifest: &UpdateManifest) -> Option<PlatformUpdateArtifact> {
    let (download_url, artifact) = platform_download_and_artifact(manifest)?;
    Some(PlatformUpdateArtifact {
        download_url: download_url.to_string(),
        sha256: artifact.sha256.clone(),
        size_bytes: artifact.size_bytes,
    })
}

fn platform_download_and_artifact(manifest: &UpdateManifest) -> Option<(&str, &ReleaseArtifact)> {
    #[cfg(target_os = "macos")]
    {
        Some((
            manifest.downloads.macos_arm64.as_deref()?,
            manifest.artifacts.macos_arm64.as_ref()?,
        ))
    }

    #[cfg(target_os = "linux")]
    {
        Some((
            manifest.downloads.linux_x86_64.as_deref()?,
            manifest.artifacts.linux_x86_64.as_ref()?,
        ))
    }

    #[cfg(target_os = "windows")]
    {
        Some((
            manifest.downloads.windows_x86_64.as_deref()?,
            manifest.artifacts.windows_x86_64.as_ref()?,
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left = normalize_version(left);
    let right = normalize_version(right);
    let width = left.len().max(right.len());
    for index in 0..width {
        let l = *left.get(index).unwrap_or(&0);
        let r = *right.get(index).unwrap_or(&0);
        match l.cmp(&r) {
            Ordering::Equal => {}
            non_eq => return non_eq,
        }
    }
    Ordering::Equal
}

fn normalize_version(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|segment| {
            segment
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests;
