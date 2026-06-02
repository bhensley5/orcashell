use super::*;

use ring::signature::{Ed25519KeyPair, KeyPair};

#[test]
fn version_compare_handles_semver_like_numbers() {
    assert!(compare_versions("0.1.3", "0.1.2").is_gt());
    assert!(compare_versions("1.0.0", "0.9.9").is_gt());
    assert!(compare_versions("0.1.2", "0.1.2").is_eq());
    assert!(compare_versions("0.1.2", "0.1.10").is_lt());
}

#[test]
fn parse_verified_manifest_accepts_valid_signed_metadata() {
    let manifest = signed_manifest();
    let parsed = parse_verified_update_manifest(
        manifest.bytes.as_bytes(),
        manifest.signature.as_bytes(),
        &manifest.public_key,
    )
    .expect("valid signed manifest should parse");

    assert_eq!(parsed.manifest_version, 1);
    assert_eq!(parsed.version, "9.9.9");
    assert_eq!(
        parsed.downloads.macos_arm64.as_deref(),
        Some("https://orcashell.com/downloads/OrcaShell-9.9.9-macos-arm64.dmg")
    );
    assert_eq!(
        parsed
            .artifacts
            .linux_x86_64
            .as_ref()
            .map(|artifact| artifact.size_bytes),
        Some(22_222)
    );
}

#[test]
fn parse_verified_manifest_rejects_tampered_metadata() {
    let mut manifest = signed_manifest();
    manifest.bytes = manifest.bytes.replace("9.9.9", "9.9.10");

    let error = parse_verified_update_manifest(
        manifest.bytes.as_bytes(),
        manifest.signature.as_bytes(),
        &manifest.public_key,
    )
    .expect_err("tampered manifest must not verify");

    assert!(error.contains("signature verification failed"));
}

#[test]
fn parse_verified_manifest_rejects_wrong_public_key() {
    let manifest = signed_manifest();
    let wrong_key = public_key_base64(&key_pair_from_seed([9; 32]));

    let error = parse_verified_update_manifest(
        manifest.bytes.as_bytes(),
        manifest.signature.as_bytes(),
        &wrong_key,
    )
    .expect_err("wrong key must not verify");

    assert!(error.contains("signature verification failed"));
}

#[test]
fn check_for_updates_with_fetcher_returns_update_for_verified_newer_manifest() {
    let manifest = signed_manifest();

    let result = check_with_signed_manifest("9.9.8", &manifest);

    let UpdateCheckResult::UpdateAvailable(update) = result else {
        panic!("expected update to be available");
    };
    assert_eq!(update.current_version, "9.9.8");
    assert_eq!(update.latest_version, "9.9.9");
    assert_eq!(update.download_url, expected_platform_download_url());
    assert_eq!(update.download_sha256, "a".repeat(64));
    assert_eq!(update.download_size_bytes, expected_platform_size());
    assert_eq!(
        update.release_notes_url.as_deref(),
        Some("https://github.com/orcashell/orcashell/releases/tag/v9.9.9")
    );
}

#[test]
fn check_for_updates_with_fetcher_returns_up_to_date_for_verified_equal_manifest() {
    let manifest = signed_manifest();

    let result = check_with_signed_manifest("9.9.9", &manifest);

    assert_eq!(
        result,
        UpdateCheckResult::UpToDate {
            current_version: "9.9.9".to_string()
        }
    );
}

#[test]
fn check_for_updates_with_fetcher_fails_closed_when_signature_fetch_fails() {
    let manifest = signed_manifest();

    let result = check_for_updates_with_fetcher(
        "9.9.8",
        "https://orcashell.com/updates/latest.json",
        &manifest.public_key,
        |url, _limit| {
            if url == "https://orcashell.com/updates/latest.json" {
                Ok(manifest.bytes.as_bytes().to_vec())
            } else {
                Err("missing signature".to_string())
            }
        },
    );

    assert_update_verification_failed(result);
}

