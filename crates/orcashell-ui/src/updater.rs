use std::cmp::Ordering;
use std::time::Duration;

use serde::Deserialize;

const DEFAULT_UPDATE_METADATA_URL: &str = "https://orcashell.com/updates/latest.json";
const HTTP_TIMEOUT: Duration = Duration::from_secs(4);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub current_version: String,
    pub latest_version: String,
    pub download_url: String,
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
    match fetch_release_metadata() {
        Ok(metadata) => {
            let current_version = current_app_version();
            if compare_versions(&metadata.version, &current_version).is_gt() {
                match platform_download_url(&metadata.downloads) {
                    Some(download_url) => UpdateCheckResult::UpdateAvailable(AvailableUpdate {
                        current_version,
                        latest_version: metadata.version,
                        download_url,
                        release_notes_url: metadata.release_notes_url,
                    }),
                    None => UpdateCheckResult::Failed {
                        message: "Update metadata is missing a download for this platform."
                            .to_string(),
                    },
                }
            } else {
                UpdateCheckResult::UpToDate { current_version }
            }
        }
        Err(message) => UpdateCheckResult::Failed { message },
    }
}

fn fetch_release_metadata() -> Result<ReleaseMetadata, String> {
    let metadata_url = std::env::var("ORCASHELL_UPDATE_METADATA_URL")
        .unwrap_or_else(|_| DEFAULT_UPDATE_METADATA_URL.to_string());
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(&metadata_url)
        .call()
        .map_err(|error| format!("update check failed: {error}"))?;
    let body = response
        .body_mut()
        .read_to_vec()
        .map_err(|error| format!("invalid update metadata: {error}"))?;
    serde_json::from_slice::<ReleaseMetadata>(&body)
        .map_err(|error| format!("invalid update metadata: {error}"))
}

fn platform_download_url(downloads: &ReleaseDownloads) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        downloads.macos_arm64.clone()
    }

    #[cfg(target_os = "linux")]
    {
        downloads.linux_x86_64.clone()
    }

    #[cfg(target_os = "windows")]
    {
        downloads.windows_x86_64.clone()
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
