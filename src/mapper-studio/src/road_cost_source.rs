//! Explicit installation-bound road cost policy, independent of sibling asset caches.
use crate::{
    Project, infrastructure::Source, installation::SourceFile, prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use papyrus_terrain::{native_layers::Layers, placement::Solution, road_costmap};
use serde::{Deserialize, Serialize};
pub const COASTAL_ENTRANCES_VERSION: &str = "papyrus-coastal-road-entrances-1";
pub const PROJECTED_COASTAL_ENTRANCES_VERSION: &str = "papyrus-coastal-road-entrances-2";
pub const LATTICE_COASTAL_ENTRANCES_VERSION: &str = "papyrus-coastal-road-entrances-3";
pub const GROUND_GUIDED_COASTAL_ENTRANCES_VERSION: &str = "papyrus-coastal-road-entrances-4";
const BUNDLED_PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/infrastructure/client-0f703d43-road-reservations.json"),
    include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-reservations.json"),
    include_str!("../../../compatibility/infrastructure/client-58f3263f-road-reservations.json"),
];
const SEMANTIC_CAPTURES: [&str; 3] = [
    include_str!("../../../compatibility/infrastructure/client-0f703d43-road-semantics.json"),
    include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-semantics.json"),
    include_str!("../../../compatibility/infrastructure/client-58f3263f-road-semantics.json"),
];
/// The source road costmap reads the existing placement map after rail paths
/// reserve their strips. This is a soft source cost, not a collision obstacle.
pub(crate) fn with_retained_placement(
    project: &Project,
    maps: &papyrus_terrain::monument_composition::Composed,
    receipt: Option<&crate::retained_rail_scene::Receipt>,
    mut placement: Vec<u8>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<u8>>, String> {
    if cancelled() {
        return Ok(None);
    }
    let Some(receipt) = receipt else {
        return Ok(Some(placement));
    };
    let scene = maps
        .rails
        .as_ref()
        .ok_or("Missing retained rail placement scene")?;
    let recipe = project
        .rails
        .as_ref()
        .ok_or("Missing retained rail placement recipe")?;
    let n = maps.layers.surface_resolution;
    if placement.len()
        != n.checked_mul(n)
            .ok_or("Road placement dimensions overflow")?
        || placement.iter().any(|&v| v > 1)
    {
        return Err("Invalid initial road placement raster".into());
    }
    if receipt.check(scene, recipe, n, &cancelled)?.is_none() {
        return Ok(None);
    }
    for (i, (a, &b)) in placement
        .iter_mut()
        .zip(&scene.reservations.raster)
        .enumerate()
    {
        if i % 4096 == 0 && cancelled() {
            return Ok(None);
        }
        *a |= b;
    }
    Ok((!cancelled()).then_some(placement))
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub format: String,
    pub schema: u32,
    pub id: String,
    pub engine: String,
    pub infrastructure_sha256: String,
    pub sources: Vec<SourceFile>,
    pub policy: road_costmap::Policy,
    pub maximum_work: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joins: Option<papyrus_terrain::road_ring_join::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_reservations: Option<papyrus_terrain::road_reservations::Policy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coastal_entrances: Option<String>,
}
impl Profile {
    /// Optional compatibility metadata, selected only by the complete verified
    /// infrastructure identity and ordered installation receipts.
    pub fn bundled_joined(source: &Source) -> Result<Option<Self>, String> {
        let Ok(receipts) = source.road_object_receipts() else {
            return Ok(None);
        };
        if let Some(profile) = Self::bundled_for_receipts(&source.sha256, &receipts)? {
            return Ok(Some(profile));
        }
        for (raw, capture) in BUNDLED_PROFILES.into_iter().zip(SEMANTIC_CAPTURES) {
            let profile: Self = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            let capture: SemanticCapture =
                serde_json::from_str(capture).map_err(|e| e.to_string())?;
            if capture.format != "papyrus-road-source-semantics" || capture.schema != 1 {
                return Err("Unsupported road semantic capture".into());
            }
            if let Some(profile) = Self::for_semantics(
                profile,
                source,
                &capture.infrastructure_sha256,
                &capture.semantic_sha256,
            )? {
                return Ok(Some(profile));
            }
        }
        Ok(None)
    }
    fn for_semantics(
        mut profile: Self,
        source: &Source,
        reference: &str,
        semantic: &str,
    ) -> Result<Option<Self>, String> {
        profile.validate()?;
        if profile.infrastructure_sha256 != reference
            || profile.sources != source.road_object_receipts()?
            || source.semantic_sha256()? != semantic
        {
            return Ok(None);
        }
        profile.infrastructure_sha256 = source.sha256.clone();
        profile.check(source)?;
        Ok(Some(profile))
    }
    fn bundled_for_receipts(
        infrastructure: &str,
        receipts: &[SourceFile],
    ) -> Result<Option<Self>, String> {
        for raw in BUNDLED_PROFILES {
            let profile: Self = serde_json::from_str(raw)
                .map_err(|e| format!("Invalid bundled road routing profile: {e}"))?;
            profile.validate()?;
            if profile.infrastructure_sha256 == infrastructure
                && profile.sources == receipts
                && profile.joins.is_some()
            {
                return Ok(Some(profile));
            }
        }
        Ok(None)
    }
    pub fn validate(&self) -> Result<(), String> {
        self.policy.validate()?;
        if self.coastal_entrances.as_deref().is_some_and(|v| {
            ![
                COASTAL_ENTRANCES_VERSION,
                PROJECTED_COASTAL_ENTRANCES_VERSION,
                LATTICE_COASTAL_ENTRANCES_VERSION,
                GROUND_GUIDED_COASTAL_ENTRANCES_VERSION,
            ]
            .contains(&v)
        }) {
            return Err("Unsupported coastal road entrance policy".into());
        }
        if self.policy.sampling.is_some()
            && self
                .joins
                .as_ref()
                .is_none_or(|p| p.source_grid_spacing != 7.5)
        {
            return Err("Native road sampling requires the captured source lattice".into());
        }
        if let Some(policy) = &self.path_reservations {
            policy.validate()?;
        }
        if let Some(joins) = &self.joins {
            joins.validate()?;
        }
        if self.format != "papyrus-road-cost-profile"
            || self.schema != 1
            || self.engine != road_costmap::VERSION
            || self.id.is_empty()
            || self.id.len() > 128
            || self.infrastructure_sha256.len() != 64
            || !self
                .infrastructure_sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || !(4..=8).contains(&self.sources.len())
            || !(1..=128_000_000).contains(&self.maximum_work)
        {
            return Err("Invalid bound road cost profile".into());
        }
        Ok(())
    }
    pub fn check(&self, source: &Source) -> Result<(), String> {
        self.validate()?;
        if self.infrastructure_sha256 != source.sha256
            || self.sources != source.road_object_receipts()?
        {
            return Err("Road cost profile differs from the selected infrastructure or installation receipts".into());
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)] // Explicit verified sources and current terrain.
    pub fn prepare(
        &self,
        project: &Project,
        layout: &Solution,
        catalog: &Catalog,
        bindings: &Bindings,
        source: &Source,
        layers: &Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<road_costmap::Grid>, String> {
        let Some(blockers) =
            self.prepare_blockers(project, layout, catalog, bindings, source, &cancelled)?
        else {
            return Ok(None);
        };
        self.build_family(layers, &blockers, project.seeds[3].value, false, cancelled)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_blockers(
        &self,
        project: &Project,
        layout: &Solution,
        catalog: &Catalog,
        bindings: &Bindings,
        source: &Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Vec<u8>>, String> {
        self.check(source)?;
        let monuments = crate::monument_source::Source::cached_shared(
            project
                .monument_rules
                .as_ref()
                .ok_or("Source road costs require pinned monument rules")?,
            catalog,
            bindings,
            &cancelled,
        )?;
        let Some(blockers) = crate::monument_blockers::generate(
            project,
            layout,
            &monuments,
            self.maximum_work,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(blockers.cells))
    }
    pub fn build_family(
        &self,
        layers: &Layers,
        placement: &[u8],
        seed: u32,
        trail: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<road_costmap::Grid>, String> {
        self.validate()?;
        road_costmap::build(
            papyrus_terrain::rail_costmap::Terrain {
                size: layers.size as f32,
                height_resolution: layers.height_resolution,
                height: &layers.height,
                topology_resolution: layers.surface_resolution,
                topology: &layers.topology,
                placement_resolution: layers.surface_resolution,
                placement,
            },
            &self.policy,
            257,
            seed,
            trail,
            self.maximum_work,
            cancelled,
        )
    }
    /// Preparation for the source-lattice layout revision. Not selected by old
    /// recipes: their routing and saved cost-grid audits use vertex coordinates.
    pub fn build_cell_family(
        &self,
        source: &Source,
        layers: &Layers,
        placement: &[u8],
        seed: u32,
        trail: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<road_costmap::CellGrid>, String> {
        self.check(source)?;
        let spacing = self
            .joins
            .as_ref()
            .ok_or("Source cell routing requires captured grid spacing")?
            .source_grid_spacing;
        let lattice = road_costmap::CellLattice::new(layers.size as f32, spacing)?;
        road_costmap::build_cell_centred(
            papyrus_terrain::rail_costmap::Terrain {
                size: layers.size as f32,
                height_resolution: layers.height_resolution,
                height: &layers.height,
                topology_resolution: layers.surface_resolution,
                topology: &layers.topology,
                placement_resolution: layers.surface_resolution,
                placement,
            },
            &self.policy,
            lattice,
            seed,
            trail,
            self.maximum_work,
            cancelled,
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticCapture {
    format: String,
    schema: u32,
    infrastructure_sha256: String,
    semantic_sha256: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_rail_placement_has_the_source_soft_cost_and_rejects_stale_scene_bindings() {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17039; hr * hr],
            biome: vec![0; sr * sr * 5],
            splat: vec![0; sr * sr * 8],
            topology: vec![0; sr * sr],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut maps =
            papyrus_terrain::monument_composition::compose(&layers, &vec![255; sr * sr], &[], || false)
                .unwrap()
                .unwrap();
        let (recipe, mut scene) = crate::retained_rail_scene::tests::fixture(sr);
        scene.reservations.raster[(sr / 2) * sr..(sr / 2 + 1) * sr].fill(1);
        scene.reservations.report.newly_blocked = sr;
        scene.reservations.report.final_blocked = sr;
        let receipt = crate::retained_rail_scene::Receipt::capture(&scene, &recipe, sr, || false)
            .unwrap()
            .unwrap();
        let project = Project {
            rails: Some(recipe),
            ..Default::default()
        };
        maps.rails = Some(scene);
        let mut initial = vec![0; sr * sr];
        initial[0] = 1;
        assert_eq!(
            with_retained_placement(&project, &maps, None, initial.clone(), || false)
                .unwrap()
                .unwrap(),
            initial
        );
        let merged =
            with_retained_placement(&project, &maps, Some(&receipt), initial.clone(), || false)
                .unwrap()
                .unwrap();
        assert_eq!(merged[0], 1);
        assert!(
            merged[(sr / 2) * sr..(sr / 2 + 1) * sr]
                .iter()
                .all(|&v| v == 1)
        );
        let profile = bundled();
        let before = profile
            .build_family(&layers, &initial, 123, false, || false)
            .unwrap()
            .unwrap();
        let after = profile
            .build_family(&layers, &merged, 123, false, || false)
            .unwrap()
            .unwrap();
        assert_eq!(profile.policy.expensive_cost, 5000);
        assert!(after.counts.placement > before.counts.placement);
        assert!(after.costs.iter().all(|&v| v > 0));
        for (&old, &new) in before.costs.iter().zip(&after.costs) {
            if old != new {
                assert_eq!(new, 5000);
            }
        }
        assert!(
            with_retained_placement(&project, &maps, Some(&receipt), initial.clone(), || true)
                .unwrap()
                .is_none()
        );
        let mut invalid = initial.clone();
        invalid[1] = 2;
        assert!(
            with_retained_placement(&project, &maps, Some(&receipt), invalid, || false).is_err()
        );
        maps.rails.as_mut().unwrap().reservations.raster[1] = 1;
        assert!(
            with_retained_placement(&project, &maps, Some(&receipt), initial, || false).is_err()
        );
    }
    fn bundled() -> Profile {
        serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-reservations.json"
        ))
        .unwrap()
    }
    #[test]
    fn native_sampling_is_explicit_round_tripped_and_requires_source_cells() {
        let mut profile = bundled();
        assert!(
            serde_json::to_value(&profile).unwrap()["policy"]
                .get("sampling")
                .is_none()
        );
        profile.policy.sampling = Some(road_costmap::NATIVE_SAMPLING_VERSION.into());
        profile.validate().unwrap();
        assert_eq!(
            profile,
            serde_json::from_value(serde_json::to_value(&profile).unwrap()).unwrap()
        );
        profile.joins.as_mut().unwrap().source_grid_spacing = 8.;
        assert!(profile.validate().is_err());
        profile.joins = None;
        assert!(profile.validate().is_err());
    }
    #[test]
    fn reservations_are_optional_versioned_and_preserve_legacy_profile_serialization() {
        let old: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-rivers.json"
        ))
        .unwrap();
        let legacy: Profile = serde_json::from_value(old.clone()).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.path_reservations.is_none());
        assert_eq!(serde_json::to_value(legacy).unwrap(), old);
        let current = bundled();
        current.validate().unwrap();
        assert_eq!(
            current,
            serde_json::from_slice(&serde_json::to_vec(&current).unwrap()).unwrap()
        );
        for (field, value) in [
            ("engine", serde_json::json!("unknown")),
            ("width", serde_json::json!(0)),
            ("maximum_work", serde_json::json!(0)),
            ("sample_spacing", serde_json::json!(0)),
            ("spline", serde_json::json!(false)),
        ] {
            let mut bad = serde_json::to_value(&current).unwrap();
            bad["path_reservations"][field] = value;
            assert!(
                serde_json::from_value::<Profile>(bad)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn semantic_binding_ignores_only_cache_provenance_and_keeps_all_gameplay_data_and_receipts() {
        let (_, mut doc) = crate::infrastructure::tests::fixture();
        doc["profile"]["code_sources"] = serde_json::json!([{"relative_path":"GameAssembly.dll","sha256":"b".repeat(64),"bytes":1}]);
        doc["profile"]["map_scene"] = serde_json::json!({"relative_path":"Bundles/maps/maps.bundle","sha256":"c".repeat(64),"bytes":1});
        let mut source: Source = serde_json::from_value(doc.clone()).unwrap();
        source.sha256 = "e".repeat(64);
        let semantic = source.semantic_sha256().unwrap();
        let mut profile = bundled();
        profile.sources = source.road_object_receipts().unwrap();
        let reference = profile.infrastructure_sha256.clone();
        let rebound = Profile::for_semantics(profile.clone(), &source, &reference, &semantic)
            .unwrap()
            .unwrap();
        assert_eq!(rebound.infrastructure_sha256, source.sha256);
        rebound.check(&source).unwrap();
        doc["catalog_sha256"] = "f".repeat(64).into();
        doc["extractor_sha256"] = "d".repeat(64).into();
        let mut changed: Source = serde_json::from_value(doc.clone()).unwrap();
        changed.sha256 = "1".repeat(64);
        assert_eq!(semantic, changed.semantic_sha256().unwrap());
        assert!(
            Profile::for_semantics(profile.clone(), &changed, &reference, &semantic)
                .unwrap()
                .is_some()
        );
        for pointer in [
            "/profile/width",
            "/profile/road_type",
            "/profile/protected_topology",
        ] {
            let mut altered = doc.clone();
            let value = altered.pointer_mut(pointer).unwrap();
            *value = serde_json::json!(999);
            let mut changed: Source = serde_json::from_value(altered).unwrap();
            changed.sha256 = "2".repeat(64);
            assert!(
                Profile::for_semantics(profile.clone(), &changed, &reference, &semantic)
                    .unwrap()
                    .is_none(),
                "{pointer}"
            );
        }
        let mut wrong = profile.clone();
        wrong.sources[0].bytes += 1;
        assert!(
            Profile::for_semantics(wrong, &source, &reference, &semantic)
                .unwrap()
                .is_none()
        );
        assert!(
            Profile::for_semantics(profile, &source, &"0".repeat(64), &semantic)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn family_evaluation_keeps_shared_noise_and_blocks_water_only_for_dirt() {
        let p = bundled();
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![16; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in sr / 3..sr * 2 / 3 {
            for x in sr / 3..sr * 2 / 3 {
                layers.topology[z * sr + x] = p.policy.water_topology as i32;
            }
        }
        let placement = vec![0; cells];
        let paved = p
            .build_family(&layers, &placement, 123, false, || false)
            .unwrap()
            .unwrap();
        let dirt = p
            .build_family(&layers, &placement, 123, true, || false)
            .unwrap()
            .unwrap();
        assert!(paved.counts.water > 100);
        assert_eq!(paved.counts.water, dirt.counts.blocked_trail_water);
        assert!(!paved.trail && dirt.trail);
        assert_eq!(paved.seed, dirt.seed);
        for (&road, &trail) in paved.costs.iter().zip(&dirt.costs) {
            if trail == 0 {
                assert_eq!(road, p.policy.expensive_cost);
            } else {
                assert_eq!(road, trail);
            }
        }
        assert!(
            p.build_family(&layers, &placement, 123, true, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            p.build_family(&layers, &placement[..10], 123, true, || false)
                .is_err()
        );
    }
    #[test]
    fn bundled_joined_policy_requires_every_ordered_receipt_and_parent_identity() {
        for raw in BUNDLED_PROFILES {
            let p: Profile = serde_json::from_str(raw).unwrap();
            p.validate().unwrap();
            assert_eq!(
                Profile::bundled_for_receipts(&p.infrastructure_sha256, &p.sources).unwrap(),
                Some(p.clone())
            );
            assert!(
                Profile::bundled_for_receipts(&"a".repeat(64), &p.sources)
                    .unwrap()
                    .is_none()
            );
            for i in 0..p.sources.len() {
                for field in ["path", "hash", "bytes"] {
                    let mut receipts = p.sources.clone();
                    match field {
                        "path" => receipts[i].relative_path.push_str(".changed"),
                        "hash" => receipts[i].sha256 = "a".repeat(64),
                        "bytes" => receipts[i].bytes += 1,
                        _ => unreachable!(),
                    }
                    assert!(
                        Profile::bundled_for_receipts(&p.infrastructure_sha256, &receipts)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            let mut reversed = p.sources.clone();
            reversed.reverse();
            assert!(
                Profile::bundled_for_receipts(&p.infrastructure_sha256, &reversed)
                    .unwrap()
                    .is_none()
            );
            assert!(
                Profile::bundled_for_receipts(&p.infrastructure_sha256, &[])
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn coastal_entrance_admission_is_versioned_and_optional_for_saved_profiles() {
        let mut historical = bundled();
        historical.coastal_entrances = None;
        let value = serde_json::to_value(&historical).unwrap();
        assert!(value.get("coastal_entrances").is_none());
        assert_eq!(
            serde_json::from_value::<Profile>(value).unwrap(),
            historical
        );

        let mut current = historical;
        current.coastal_entrances = Some(COASTAL_ENTRANCES_VERSION.into());
        current.validate().unwrap();
        current.coastal_entrances = Some(PROJECTED_COASTAL_ENTRANCES_VERSION.into());
        current.validate().unwrap();
        current.coastal_entrances = Some(LATTICE_COASTAL_ENTRANCES_VERSION.into());
        current.validate().unwrap();
        current.coastal_entrances = Some(GROUND_GUIDED_COASTAL_ENTRANCES_VERSION.into());
        current.validate().unwrap();
        current.coastal_entrances = Some("unknown-coastal-entrance-policy".into());
        assert!(current.validate().is_err());
    }
}
