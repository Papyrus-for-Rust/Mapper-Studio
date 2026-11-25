//! Paint a verified roadside selection onto the private land-monument revision.
//! This intermediate stage does not paint roads or certify a playable world.
use crate::{
    road_planning::StraightenedPaths,
    roadside_placement::{Context, Provisional, ValidatedSelection},
};
use papyrus_terrain::monument_composition::{self, Composed};
use serde::Serialize;
use std::cell::{Ref, RefCell};
pub const VERSION: &str = "papyrus-roadside-stamp-stage-1";
#[derive(Debug, Serialize, serde::Deserialize, PartialEq)]
pub struct Placement {
    pub request: u64,
    pub prefab: String,
    pub position: [f32; 3],
    pub yaw: f32,
    pub road: usize,
    pub start_index: usize,
    pub end_index: usize,
}
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct Changes {
    pub height_samples: usize,
    pub splat_cells: usize,
    pub biome_cells: usize,
    pub topology_cells: usize,
    pub alpha_cells: usize,
}
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub roadside_source_sha256: String,
    pub catalog_sha256: String,
    pub bindings_sha256: String,
    pub placements: Vec<Placement>,
    pub missing: Vec<u64>,
    pub changes: Changes,
    pub ocean_reclassified: bool,
    pub existing_local_water_preserved: bool,
    pub accepted_world_published: bool,
    pub environment_volumes_validated: bool,
    pub runtime_physics_validated: bool,
    pub final_access_validated: bool,
}
pub struct StampedStage<'a, M = Composed> {
    pub maps: M,
    pub roads: StraightenedPaths<'a>,
    pub report: Report,
}

/// One private stamped map for a verified pre-road world and source. Road
/// retries still straighten the current network and build a fresh report.
#[derive(Default)]
pub(crate) struct TerrainCache(RefCell<Option<CachedTerrain>>);

struct CachedTerrain {
    stamps: Vec<(String, [u32; 4])>,
    maps: Composed,
    changes: Changes,
}

impl CachedTerrain {
    fn matches(&self, placements: &[Placement]) -> bool {
        self.stamps.len() == placements.len()
            && self.stamps.iter().zip(placements).all(|((path, pose), placement)| {
                *path == placement.prefab && *pose == placement.pose_bits()
            })
    }
}

impl Placement {
    fn pose_bits(&self) -> [u32; 4] {
        let [x, y, z] = self.position;
        [x, y, z, self.yaw].map(f32::to_bits)
    }
}

impl TerrainCache {
    pub(crate) fn take(&self) -> Result<Composed, String> {
        self.0
            .take()
            .map(|cached| cached.maps)
            .ok_or_else(|| "Missing staged roadside terrain".into())
    }