#[test]
fn check_for_updates_with_fetcher_fails_closed_for_unsafe_metadata_url() {
    let manifest = signed_manifest();

    let result = check_for_updates_with_fetcher(
        "9.9.8",
        "http://orcashell.com/updates/latest.json",
        &manifest.public_key,
        |_url, _limit| panic!("unsafe metadata URL should fail before fetching"),
    );

    assert_update_verification_failed(result);
}

#[test]
fn parse_verified_manifest_rejects_malformed_signature_base64() {
    let manifest = signed_manifest();

    let error = parse_verified_update_manifest(
        manifest.bytes.as_bytes(),
        b"not base64!",
        &manifest.public_key,
    )
    .expect_err("malformed signature must fail");

    assert!(error.contains("invalid update metadata signature"));
}

#[test]
fn parse_verified_manifest_rejects_missing_signature() {
    let manifest = signed_manifest();

    let error =
        parse_verified_update_manifest(manifest.bytes.as_bytes(), b"", &manifest.public_key)
            .expect_err("missing signature must fail");

    assert!(error.contains("invalid update metadata signature"));
}

#[test]
fn parse_verified_manifest_rejects_oversized_signature() {
    let manifest = signed_manifest();
    let oversized_signature = vec![b'a'; MAX_UPDATE_SIGNATURE_BYTES + 1];

    let error = parse_verified_update_manifest(
        manifest.bytes.as_bytes(),
        &oversized_signature,
        &manifest.public_key,
    )
    .expect_err("oversized signature must fail");

    assert!(error.contains("signature exceeds"));
}

#[test]
fn parse_verified_manifest_rejects_oversized_manifest() {
    let manifest = signed_manifest();
    let oversized_manifest = vec![b' '; MAX_UPDATE_MANIFEST_BYTES + 1];

    let error = parse_verified_update_manifest(
        &oversized_manifest,
        manifest.signature.as_bytes(),
        &manifest.public_key,
    )
    .expect_err("oversized manifest must fail");

    assert!(error.contains("metadata exceeds"));
}

#[test]
fn validate_manifest_requires_supported_platform_downloads() {
    let mut manifest = valid_manifest_struct();
    manifest.downloads.windows_x86_64 = None;

    let error = validate_update_manifest(&manifest).expect_err("missing download must fail");

    assert!(error.contains("missing a download for windows_x86_64"));
}

#[test]
fn validate_manifest_requires_supported_platform_artifacts() {
    let mut manifest = valid_manifest_struct();
    manifest.artifacts.macos_arm64 = None;

    let error = validate_update_manifest(&manifest).expect_err("missing artifact must fail");

    assert!(error.contains("missing artifact data for macos_arm64"));
}

#[test]
fn validate_manifest_rejects_non_https_urls() {
    let mut manifest = valid_manifest_struct();
    manifest.downloads.linux_x86_64 = Some("http://orcashell.com/downloads/app.tar.gz".to_string());

    let error = validate_update_manifest(&manifest).expect_err("non-https URL must fail");

    assert!(error.contains("must use https"));
}

#[test]
fn validate_manifest_rejects_disallowed_hosts() {
    let mut manifest = valid_manifest_struct();
    manifest.release_notes_url = "https://evil.example/releases/v9.9.9".to_string();

    let error = validate_update_manifest(&manifest).expect_err("disallowed host must fail");

    assert!(error.contains("host is not allowed"));
}

#[test]
fn validate_manifest_rejects_malformed_sha256() {
    let mut manifest = valid_manifest_struct();
    manifest
        .artifacts
        .windows_x86_64
        .as_mut()
        .expect("fixture has windows artifact")
        .sha256 = "ABC".to_string();

    let error = validate_update_manifest(&manifest).expect_err("bad hash must fail");

    assert!(error.contains("lowercase SHA256"));
}

#[test]
fn validate_manifest_rejects_zero_size_artifacts() {
    let mut manifest = valid_manifest_struct();
    manifest
        .artifacts
        .linux_x86_64
        .as_mut()
        .expect("fixture has linux artifact")
        .size_bytes = 0;

    let error = validate_update_manifest(&manifest).expect_err("zero size must fail");

    assert!(error.contains("greater than zero"));
}

