//! Source-profiled environment activation model. Scene/stage coverage and PhysX
//! cooking remain separate requirements. A serialized record is not an instance.
use crate::{
    environment_checks::Registration,
    installation::{Installation, InstallationKind, SourceFile},
    monument_attributes::EnvironmentRecord,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Lifecycle {
    SerializedOnly,
    /// Successful fresh spawn/restoration, original child states, and completed
    /// enable callbacks. Never set merely because placement accepted a pose.
    ActivatedAuthored,
    RootInactive,
    Removed,
    /// Only activation is unknown; authored geometry/pose remain valid.
    ActivationUnknown,
    /// Runtime changes may also have altered layers, hierarchy or geometry.
    Unmodeled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Reason {
    NotInstantiated,
    InactiveRoot,
    Removed,
    ActivationUnknown,
    UnmodeledRuntime,
    MissingObjectFacts,
    OutsideQueryLayers,
    InactiveChild,
    NotFirstVolume,
    SharedTrigger,
    DisabledBehaviour,
    EnabledByCallback,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub registration: Registration,
    pub reason: Reason,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<InstallationKind>,
    native: SourceFile,
    code_sources: Vec<SourceFile>,
    query_layer_mask: u32,
    program: String,
}
/// An explicitly named reference model, not verification of the selected build.
pub struct Model(Profile);
/// Native/code receipts were verified against the same selected installation.
/// Captured prefab data must also be bound to this installation by its loader.
pub struct Verified {
    model: Model,
    root: PathBuf,
}
impl Model {
    fn parse(raw: &str) -> Result<Self, String> {
        let profile: Profile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        let model = Self(profile);
        model.validate()?;
        Ok(model)
    }
    pub fn reference() -> Result<Self, String> {
        Self::parse(include_str!(
            "../../../compatibility/monuments/server-bd19c4ac-environment.json"
        ))
    }
    /// Selected-client activation/query-mask model. Does not certify physics or
    /// complete provider coverage; verify receipts before using it on instances.
    pub fn client_reference() -> Result<Self, String> {
        Self::parse(include_str!(
            "../../../compatibility/monuments/client-e7156192-environment.json"
        ))
    }
    pub fn current_client_reference() -> Result<Self, String> {
        Self::parse(include_str!(
            "../../../compatibility/monuments/client-199a470f-environment.json"
        ))
    }
    fn client_reference_for(native: &SourceFile) -> Result<Option<Self>, String> {
        for raw in [
            include_str!("../../../compatibility/monuments/client-e7156192-environment.json"),
            include_str!("../../../compatibility/monuments/client-199a470f-environment.json"),
            include_str!("../../../compatibility/monuments/client-516653f6-environment.json"),
        ] {
            let model = Self::parse(raw)?;
            if model.0.native == *native {
                return Ok(Some(model));
            }
        }
        Ok(None)
    }
    fn kind(&self) -> InstallationKind {
        self.0.kind.unwrap_or(InstallationKind::DedicatedServer)
    }
    fn validate(&self) -> Result<(), String> {
        let p = &self.0;
        let (native_path, code_paths) = match self.kind() {
            InstallationKind::DedicatedServer => (
                "RustDedicated_Data/Plugins/x86_64/RustNative.dll",
                [
                    "RustDedicated_Data/Managed/Assembly-CSharp.dll",
                    "RustDedicated_Data/Managed/UnityEngine.CoreModule.dll",
                ],
            ),
            InstallationKind::Client => (
                "RustClient_Data/Plugins/x86_64/RustNative.dll",
                [
                    "GameAssembly.dll",
                    "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
                ],
            ),
        };
        let hash = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        };
        if p.format != "papyrus-environment-runtime-profile"
            || p.schema != 1
            || p.id.is_empty()
            || p.id.len() > 128
            || p.program != "environment-onenable-1"
            || p.query_layer_mask == 0
            || p.native.relative_path != native_path
            || !hash(&p.native.sha256)
            || !(1..=64 * 1024 * 1024).contains(&p.native.bytes)
            || p.code_sources.len() != 2
        {
            return Err("Unsupported environment runtime profile".into());
        }
        let paths = p
            .code_sources
            .iter()
            .map(|s| s.relative_path.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if paths != code_paths.into_iter().collect()
            || p.code_sources
                .iter()
                .any(|s| !hash(&s.sha256) || !(1..=1024 * 1024 * 1024).contains(&s.bytes))
        {
            return Err("Invalid environment code receipts".into());
        }
        Ok(())
    }
    pub fn id(&self) -> &str {
        &self.0.id
    }
    pub fn native(&self) -> &SourceFile {
        &self.0.native
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&self.0).map_err(|e| e.to_string())?)
        ))
    }
    /// Diagnostic model only. Normal generation must use a Verified instance.
    pub fn modeled_registration(
        &self,
        record: &EnvironmentRecord,
        lifecycle: Lifecycle,
    ) -> Decision {
        use Lifecycle as L;
        use Reason as R;
        use Registration as S;
        let (registration, reason) = match lifecycle {
            L::SerializedOnly => (S::Disabled, R::NotInstantiated),
            L::RootInactive => (S::Disabled, R::InactiveRoot),
            L::Removed => (S::Disabled, R::Removed),
            L::ActivationUnknown => (S::Unknown, R::ActivationUnknown),
            L::Unmodeled => (S::UnknownGeometry, R::UnmodeledRuntime),
            L::ActivatedAuthored => match &record.game_object {
                None => (S::UnknownGeometry, R::MissingObjectFacts),
                Some(o) if o.layer > 31 || o.object_id == 0 => {
                    (S::UnknownGeometry, R::MissingObjectFacts)
                }
                Some(o) if self.0.query_layer_mask & (1u32 << o.layer) == 0 => {
                    (S::Disabled, R::OutsideQueryLayers)
                }
                Some(_) if !record.hierarchy_active => (S::Disabled, R::InactiveChild),
                Some(o) if o.volume_components.first() != Some(&record.object_id) => {
                    (S::Disabled, R::NotFirstVolume)
                }
                Some(o) if o.volume_components.len() != 1 => (S::UnknownGeometry, R::SharedTrigger),
                Some(_) if !record.component_enabled => (S::UnknownGeometry, R::DisabledBehaviour),
                Some(_) => (S::Enabled, R::EnabledByCallback),
            },
        };
        Decision {
            registration,
            reason,
        }
    }
    pub fn verify(
        self,
        root: &Path,
        installation: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Verified>, String> {
        if cancelled() {
            return Ok(None);
        }
        self.validate()?;
        installation.validate()?;
        if installation.kind != self.kind() || installation.native != self.0.native {
            return Err("Environment runtime profile belongs to another Rust installation".into());
        }
        for source in std::iter::once(&self.0.native).chain(&self.0.code_sources) {
            if cancelled() {
                return Ok(None);
            }
            if crate::installation::hash_file(
                root,
                &source.relative_path,
                1024 * 1024 * 1024,
                &cancelled,
            )? != *source
            {
                return Err("Environment runtime code or native library changed".into());
            }
        }
        if cancelled() {
            return Ok(None);
        }
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        Ok(Some(Verified { model: self, root }))
    }
}
impl Verified {
    pub fn available(
        root: &Path,
        installation: &Installation,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let model = match installation.kind {
            InstallationKind::DedicatedServer => Model::reference()?,
            InstallationKind::Client => {
                let Some(model) = Model::client_reference_for(&installation.native)? else {
                    return Ok(None);
                };
                model
            }
        };
        if installation.native != model.0.native {
            return Ok(None);
        }
        model.verify(root, installation, cancelled)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn native(&self) -> &SourceFile {
        &self.model.0.native
    }
    pub fn id(&self) -> &str {
        self.model.id()
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        self.model.fingerprint()
    }
    /// Per-record response; this does not cover added components or changed type
    /// masks on an unmodeled occurrence. The provider bridge also installs an
    /// all-mask unknown marker for that whole occurrence, including empty captures.
    pub fn registration(&self, record: &EnvironmentRecord, lifecycle: Lifecycle) -> Decision {
        self.model.modeled_registration(record, lifecycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> EnvironmentRecord {
        serde_json::from_value(serde_json::json!({"object_id":50,"hierarchy_active":true,"component_enabled":true,"type_mask":17,"center":[0,0,0],"size":[1,1,1],"local_to_root":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],"collider_types":[64],"collider_ids":[52],"rotation_chain":[],"field_names":["Center","Size","Type"],"game_object":{"object_id":90,"layer":18,"volume_components":[50],"collider_enabled":[false]}})).unwrap()
    }
    #[test]
    fn current_client_retains_reviewed_activation_rules_and_distinct_source_identity() {
        let old = Model::client_reference().unwrap();
        let current = Model::current_client_reference().unwrap();
        assert_eq!(current.id(), "client-199a470f-environment-1");
        assert_eq!(
            current.native().sha256,
            "8dd4e23570729ddbd72844bdb8a5e04bfa671612b3e7d34841d17d2353f7015c"
        );
        assert_ne!(current.fingerprint().unwrap(), old.fingerprint().unwrap());
        for lifecycle in [
            Lifecycle::SerializedOnly,
            Lifecycle::ActivatedAuthored,
            Lifecycle::RootInactive,
            Lifecycle::Removed,
            Lifecycle::ActivationUnknown,
            Lifecycle::Unmodeled,
        ] {
            for layer in [16, 18] {
                let mut record = record();
                record.game_object.as_mut().unwrap().layer = layer;
                assert_eq!(
                    current.modeled_registration(&record, lifecycle),
                    old.modeled_registration(&record, lifecycle)
                );
            }
        }
        let installation = Installation {
            kind: InstallationKind::Client,
            native: current.native().clone(),
            scene_manifest: None,
        };
        assert!(
            Verified::available(
                Path::new("missing-current-installation"),
                &installation,
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn updated_client_selection_checks_exact_identity_and_reviewed_activation() {
        let profile = Model::parse(include_str!(
            "../../../compatibility/monuments/client-516653f6-environment.json"
        ))
        .unwrap();
        let native = profile.native().clone();
        let selected = Model::client_reference_for(&native).unwrap().unwrap();
        assert_eq!(selected.id(), "client-516653f6-environment-1");
        assert_eq!(selected.0.query_layer_mask, 1 << 18);
        let previous = Model::current_client_reference().unwrap();
        assert_ne!(
            selected.fingerprint().unwrap(),
            previous.fingerprint().unwrap()
        );
        for lifecycle in [
            Lifecycle::SerializedOnly,
            Lifecycle::ActivatedAuthored,
            Lifecycle::RootInactive,
            Lifecycle::Removed,
            Lifecycle::ActivationUnknown,
            Lifecycle::Unmodeled,
        ] {
            for layer in 0..32 {
                let mut record = record();
                record.game_object.as_mut().unwrap().layer = layer;
                assert_eq!(
                    selected.modeled_registration(&record, lifecycle),
                    previous.modeled_registration(&record, lifecycle)
                );
            }
        }
        let installation = Installation {
            kind: InstallationKind::Client,
            native: native.clone(),
            scene_manifest: None,
        };
        assert!(
            Verified::available(
                Path::new("missing-updated-installation"),
                &installation,
                || false
            )
            .is_err()
        );
        let mut changed = native;
        changed.bytes -= 1;
        assert!(Model::client_reference_for(&changed).unwrap().is_none());
        changed.sha256 = "c".repeat(64);
        assert!(Model::client_reference_for(&changed).unwrap().is_none());
    }

    #[test]
    fn lifecycle_separates_serialized_records_activation_and_unmodeled_mutation() {
        let model = Model::reference().unwrap();
        let record = record();
        for (lifecycle, expected) in [
            (Lifecycle::SerializedOnly, Registration::Disabled),
            (Lifecycle::RootInactive, Registration::Disabled),
            (Lifecycle::Removed, Registration::Disabled),
            (Lifecycle::ActivationUnknown, Registration::Unknown),
            (Lifecycle::Unmodeled, Registration::UnknownGeometry),
            (Lifecycle::ActivatedAuthored, Registration::Enabled),
        ] {
            assert_eq!(
                model.modeled_registration(&record, lifecycle).registration,
                expected,
                "{lifecycle:?}"
            );
        }
        // The existing mesh's serialized-disabled flag is superseded by the
        // source OnEnable callback; it is not proof of runtime absence.
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .reason,
            Reason::EnabledByCallback
        );
    }
    #[test]
    fn layers_first_component_child_activity_and_disabled_behaviours_are_distinct() {
        let model = Model::reference().unwrap();
        let mut record = record();
        record.game_object.as_mut().unwrap().layer = 16;
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .reason,
            Reason::OutsideQueryLayers
        );
        record.game_object.as_mut().unwrap().layer = 18;
        record.hierarchy_active = false;
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .reason,
            Reason::InactiveChild
        );
        record.hierarchy_active = true;
        record.component_enabled = false;
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .registration,
            Registration::UnknownGeometry
        );
        record.component_enabled = true;
        record.game_object.as_mut().unwrap().volume_components = vec![50, 51];
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .reason,
            Reason::SharedTrigger
        );
        record.object_id = 51;
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .reason,
            Reason::NotFirstVolume
        );
        record.game_object.as_mut().unwrap().layer = 255;
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .registration,
            Registration::UnknownGeometry
        );
        record.game_object = None;
        assert_eq!(
            model
                .modeled_registration(&record, Lifecycle::ActivatedAuthored)
                .reason,
            Reason::MissingObjectFacts
        );
    }
    fn fixture() -> (tempfile::TempDir, Model, Installation) {
        fixture_for(InstallationKind::DedicatedServer)
    }
    fn fixture_for(kind: InstallationKind) -> (tempfile::TempDir, Model, Installation) {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().unwrap();
        let mut model = match kind {
            InstallationKind::DedicatedServer => Model::reference().unwrap(),
            InstallationKind::Client => Model::client_reference().unwrap(),
        };
        for source in std::iter::once(&mut model.0.native).chain(&mut model.0.code_sources) {
            let path = root.path().join(&source.relative_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = source.relative_path.as_bytes();
            std::fs::write(&path, bytes).unwrap();
            source.bytes = bytes.len() as u64;
            source.sha256 = format!("{:x}", Sha256::digest(bytes));
        }
        let installation = Installation {
            kind,
            native: model.0.native.clone(),
            scene_manifest: None,
        };
        (root, model, installation)
    }
    #[test]
    fn verification_binds_native_code_and_root_and_rejects_changed_receipts() {
        let (root, model, installation) = fixture();
        let verified = model
            .verify(root.path(), &installation, || false)
            .unwrap()
            .unwrap();
        assert_eq!(verified.root(), root.path().canonicalize().unwrap());
        assert_eq!(verified.native(), &installation.native);
        assert_eq!(
            verified
                .registration(&record(), Lifecycle::ActivatedAuthored)
                .registration,
            Registration::Enabled
        );
        let (root, model, installation) = fixture();
        std::fs::write(
            root.path()
                .join("RustDedicated_Data/Managed/Assembly-CSharp.dll"),
            b"changed",
        )
        .unwrap();
        assert!(model.verify(root.path(), &installation, || false).is_err());
        let (root, model, mut installation) = fixture();
        installation.native.sha256 = "f".repeat(64);
        assert!(model.verify(root.path(), &installation, || false).is_err());
    }
    #[test]
    fn model_fingerprint_distinguishes_policy_changes_even_with_the_same_label() {
        let model = Model::reference().unwrap();
        let mut changed = Model::reference().unwrap();
        changed.0.query_layer_mask = 1 << 16;
        assert_eq!(model.id(), changed.id());
        assert_ne!(model.fingerprint().unwrap(), changed.fingerprint().unwrap());
    }
    #[test]
    fn cancellation_and_unrecognized_installations_do_not_guess_a_program() {
        let (_, model, installation) = fixture();
        assert!(
            model
                .verify(Path::new("missing-installation"), &installation, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            Verified::available(Path::new("missing-installation"), &installation, || false)
                .unwrap()
                .is_none()
        );
        let mut model = Model::reference().unwrap();
        model.0.program = "future-program".into();
        assert!(model.validate().is_err());
    }
    #[test]
    fn client_receipts_bind_code_metadata_and_kind_without_guessing_unknown_builds() {
        let (root, model, installation) = fixture_for(InstallationKind::Client);
        let verified = model
            .verify(root.path(), &installation, || false)
            .unwrap()
            .unwrap();
        assert_eq!(verified.id(), "client-e7156192-environment-1");
        // The fixture shares a profile label but has different file receipts.
        // A water adapter must bind actual code identity, not that label.
        assert!(
            crate::placement_water::Model::for_runtime(&verified)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            verified
                .registration(&record(), Lifecycle::ActivatedAuthored)
                .registration,
            Registration::Enabled
        );
        for relative in [
            "GameAssembly.dll",
            "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
        ] {
            let (root, model, installation) = fixture_for(InstallationKind::Client);
            std::fs::write(root.path().join(relative), b"changed").unwrap();
            assert!(model.verify(root.path(), &installation, || false).is_err());
        }
        let (root, model, mut installation) = fixture_for(InstallationKind::Client);
        installation.kind = InstallationKind::DedicatedServer;
        assert!(model.verify(root.path(), &installation, || false).is_err());
        let (_, _, installation) = fixture_for(InstallationKind::Client);
        assert!(
            Verified::available(Path::new("missing-installation"), &installation, || false)
                .unwrap()
                .is_none()
        );
        let mut invalid = Model::client_reference().unwrap();
        invalid.0.code_sources[1].relative_path =
            "RustDedicated_Data/Managed/UnityEngine.CoreModule.dll".into();
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn server_fingerprint_keeps_legacy_kind_omission_and_lifecycle_policies_agree() {
        let server = Model::reference().unwrap();
        let raw: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/monuments/server-bd19c4ac-environment.json"
        ))
        .unwrap();
        // Original struct serialization order, with no newly invented kind field.
        let original = serde_json::json!({"format":raw["format"],"schema":raw["schema"],"id":raw["id"],"native":raw["native"],"code_sources":raw["code_sources"],"query_layer_mask":raw["query_layer_mask"],"program":raw["program"]});
        let serialized = serde_json::to_value(&server.0).unwrap();
        assert_eq!(serialized, original);
        assert!(serialized.get("kind").is_none());
        assert_eq!(
            server.fingerprint().unwrap(),
            "007549b526c6e5e9fc34de25c7235123c1f6ce49108131c120bdf3abb3476627"
        );
        let client = Model::client_reference().unwrap();
        assert_ne!(client.fingerprint().unwrap(), server.fingerprint().unwrap());
        for state in [
            Lifecycle::SerializedOnly,
            Lifecycle::ActivatedAuthored,
            Lifecycle::RootInactive,
            Lifecycle::Removed,
            Lifecycle::ActivationUnknown,
            Lifecycle::Unmodeled,
        ] {
            assert_eq!(
                client.modeled_registration(&record(), state),
                server.modeled_registration(&record(), state)
            );
        }
    }
}
