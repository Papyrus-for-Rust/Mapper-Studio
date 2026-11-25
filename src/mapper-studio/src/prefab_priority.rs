//! Code-bound priority defaults for prefab roots without parameter components.
use crate::{
    Project,
    installation::{InstallationKind, SourceFile},
    prefab_assets::Catalog,
};
use serde::Deserialize;

const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/monuments/client-0f703d43-priority.json"),
    include_str!("../../../compatibility/monuments/client-8dd4e235-priority.json"),
    include_str!("../../../compatibility/monuments/client-58f3263f-priority.json"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comparison {
    owner: String,
    address: u64,
    bytes: usize,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    native_sha256: String,
    code_sources: Vec<SourceFile>,
    default_priority: i32,
    enum_type: String,
    enum_field: String,
    comparison: Comparison,
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Profile {
    fn decode(raw: &str) -> Result<Self, String> {
        let p: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        crate::installation::validate_source_receipts(&p.code_sources, &[])?;
        if p.format != "papyrus-prefab-priority-profile"
            || p.schema != 1
            || !hash(&p.native_sha256)
            || p.code_sources.len() != 2
            || p.code_sources[0].relative_path != "GameAssembly.dll"
            || p.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || p.enum_type != "PrefabPriority"
            || p.enum_field != "Default"
            || p.comparison.owner.is_empty()
            || p.comparison.owner.len() > 256
            || p.comparison.address == 0
            || !(1..=65536).contains(&p.comparison.bytes)
            || !hash(&p.comparison.sha256)
        {
            return Err("Invalid prefab priority compatibility profile".into());
        }
        Ok(p)
    }
}

fn selected(native: &str) -> Result<Option<Profile>, String> {
    for raw in PROFILES {
        let profile = Profile::decode(raw)?;
        if profile.native_sha256 == native {
            return Ok(Some(profile));
        }
    }
    Ok(None)
}

/// The monument reader verifies these code receipts before returning its source.
pub(crate) fn supports(catalog: &Catalog, code_sources: &[SourceFile]) -> Result<bool, String> {
    if catalog.installation.kind != InstallationKind::Client {
        return Ok(false);
    }
    let Some(profile) = selected(&catalog.installation.native.sha256)? else {
        return Ok(false);
    };
    Ok(code_sources.len() == profile.code_sources.len()
        && profile
            .code_sources
            .iter()
            .all(|source| code_sources.contains(source)))
}

pub(crate) fn fallback(project: &Project) -> Result<Option<i32>, String> {
    if project
        .monument_rules
        .as_ref()
        .and_then(|rules| rules.surface_modifiers.as_deref())
        != Some(crate::monument_source::SOURCE_PRIORITY_VERSION)
    {
        return Ok(None);
    }
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing prefab priority installation")?;
    if installation.kind != InstallationKind::Client {
        return Err("Prefab priority defaults require a compatible selected client".into());
    }
    selected(&installation.native.sha256)?
        .map(|profile| Some(profile.default_priority))
        .ok_or_else(|| "No priority defaults match the selected Rust installation".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn priority_defaults_require_matching_native_and_both_code_receipts() {
        for raw in PROFILES {
            let p = Profile::decode(raw).unwrap();
            assert_eq!(p.default_priority, 2);
            let mut catalog = Catalog {
                sha256: "a".repeat(64),
                installation: crate::installation::Installation {
                    kind: InstallationKind::Client,
                    native: SourceFile {
                        relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                        sha256: p.native_sha256,
                        bytes: 1,
                    },
                    scene_manifest: None,
                },
                sources: vec![],
                entries: Default::default(),
            };
            assert!(supports(&catalog, &p.code_sources).unwrap());
            let mut reversed = p.code_sources.clone();
            reversed.reverse();
            assert!(supports(&catalog, &reversed).unwrap());
            reversed[0].sha256 = "0".repeat(64);
            assert!(!supports(&catalog, &reversed).unwrap());
            assert!(!supports(&catalog, &p.code_sources[..1]).unwrap());
            catalog.installation.native.sha256 = "0".repeat(64);
            assert!(!supports(&catalog, &p.code_sources).unwrap());
        }
    }
}
