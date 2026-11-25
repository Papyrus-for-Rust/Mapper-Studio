//! Versioned, user-shared diagnostic data. The code is an encoding, not encryption.
use crate::{
    installation::{Installation, InstallationKind},
    native_profile,
};
use serde::{Deserialize, Serialize};

const PREFIX: &str = "MPS1-";
const MAX_JSON_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    mapper_version: String,
    os: String,
    os_version: Option<[u32; 3]>,
    architecture: String,
    rust: Option<RustSource>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustSource {
    kind: InstallationKind,
    native_sha256: String,
    scene_manifest_sha256: Option<String>,
    terrain_profile: Option<String>,
}

impl Snapshot {
    pub fn capture(
        version: &str,
        installation: Option<&Installation>,
        selected_profile: Option<&native_profile::Profile>,
    ) -> Self {
        let rust = installation.map(|source| {
            let selected_profile =
                selected_profile.filter(|profile| profile.matches(source).is_ok());
            let profile = selected_profile.or_else(|| native_profile::legacy_profile(source));
            RustSource {
                kind: source.kind,
                native_sha256: source.native.sha256.clone(),
                scene_manifest_sha256: source
                    .scene_manifest
                    .as_ref()
                    .map(|file| file.sha256.clone()),
                terrain_profile: profile.map(|profile| profile.id.clone()),
            }
        });

        Self {
            mapper_version: version.into(),
            os: std::env::consts::OS.into(),
            #[cfg(windows)]
            os_version: papyrus_file_id::windows_version(),
            #[cfg(not(windows))]
            os_version: None,
            architecture: std::env::consts::ARCH.into(),
            rust,
        }
    }

    pub fn encode(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_JSON_BYTES {
            return Err("Support snapshot is too large.".into());
        }
        Ok(format!("{PREFIX}{}", hex::encode(bytes)))
    }

    pub fn decode(code: &str) -> Result<Self, String> {
        let encoded = code
            .trim()
            .strip_prefix(PREFIX)
            .ok_or("Expected a Mapper support code starting with MPS1-.")?;
        if encoded.len() > MAX_JSON_BYTES * 2 {
            return Err("Support code is too large.".into());
        }
        let bytes = hex::decode(encoded).map_err(|_| "Support code is incomplete or invalid.")?;
        serde_json::from_slice(&bytes).map_err(|_| "Support code contents are invalid.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_snapshot_round_trips_without_installation_paths() {
        let source = Installation {
            kind: InstallationKind::Client,
            native: crate::installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "b12f07380464b9832842f136659ce0bbef31697f796217e4890dc13defe1472d".into(),
                bytes: 1,
            },
            scene_manifest: None,
        };
        let snapshot = Snapshot::capture("0.1.0-beta.1", Some(&source), None);
        let code = snapshot.encode().unwrap();
        let decoded = Snapshot::decode(&format!(" {code}\n")).unwrap();
        assert_eq!(decoded, snapshot);
        let json = serde_json::to_string(&decoded).unwrap();
        assert!(!json.contains("Plugins"));
        assert!(json.contains("client-b12f0738-preview-1"));
        assert!(json.contains(&source.native.sha256));
        #[cfg(windows)]
        assert!(decoded.os_version.is_some());
    }

    #[test]
    fn decoder_rejects_unsupported_versions_and_invalid_codes() {
        assert!(Snapshot::decode("MPS2-0123").is_err());
        assert!(Snapshot::decode("MPS1-xy").is_err());
        assert!(Snapshot::decode("MPS1-7b").is_err());
        assert!(Snapshot::decode(&format!("{PREFIX}{}", "aa".repeat(MAX_JSON_BYTES + 1))).is_err());
    }
}