#[test]
fn signed_manifest_json_stays_legacy_release_metadata_compatible() {
    let manifest = signed_manifest();

    let legacy = serde_json::from_str::<ReleaseMetadata>(&manifest.bytes)
        .expect("new manifest must remain parseable by old updater metadata shape");

    assert_eq!(legacy.version, "9.9.9");
    assert_eq!(
        legacy.downloads.windows_x86_64.as_deref(),
        Some("https://orcashell.com/downloads/orcashell-9.9.9-windows-x64.zip")
    );
}

struct SignedManifest {
    bytes: String,
    signature: String,
    public_key: String,
}

fn signed_manifest() -> SignedManifest {
    let signing_key = key_pair_from_seed([7; 32]);
    let bytes = valid_manifest_json();
    let signature = signing_key.sign(bytes.as_bytes());
    SignedManifest {
        bytes,
        signature: base64::engine::general_purpose::STANDARD.encode(signature.as_ref()),
        public_key: public_key_base64(&signing_key),
    }
}

fn key_pair_from_seed(seed: [u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&seed).expect("fixture seed should create key pair")
}

fn public_key_base64(signing_key: &Ed25519KeyPair) -> String {
    base64::engine::general_purpose::STANDARD.encode(signing_key.public_key().as_ref())
}

fn valid_manifest_json() -> String {
    format!(
        r#"{{
  "manifest_version": 1,
  "version": "9.9.9",
  "published_at": "2026-05-30T00:00:00Z",
  "release_notes_url": "https://github.com/orcashell/orcashell/releases/tag/v9.9.9",
  "downloads": {{
    "macos_arm64": "https://orcashell.com/downloads/OrcaShell-9.9.9-macos-arm64.dmg",
    "linux_x86_64": "https://orcashell.com/downloads/orcashell-9.9.9-linux-x86_64.tar.gz",
    "windows_x86_64": "https://orcashell.com/downloads/orcashell-9.9.9-windows-x64.zip"
  }},
  "artifacts": {{
    "macos_arm64": {{
      "sha256": "{hash}",
      "size_bytes": 11111
    }},
    "linux_x86_64": {{
      "sha256": "{hash}",
      "size_bytes": 22222
    }},
    "windows_x86_64": {{
      "sha256": "{hash}",
      "size_bytes": 33333
    }}
  }}
}}"#,
        hash = "a".repeat(64)
    )
}

fn valid_manifest_struct() -> UpdateManifest {
    serde_json::from_str(&valid_manifest_json()).expect("fixture manifest should parse")
}

fn check_with_signed_manifest(
    current_version: &str,
    manifest: &SignedManifest,
) -> UpdateCheckResult {
    check_for_updates_with_fetcher(
        current_version,
        "https://orcashell.com/updates/latest.json",
        &manifest.public_key,
        |url, _limit| match url {
            "https://orcashell.com/updates/latest.json" => Ok(manifest.bytes.as_bytes().to_vec()),
            "https://orcashell.com/updates/latest.json.sig" => {
                Ok(manifest.signature.as_bytes().to_vec())
            }
            unexpected => Err(format!("unexpected URL: {unexpected}")),
        },
    )
}

fn assert_update_verification_failed(result: UpdateCheckResult) {
    assert_eq!(
        result,
        UpdateCheckResult::Failed {
            message: "Could not verify update metadata. Please download OrcaShell from the official release page.".to_string()
        }
    );
}

fn expected_platform_download_url() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "https://orcashell.com/downloads/OrcaShell-9.9.9-macos-arm64.dmg"
    }

    #[cfg(target_os = "linux")]
    {
        "https://orcashell.com/downloads/orcashell-9.9.9-linux-x86_64.tar.gz"
    }

    #[cfg(target_os = "windows")]
    {
        "https://orcashell.com/downloads/orcashell-9.9.9-windows-x64.zip"
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        ""
    }
}

fn expected_platform_size() -> u64 {
    #[cfg(target_os = "macos")]
    {
        11_111
    }

    #[cfg(target_os = "linux")]
    {
        22_222
    }

    #[cfg(target_os = "windows")]
    {
        33_333
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        0
    }
}