    pub(crate) fn trim(&self, maximum_raster_bytes: usize) {
        let mut cached = self.0.borrow_mut();
        if cached.as_ref().is_some_and(|cached| {
            let maps = &cached.maps;
            let layers = &maps.layers;
            let raster_bytes = layers.height.capacity() * size_of::<i16>()
                + layers.topology.capacity() * size_of::<i32>()
                + layers.splat.capacity()
                + layers.biome.capacity()
                + maps.alpha.capacity()
                + maps.ocean.as_ref().map_or(0, |ocean| ocean.bytes())
                + maps.rails.as_ref().map_or(0, |rails| {
                    rails.reservations.raster.capacity()
                });
            raster_bytes > maximum_raster_bytes
        }) {
            cached.take();
        }
    }
}
fn finish(
    base: &Composed,
    mut out: Composed,
    rails: Option<&crate::rail_generation::Recipe>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<(Composed, Changes)>, String> {
    if cancelled() {
        return Ok(None);
    }
    if out.clamped_height_samples != 0 {
        return Err("Roadside stamp clamped terrain heights".into());
    }
    out.clamped_height_samples = base.clamped_height_samples;
    out.rivers = base.rivers.clone();
    out.tunnels = base.tunnels.clone();
    if out.rails.is_some() {
        return Err("Roadside stamp already contains an unexpected rail scene".into());
    }
    match (&base.rails, rails) {
        (Some(scene), Some(recipe)) => {
            let Some(scene) = crate::retained_rail_scene::copy(
                scene,
                recipe,
                base.layers.surface_resolution,
                cancelled,
            )?
            else {
                return Ok(None);
            };
            out.rails = Some(scene);
        }
        (None, None) => {}
        _ => return Err("Roadside stamp rail scene and saved recipe differ".into()),
    }
    if let Some(previous) = &base.ocean {
        let n = base.layers.surface_resolution;
        if previous.resolution != n
            || previous.local_water.len() != base.layers.height.len()
            || previous.classification.len() != n * n
            || previous.topology.len() != n * n
        {
            return Err("Invalid preceding local-water revision".into());
        }
        // The Monument water hook preserves local water after changing terrain.
        let Some(ocean) = papyrus_terrain::ocean::reconstruct_with_water(
            &out.layers.height,
            out.layers.height_resolution,
            &out.layers.topology,
            n,
            Some(&previous.local_water),
            cancelled,
        )?
        else {
            return Ok(None);
        };
        out.layers.topology.clone_from(&ocean.topology);
        out.ocean = Some(ocean);
    }
    let mut changes = Changes::default();
    for (before, after) in base
        .layers
        .height
        .chunks(4096)
        .zip(out.layers.height.chunks(4096))
    {
        if cancelled() {
            return Ok(None);
        }
        changes.height_samples += before.iter().zip(after).filter(|(a, b)| a != b).count();
    }
    let cells = base.layers.surface_resolution.pow(2);
    for start in (0..cells).step_by(4096) {
        if cancelled() {
            return Ok(None);
        }
        for cell in start..(start + 4096).min(cells) {
            changes.alpha_cells += usize::from(base.alpha[cell] != out.alpha[cell]);
            changes.topology_cells +=
                usize::from(base.layers.topology[cell] != out.layers.topology[cell]);
            changes.splat_cells += usize::from((0..8).any(|c| {
                base.layers.splat[c * cells + cell] != out.layers.splat[c * cells + cell]
            }));
            changes.biome_cells += usize::from((0..5).any(|c| {
                base.layers.biome[c * cells + cell] != out.layers.biome[c * cells + cell]
            }));
        }
    }
    Ok((!cancelled()).then_some((out, changes)))
}
/// Ordered new stamps only; initial land stamps are not reapplied. All terrain
/// writes and adjusted roads remain private on errors, limits and cancellation.
pub fn compose<'a>(
    context: Context<'a>,
    provisional: &Provisional,
    maximum_road_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<StampedStage<'a>>, String> {
    let cache = TerrainCache::default();
    let Some(StampedStage { maps, roads, report }) = compose_inner(
        context, provisional, &cache, maximum_road_work, true, cancelled,
    )?
    else {
        return Ok(None);
    };
    drop(maps);
    Ok(Some(StampedStage {
        maps: cache.take()?,
        roads,
        report,
    }))
}

