//! Installation-bound erosion coefficients and early/late procedural phases.
use crate::{
    installation::{self, Installation, InstallationKind, SourceFile},
    monument_stages,
    road_object_source::{self, ProceduralStage},
};
use papyrus_terrain::{erosion_bounds, erosion_grid, erosion_pipeline, erosion_splat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

pub const VERSION: &str = "papyrus-installation-erosion-1";
const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/erosion/client-0f703d43.json"),
    include_str!("../../../compatibility/erosion/client-8dd4e235.json"),
    include_str!("../../../compatibility/erosion/client-58f3263f.json"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeWindow {
    owner: String,
    address: u64,
    bytes: usize,
    sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StageIdentity {
    object_id: i64,
    order: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Coefficients {
    iterations: usize,
    time_step: f32,
    water_fill_rate: f32,
    evaporation_rate: f32,
    gravity: f32,
    pipe_area: f32,
    pipe_length: f32,
    minimum_angle: f32,
    sediment_capacity: f32,
    dissolve_rate: f32,
    depth_fade: f32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Activation {
    Disabled,
    Hierarchy,
}

impl Coefficients {
    fn policy(&self, size: u32, resolution: usize, ocean_level: f32) -> erosion_grid::Policy {
        // Match the source's left-to-right world-X/res * world-Z/res order.
        let cell_area = size as f32 / resolution as f32 * size as f32 / resolution as f32;
        erosion_grid::Policy {
            iterations: self.iterations,
            time_step: self.time_step,
            water_fill_rate: self.water_fill_rate,
            evaporation_rate: self.evaporation_rate,
            gravity: self.gravity,
            pipe_area: self.pipe_area,
            pipe_length: self.pipe_length,
            cell_area,
            normal_y: size as f32 / 1000. / resolution as f32,
            ocean_level,
            minimum_angle: self.minimum_angle,
            sediment_capacity: self.sediment_capacity,
            dissolve_rate: self.dissolve_rate,
            depth_fade: self.depth_fade,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    native_sha256: String,
    code_sources: Vec<SourceFile>,
    map_scene: SourceFile,
    realm_mask: i32,
    activation: Activation,
    erosion: StageIdentity,
    materials: StageIdentity,
    river: StageIdentity,
    code_windows: Vec<CodeWindow>,
    coefficients: Coefficients,
    bounds: erosion_bounds::Policy,
    paint: erosion_splat::Policy,
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Profile {
    fn decode(raw: &str) -> Result<Self, String> {
        let profile: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        installation::validate_source_receipts(
            &profile.code_sources,
            std::slice::from_ref(&profile.map_scene),
        )?;
        profile.bounds.validate()?;
        profile.paint.validate()?;
        profile.coefficients.policy(4000, 513, 0.).validate()?;
        let stages = [profile.erosion, profile.river, profile.materials];
        let mut addresses = BTreeSet::new();
        if profile.format != "papyrus-installation-erosion-profile"
            || profile.schema != 1
            || !hash(&profile.native_sha256)
            || !matches!(profile.realm_mask, 1 | 2)
            || profile.code_sources.len() != 2
            || profile.code_sources[0].relative_path != "GameAssembly.dll"
            || profile.code_sources[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
            || profile.map_scene.relative_path != "Bundles/maps/maps.bundle"
            || profile
                .code_sources
                .iter()
                .chain(std::iter::once(&profile.map_scene))
                .any(|receipt| receipt.bytes > 1024 * 1024 * 1024)
            || stages.iter().any(|s| s.object_id <= 0 || s.order > 65535)
            || stages
                .iter()
                .map(|s| s.object_id)
                .collect::<BTreeSet<_>>()
                .len()
                != 3
            || !stages.windows(2).all(|s| s[0].order < s[1].order)
            || !(1..=128).contains(&profile.code_windows.len())
            || profile.code_windows.iter().any(|w| {
                !w.owner.starts_with("GenerateErosion")
                    || w.owner.len() > 256
                    || w.address == 0
                    || !addresses.insert(w.address)
                    || !(1..=65536).contains(&w.bytes)
                    || !hash(&w.sha256)
            })
        {
            return Err("Invalid installation erosion profile".into());
        }
        Ok(profile)
    }

    fn receipts(&self) -> Vec<SourceFile> {
        self.code_sources
            .iter()
            .chain(std::iter::once(&self.map_scene))
            .cloned()
            .collect()
    }
}

fn selected(native: &str) -> Result<(Profile, &'static str), String> {
    for raw in PROFILES {
        let profile = Profile::decode(raw)?;
        if profile.native_sha256 == native {
            return Ok((profile, raw));
        }
    }
    Err("No erosion adapter matches the selected Rust installation".into())
}

#[derive(Clone, Debug, Serialize)]
pub struct Stage {
    pub object_id: i64,
    pub order: usize,
    pub active: bool,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub engine: &'static str,
    pub profile_sha256: String,
    pub workflow_sha256: String,
    pub early_monuments: Vec<Stage>,
    pub erosion: Stage,
    pub river: Stage,
    pub materials: Stage,
}

pub type Policies = erosion_pipeline::Policy;

pub struct Source {
    profile: Profile,
    plan: Plan,
}

fn active(stage: &ProceduralStage, profile: &Profile) -> bool {
    let enabled = match profile.activation {
        Activation::Disabled => !stage.disabled.unwrap_or(true),
        Activation::Hierarchy => stage.hierarchy_active,
    };
    enabled && stage.mode & profile.realm_mask != 0
}

fn bind_stage(
    workflow: &[ProceduralStage],
    kind: &str,
    identity: StageIdentity,
    profile: &Profile,
) -> Result<Stage, String> {
    let mut matches = workflow.iter().filter(|s| s.kind == kind);
    let stage = matches
        .next()
        .ok_or_else(|| format!("Missing source {kind} stage"))?;
    if matches.next().is_some()
        || stage.object_id != identity.object_id
        || stage.order != identity.order
    {
        return Err(format!(
            "Source {kind} stage differs from the selected erosion profile"
        ));
    }
    Ok(Stage {
        object_id: stage.object_id,
        order: stage.order,
        active: active(stage, profile),
    })
}

fn bind_plan(
    profile: &Profile,
    profile_sha256: String,
    workflow_sha256: String,
    workflow: &[ProceduralStage],
) -> Result<Plan, String> {
    let legacy = matches!(profile.activation, Activation::Disabled);
    if workflow
        .iter()
        .any(|stage| stage.disabled.is_some() != legacy)
    {
        return Err("Erosion activation fields differ from the selected Rust version".into());
    }
    let erosion = bind_stage(workflow, "GenerateErosion", profile.erosion, profile)?;
    let river = bind_stage(workflow, "GenerateRiverLayout", profile.river, profile)?;
    let materials = bind_stage(workflow, "GenerateErosionSplat", profile.materials, profile)?;
    let early_monuments = workflow
        .iter()
        .filter(|s| s.kind == "PlaceMonuments" && s.order < erosion.order)
        .map(|s| Stage {
            object_id: s.object_id,
            order: s.order,
            active: active(s, profile),
        })
        .collect();
    Ok(Plan {
        engine: VERSION,
        profile_sha256,
        workflow_sha256,
        early_monuments,
        erosion,
        river,
        materials,
    })
}

impl Source {
    /// Verify selected files and join the already prepared scene workflow. This
    /// performs no extraction and leaves immutable native terrain caches alone.
    pub fn prepare(
        root: &Path,
        installation: &Installation,
        workflow: &road_object_source::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        installation.validate()?;
        if installation.kind != InstallationKind::Client {
            return Err("Erosion needs a supported selected Rust client installation".into());
        }
        let (profile, raw) = selected(&installation.native.sha256)?;
        let ordered = workflow.procedural_order()?;
        let receipts = profile.receipts();
        if !workflow.contains_source_receipts(&receipts)
            || !workflow.contains_source_receipts(std::slice::from_ref(&installation.native))
        {
            return Err("Erosion workflow belongs to another Rust installation".into());
        }
        let _verification = installation::VerificationScope::default();
        for receipt in std::iter::once(&installation.native).chain(&receipts) {
            if cancelled() {
                return Ok(None);
            }
            let actual = installation::hash_file(
                root,
                &receipt.relative_path,
                1024 * 1024 * 1024,
                &cancelled,
            );
            if cancelled() {
                return Ok(None);
            }
            if actual? != *receipt {
                return Err("Erosion profile differs from the selected Rust files".into());
            }
        }
        let plan = bind_plan(
            &profile,
            format!("{:x}", Sha256::digest(raw.as_bytes())),
            workflow.sha256.clone(),
            ordered,
        )?;
        Ok((!cancelled()).then_some(Self { profile, plan }))
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn policies(
        &self,
        size: u32,
        resolution: usize,
        ocean_level: f32,
    ) -> Result<Policies, String> {
        if !(1000..=6000).contains(&size) || !(3..=4097).contains(&resolution) {
            return Err("Invalid erosion world dimensions".into());
        }
        let grid = self
            .profile
            .coefficients
            .policy(size, resolution, ocean_level);
        grid.validate()?;
        Ok(Policies {
            grid,
            bounds: self.profile.bounds,
            paint: self.profile.paint,
        })
    }

    /// Execute early erosion using the selected installation's bound policy.
    /// Publication of returned terrain and retained painting data stays atomic
    /// at the generation coordinator's stage boundary.
    pub fn erode(
        &self,
        layers: &papyrus_terrain::native_layers::Layers,
        limits: erosion_pipeline::Limits,
        pool: &rayon::ThreadPool,
        cancelled: impl Fn() -> bool + Sync,
        progress: impl FnMut(usize, usize) + Send,
    ) -> Result<Option<erosion_pipeline::Eroded>, String> {
        if !self.plan.erosion.active {
            return Err("Selected Rust workflow skips hydraulic erosion".into());
        }
        erosion_pipeline::erode(
            layers,
            self.policies(layers.size, layers.height_resolution, 0.)?,
            limits,
            pool,
            cancelled,
            progress,
        )
    }

    pub fn early_groups(
        &self,
        monuments: &monument_stages::Source,
    ) -> Result<Vec<(Stage, Vec<String>)>, String> {
        let expected = self
            .plan
            .early_monuments
            .iter()
            .map(|s| s.object_id)
            .collect::<Vec<_>>();
        let groups = monuments.groups_for_prefix(&expected, &self.profile.receipts())?;
        Ok(self
            .plan
            .early_monuments
            .iter()
            .cloned()
            .zip(groups)
            .collect())
    }

    /// Prefab membership comes from the selected client's monument programs.
    pub fn early_paths(&self, monuments: &monument_stages::Source) -> Result<Vec<String>, String> {
        let expected = self
            .plan
            .early_monuments
            .iter()
            .map(|s| s.object_id)
            .collect::<Vec<_>>();
        monuments.paths_for_prefix(&expected, &self.profile.receipts())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(profile: &Profile) -> Vec<ProceduralStage> {
        let mut stages: Vec<_> = (0..=profile.materials.order)
            .map(|order| ProceduralStage {
                object_id: 10000 + order as i64,
                order,
                kind: "Other".into(),
                description: String::new(),
                mode: -1,
                disabled: matches!(profile.activation, Activation::Disabled).then_some(false),
                component_enabled: false,
                hierarchy_active: true,
            })
            .collect();
        for (identity, kind) in [
            (profile.erosion, "GenerateErosion"),
            (profile.river, "GenerateRiverLayout"),
            (profile.materials, "GenerateErosionSplat"),
        ] {
            stages[identity.order].object_id = identity.object_id;
            stages[identity.order].kind = kind.into();
        }
        for stage in &mut stages[5..10] {
            stage.kind = "PlaceMonuments".into();
        }
        stages[20].kind = "PlaceMonuments".into();
        stages
    }

    #[test]
    fn selected_profiles_bind_ordered_phases_and_keep_source_cell_area_rounding() {
        for raw in PROFILES {
            let profile = Profile::decode(raw).unwrap();
            let stages = workflow(&profile);
            let plan = bind_plan(&profile, "a".repeat(64), "b".repeat(64), &stages).unwrap();
            assert_eq!(
                plan.early_monuments
                    .iter()
                    .map(|s| s.order)
                    .collect::<Vec<_>>(),
                [5, 6, 7, 8, 9]
            );
            assert!(plan.erosion.active && plan.river.active && plan.materials.active);
            let source = Source { profile, plan };
            let policies = source.policies(4000, 513, 0.).unwrap();
            assert_eq!(policies.grid.cell_area.to_bits(), 0x42733093);
            assert_eq!(policies.grid.normal_y, 4000_f32 / 1000. / 513.);
            assert_eq!(policies.grid.iterations, 512);
            assert_eq!(
                [
                    policies.grid.time_step,
                    policies.grid.water_fill_rate,
                    policies.grid.evaporation_rate,
                    policies.grid.gravity,
                    policies.grid.pipe_area,
                    policies.grid.pipe_length,
                    policies.grid.minimum_angle,
                    policies.grid.sediment_capacity,
                    policies.grid.dissolve_rate,
                    policies.grid.depth_fade
                ],
                [
                    0.06, 0.04, 0.015, 10., 1., 1., 0.01047198, 0.0015, 0.15, 10.
                ],
            );
            assert_eq!(policies.bounds.protected_topology, 0x14080);
            assert_eq!(policies.paint.protected_topology, 0xb4990);
            assert_eq!(policies.paint.channels, [1, 2, 0, 6, 7]);
            assert!(source.policies(0, 513, 0.).is_err());
            assert!(source.policies(4000, usize::MAX, 0.).is_err());
            assert!(source.policies(4000, 513, f32::NAN).is_err());
        }
        assert!(selected(&"0".repeat(64)).is_err());
    }

    #[test]
    fn current_client_profile_matches_the_captured_zero_based_workflow() {
        let (profile, _) =
            selected("58f3263fd5cc1ab31942e14b902e52ff952b65fa08896226d15181a01b486750").unwrap();
        let captured = [
            (611, 10, "GenerateErosion"),
            (576, 11, "GenerateRiverLayout"),
            (653, 72, "GenerateErosionSplat"),
        ];
        let mut stages = captured.map(|(object_id, order, kind)| ProceduralStage {
            object_id,
            order,
            kind: kind.into(),
            description: String::new(),
            mode: -1,
            disabled: None,
            component_enabled: true,
            hierarchy_active: true,
        });
        let plan = bind_plan(&profile, "a".repeat(64), "b".repeat(64), &stages).unwrap();
        assert!(plan.erosion.active && plan.river.active && plan.materials.active);
        for stage in &mut stages {
            stage.order += 1;
        }
        assert!(bind_plan(&profile, "a".repeat(64), "b".repeat(64), &stages).is_err());
    }

    #[test]
    fn source_activation_preserves_each_versions_gate_and_realm() {
        for raw in PROFILES {
            let profile = Profile::decode(raw).unwrap();
            let mut stages = workflow(&profile);
            let bind = |stages: &[ProceduralStage]| {
                bind_plan(&profile, "a".repeat(64), "b".repeat(64), stages).unwrap()
            };
            stages[profile.erosion.order].hierarchy_active = false;
            assert_eq!(
                bind(&stages).erosion.active,
                matches!(profile.activation, Activation::Disabled)
            );
            stages[profile.erosion.order].hierarchy_active = true;
            if matches!(profile.activation, Activation::Disabled) {
                stages[profile.erosion.order].disabled = Some(true);
                assert!(!bind(&stages).erosion.active);
                stages[profile.erosion.order].disabled = Some(false);
            }
            stages[profile.erosion.order].mode = profile.realm_mask;
            assert!(bind(&stages).erosion.active);
            stages[profile.erosion.order].mode = profile.realm_mask ^ 3;
            assert!(!bind(&stages).erosion.active);
        }
    }

    #[test]
    fn incomplete_duplicated_and_changed_phase_identities_are_rejected() {
        let profile = Profile::decode(PROFILES[1]).unwrap();
        let stages = workflow(&profile);
        for index in [
            profile.erosion.order,
            profile.materials.order,
            profile.river.order,
        ] {
            let mut changed = stages.clone();
            changed[index].object_id += 1;
            assert!(bind_plan(&profile, "a".repeat(64), "b".repeat(64), &changed).is_err());
            changed[index] = stages[index].clone();
            changed[index].kind = "Other".into();
            assert!(bind_plan(&profile, "a".repeat(64), "b".repeat(64), &changed).is_err());
            changed[index] = stages[index].clone();
            changed[0].kind = stages[index].kind.clone();
            assert!(bind_plan(&profile, "a".repeat(64), "b".repeat(64), &changed).is_err());
        }
    }

    #[test]
    fn malformed_profiles_cannot_supply_phase_or_numeric_policy() {
        let original: serde_json::Value = serde_json::from_str(PROFILES[0]).unwrap();
        for (path, value) in [
            ("/schema", serde_json::json!(2)),
            ("/realm_mask", serde_json::json!(0)),
            ("/native_sha256", serde_json::json!("invalid")),
            (
                "/code_sources/0/relative_path",
                serde_json::json!("Other.dll"),
            ),
            ("/code_sources/0/bytes", serde_json::json!(0)),
            ("/code_windows/0/sha256", serde_json::json!("invalid")),
            ("/code_windows/0/bytes", serde_json::json!(65537)),
            ("/erosion/order", serde_json::json!(72)),
            (
                "/materials/object_id",
                original["erosion"]["object_id"].clone(),
            ),
            ("/coefficients/time_step", serde_json::json!(-1)),
            ("/bounds/radius_step", serde_json::json!(0)),
            ("/paint/channels", serde_json::json!([0, 0, 0, 0, 0])),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(Profile::decode(&changed.to_string()).is_err(), "{path}");
        }
    }

    #[test]
    fn selected_paint_policies_match_all_owned_managed_material_results() {
        #[derive(Deserialize)]
        struct Capture {
            rows: Vec<Row>,
        }
        #[derive(Deserialize)]
        struct Row {
            height_resolution: usize,
            splat_resolution: usize,
            topology_resolution: usize,
            terrain_one_over_size: f32,
            height_delta: Vec<f32>,
            slopes_degrees: Vec<f32>,
            topology: Vec<i32>,
            splat: Vec<u8>,
            result: Vec<u8>,
        }
        let capture: Capture = serde_json::from_str(include_str!(
            "../../papyrus-terrain/tests/fixtures/erosion-splat-bd19c4ac.json"
        ))
        .unwrap();
        assert_eq!(capture.rows.len(), 48);
        for raw in PROFILES {
            let profile = Profile::decode(raw).unwrap();
            for row in &capture.rows {
                let actual = erosion_splat::paint(
                    erosion_splat::Input {
                        height_resolution: row.height_resolution,
                        splat_resolution: row.splat_resolution,
                        topology_resolution: row.topology_resolution,
                        terrain_one_over_size: row.terrain_one_over_size,
                        height_delta: &row.height_delta,
                        slopes_degrees: &row.slopes_degrees,
                        topology: &row.topology,
                        splat: &row.splat,
                    },
                    profile.paint,
                    erosion_splat::Limits {
                        maximum_bytes: 1024 * 1024,
                        maximum_work: 1024 * 1024,
                    },
                    || false,
                )
                .unwrap()
                .unwrap();
                assert_eq!(actual.splat, row.result);
            }
        }
    }
}
