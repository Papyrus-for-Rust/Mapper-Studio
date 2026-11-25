//! Verified Monument water-hook behavior for selected client code revisions.
use crate::{
    installation::{self, SourceFile},
    prefab_textures::Bindings,
};
use serde::Deserialize;

const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/monuments/client-0f703d43-water.json"),
    include_str!("../../../compatibility/monuments/client-8dd4e235-water.json"),
    include_str!("../../../compatibility/monuments/client-58f3263f-water.json"),
];
const NATIVE: &str = "RustClient_Data/Plugins/x86_64/RustNative.dll";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
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
    behavior: String,
    flag_offset: u32,
    texture_offset: u32,
    slot: u32,
    vtable_base: u32,
    slot_offset: u32,
    dispatcher: Window,
    implementation: Window,
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
        installation::validate_source_receipts(&p.code_sources, &[])?;
        if p.format != "papyrus-monument-water-hook-profile"
            || p.schema != 1
            || p.behavior != "no_op"
            || !hash(&p.native_sha256)
            || p.code_sources.len() != 2
            || p.code_sources[0].relative_path != "GameAssembly.dll"
            || p.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || p.flag_offset > 1_048_576
            || p.texture_offset > 1_048_576
            || p.flag_offset == p.texture_offset
            || !(1..=4096).contains(&p.slot)
            || p.vtable_base > 1_048_576
            || p.slot_offset != p.vtable_base + p.slot * 16
            || [&p.dispatcher, &p.implementation]
                .iter()
                .any(|w| w.address == 0 || !(1..=65536).contains(&w.bytes) || !hash(&w.sha256))
        {
            return Err("Invalid monument water-hook profile".into());
        }
        Ok(p)
    }
}

fn selected(native: &SourceFile) -> Result<Option<Profile>, String> {
    if native.relative_path != NATIVE {
        return Ok(None);
    }
    for raw in PROFILES {
        let profile = Profile::decode(raw)?;
        if profile.native_sha256 == native.sha256 {
            return Ok(Some(profile));
        }
    }
    Ok(None)
}

/// Resolve the source hook through existing read leases. Enabled water flags
/// still run the remaining monument terrain channels for these code revisions.
pub(crate) fn verified_noop(
    bindings: &Bindings,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    let Some(native) = bindings
        .source_receipts()
        .iter()
        .find(|r| r.relative_path == NATIVE)
    else {
        return Ok(false);
    };
    let Some(profile) = selected(native)? else {
        return Ok(false);
    };
    let _verification = installation::VerificationScope::default();
    for receipt in std::iter::once(native).chain(&profile.code_sources) {
        if cancelled() {
            return Ok(false);
        }
        let actual = installation::hash_file(
            bindings.installation_root(),
            &receipt.relative_path,
            1024 * 1024 * 1024,
            cancelled,
        );
        if cancelled() {
            return Ok(false);
        }
        if actual? != *receipt {
            return Err(
                "Monument water behavior differs from the selected Rust installation".into(),
            );
        }
    }
    Ok(!cancelled())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_hook_profiles_bind_dispatch_slot_and_return_only_implementation() {
        for raw in PROFILES {
            let p = Profile::decode(raw).unwrap();
            assert_eq!(p.implementation.bytes, 16);
            assert_eq!(
                p.implementation.sha256,
                "28f844b5972c241e804231b0e41b93e13696e59b98dde74f19029386db5da0c5"
            );
            let source = SourceFile {
                relative_path: NATIVE.into(),
                sha256: p.native_sha256,
                bytes: 1,
            };
            assert!(selected(&source).unwrap().is_some());
            let unknown = SourceFile {
                sha256: "0".repeat(64),
                ..source
            };
            assert!(selected(&unknown).unwrap().is_none());
        }
    }
    #[test]
    fn unsupported_behavior_and_changed_dispatch_layout_are_rejected() {
        let original: serde_json::Value = serde_json::from_str(PROFILES[0]).unwrap();
        for (path, value) in [
            ("/behavior", serde_json::json!("paint")),
            ("/slot_offset", serde_json::json!(1)),
            ("/slot", serde_json::json!(0)),
            ("/implementation/sha256", serde_json::json!("invalid")),
            (
                "/code_sources/0/relative_path",
                serde_json::json!("Other.dll"),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(Profile::decode(&changed.to_string()).is_err(), "{path}");
        }
    }
}