pub(crate) fn compose_validated<'a, 'b>(
    selection: ValidatedSelection<'a>,
    cache: &'b TerrainCache,
    maximum_road_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<StampedStage<'a, Ref<'b, Composed>>>, String> {
    let (context, provisional) = selection.into_parts();
    compose_inner(context, &provisional, cache, maximum_road_work, false, cancelled)
}

fn compose_inner<'a, 'b>(
    context: Context<'a>,
    provisional: &Provisional,
    cache: &'b TerrainCache,
    maximum_road_work: usize,
    verify_revision: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<StampedStage<'a, Ref<'b, Composed>>>, String> {
    let Context {
        project,
        initial,
        catalog,
        bindings,
        maps,
        plan,
        source,
    } = context;
    if cancelled() {
        return Ok(None);
    }
    if provisional.source_sha256() != source.sha256 || !project.performance.monuments {
        return Err("Roadside stamp source or content policy changed".into());
    }
    provisional.check_stamp_inputs(&catalog.sha256, &bindings.sha256, project.size)?;
    if verify_revision
        && plan.check_inputs(project, initial, catalog, bindings, maps, &cancelled)?.is_none()
    {
        return Ok(None);
    }
    if !provisional.selection.required_missing.is_empty() {
        if let Some(policy) = project.roads.as_ref().and_then(|roads| roads.staged.as_ref()) {
            let limits: Vec<_> = policy.roadside_preferences.iter().filter_map(|request| {
                let limit = request.max_displacement_m?;
                provisional.selection.required_missing.contains(&request.request)
                    .then(|| format!("{} ({limit} m)", request.prefab.rsplit('/').next().unwrap_or(&request.prefab)))
            }).collect();
            if !limits.is_empty() {
                return Err(format!("No valid roadside placement within the move limit for {}", limits.join(", ")));
            }
        }
        return Err("Required roadside requests remain unresolved".into());
    }
    let Some(roads) = provisional.straighten(plan, maximum_road_work, &cancelled)? else {
        return Ok(None);
    };
    let mut stamps = Vec::with_capacity(provisional.selection.selected.len());
    let mut placements = Vec::with_capacity(provisional.selection.selected.len());
    for selected in &provisional.selection.selected {
        if cancelled() {
            return Ok(None);
        }
        if initial.outcomes.iter().any(|o| o.id == selected.request) {
            return Err("Roadside stamp repeats an initial target identity".into());
        }
        let (path, prepared) = provisional
            .prepared_stamp(selected.group)
            .ok_or("Roadside group lost its prepared stamp")?;
        stamps.push(prepared.stamp_for_recipe(
            selected.position.map(f64::from),
            f64::from(selected.yaw),
            project.composition_engine.as_deref(),
        ));
        placements.push(Placement {
            request: selected.request,
            prefab: path.into(),
            position: selected.position,
            yaw: selected.yaw,
            road: selected.road,
            start_index: selected.start_index,
            end_index: selected.end_index,
        });
    }
    let reused = cache.0.borrow().as_ref().is_some_and(|cached| cached.matches(&placements));
    if !reused {
        // Keep only one full raster set even when the stamp list changes.
        drop(cache.0.take());
        let Some(out) =
            monument_composition::compose_streamed(&maps.layers, &maps.alpha, &cancelled, |apply| {
                for stamp in &stamps {
                    if !apply(stamp)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            })?
        else {
            return Ok(None);
        };
        let Some((out, changes)) = finish(maps, out, project.rails.as_ref(), &cancelled)? else {
            return Ok(None);
        };
        let stamps = placements
            .iter()
            .map(|p| (p.prefab.clone(), p.pose_bits()))
            .collect();
        cache.0.replace(Some(CachedTerrain { stamps, maps: out, changes }));
    }
    if cancelled() {
        return Ok(None);
    }
    if reused && std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some() {
        eprintln!("MAPPER_ROADSIDE_STAMPS_REUSED");
    }
    let cached = Ref::map(cache.0.borrow(), |value| value.as_ref().expect("verified staged terrain"));
    let report = Report {
        engine: VERSION,
        roadside_source_sha256: source.sha256.clone(),
        catalog_sha256: catalog.sha256.clone(),
        bindings_sha256: bindings.sha256.clone(),
        placements,
        missing: provisional.selection.missing.clone(),
        changes: cached.changes,
        ocean_reclassified: maps.ocean.is_some(),
        existing_local_water_preserved: true,
        accepted_world_published: false,
        environment_volumes_validated: false,
        runtime_physics_validated: false,
        final_access_validated: false,
    };
    Ok(Some(StampedStage {
        maps: Ref::map(cached, |cached| &cached.maps),
        roads,
        report,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> Composed {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut splat = vec![0; cells * 8];
        splat[..cells].fill(255);
        let mut biome = vec![0; cells * 5];
        biome[..cells].fill(255);
        let layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17000; hr * hr],
            biome,
            splat,
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        monument_composition::compose(&layers, &vec![255; cells], &[], || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn retained_terrain_requires_the_same_order_and_poses_and_fits_its_budget() {
        let placement = |request, prefab: &str, position| Placement {
            request,
            prefab: prefab.into(),
            position,
            yaw: 25.,
            road: 1,
            start_index: 1,
            end_index: 2,
        };
        let mut stamps = [
            placement(1, "assets/a.prefab", [0., 20., 0.]),
            placement(2, "assets/b.prefab", [5., 20., 0.]),
        ];
        let cached = CachedTerrain {
            stamps: stamps.iter().map(|p| (p.prefab.clone(), p.pose_bits())).collect(),
            maps: base(),
            changes: Changes::default(),
        };
        stamps[0].road = 4;
        assert!(cached.matches(&stamps));
        stamps.swap(0, 1);
        assert!(!cached.matches(&stamps));
        stamps.swap(0, 1);
        stamps[1].yaw += 1.;
        assert!(!cached.matches(&stamps));
        let cache = TerrainCache(RefCell::new(Some(cached)));
        cache.trim(usize::MAX);
        assert!(cache.0.borrow().is_some());
        cache.trim(0);
        assert!(cache.0.borrow().is_none());
    }

    #[test]
    fn roadside_handoff_preserves_prepared_rails_and_reservations_in_private_candidate() {
        let mut base = base();
        let (recipe, scene) =
            crate::retained_rail_scene::tests::fixture(base.layers.surface_resolution);
        let expected = crate::retained_rail_scene::Receipt::capture(
            &scene,
            &recipe,
            base.layers.surface_resolution,
            || false,
        )
        .unwrap()
        .unwrap();
        base.tunnels = Some(papyrus_terrain::tunnel_scene::Scene {
            engine:papyrus_terrain::tunnel_scene::VERSION.into(),source_sha256:"c".repeat(64),monument_sha256:"d".repeat(64),
            seed:7,layout_fingerprint:[2;32],report:serde_json::json!({"status":"generated"}),
            instances:vec![papyrus_terrain::tunnel_scene::Instance {instance:42,prefab:"assets/tunnel.prefab".into(),prefab_id:8,
                pose:papyrus_terrain::dungeon_base::Pose {position:[1.,-30.,2.],rotation:[0.,0.,0.,1.]},surface_position:None}],
        });
        base.rails = Some(scene);
        let candidate = || {
            monument_composition::compose(&base.layers, &base.alpha, &[], || false)
                .unwrap()
                .unwrap()
        };
        assert!(finish(&base, candidate(), None, &|| false).is_err());
        let (mut out, _) = finish(&base, candidate(), Some(&recipe), &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::retained_rail_scene::Receipt::capture(
                out.rails.as_ref().unwrap(),
                &recipe,
                base.layers.surface_resolution,
                || false
            )
            .unwrap()
            .unwrap(),
            expected
        );
        assert_eq!(out.tunnels.as_ref().unwrap().instances[0].instance,42);
        out.tunnels.as_mut().unwrap().instances[0].pose.position[0] = 99.;
        assert_eq!(base.tunnels.as_ref().unwrap().instances[0].pose.position[0],1.);
        out.rails.as_mut().unwrap().reservations.raster[0] = 1;
        assert_eq!(base.rails.as_ref().unwrap().reservations.raster[0], 0);
        assert!(
            finish(&base, candidate(), Some(&recipe), &|| true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn stage_changes_count_planar_cells_once_and_cancellation_keeps_input_unchanged() {
        let base = base();
        let mut out = monument_composition::compose(&base.layers, &base.alpha, &[], || false)
            .unwrap()
            .unwrap();
        let cells = base.alpha.len();
        out.layers.height[7] += 1;
        out.layers.splat[7] = 128;
        out.layers.splat[cells + 7] = 127;
        out.layers.biome[9] = 128;
        out.layers.biome[cells + 9] = 127;
        out.layers.topology[7] = 4;
        out.alpha[7] = 0;
        let (out, changes) = finish(&base, out, None, &|| false).unwrap().unwrap();
        assert_eq!(
            changes,
            Changes {
                height_samples: 1,
                splat_cells: 1,
                biome_cells: 1,
                topology_cells: 1,
                alpha_cells: 1
            }
        );
        assert_eq!(base.layers.height[7], 17000);
        assert_eq!(base.alpha[7], 255);
        assert_eq!(out.layers.splat[7], 128);
        assert_eq!(out.layers.splat[cells + 7], 127);
        let calls = std::cell::Cell::new(0);
        assert!(
            finish(&base, out, None, &|| {
                calls.set(calls.get() + 1);
                calls.get() > 5
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(base.layers.height[7], 17000);
        assert_eq!(base.alpha[7], 255);
    }
    #[test]
    fn ocean_reclassification_preserves_prior_local_water_and_clamp_accounting() {
        let mut base = base();
        let mut ocean = papyrus_terrain::ocean::reconstruct(
            &base.layers.height,
            base.layers.height_resolution,
            &base.layers.topology,
            base.layers.surface_resolution,
            || false,
        )
        .unwrap()
        .unwrap();
        ocean.local_water[5] = 17000;
        base.ocean = Some(ocean);
        base.clamped_height_samples = 3;
        let mut out = monument_composition::compose(&base.layers, &base.alpha, &[], || false)
            .unwrap()
            .unwrap();
        out.layers.height[0] = 15000;
        let (out, changes) = finish(&base, out, None, &|| false).unwrap().unwrap();
        assert_eq!(
            out.ocean.as_ref().unwrap().local_water,
            base.ocean.as_ref().unwrap().local_water
        );
        assert_eq!(
            out.ocean.as_ref().unwrap().classification[0],
            papyrus_terrain::ocean::OCEAN
        );
        assert_eq!(out.clamped_height_samples, 3);
        assert_eq!(changes.height_samples, 1);
        assert!(changes.topology_cells > 0);
        assert_eq!(base.layers.height[0], 17000);
        let mut invalid = monument_composition::compose(&base.layers, &base.alpha, &[], || false)
            .unwrap()
            .unwrap();
        invalid.clamped_height_samples = 1;
        assert!(finish(&base, invalid, None, &|| false).is_err());
    }
}
