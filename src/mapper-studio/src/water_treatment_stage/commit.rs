//! Carry an atomic pipe admission through final road materials and map export.
use super::{Input, Report, Settings, generate, registry};
use crate::{
    Project, infrastructure, monument_tag_source,
    prefab_assets::Catalog,
    prefab_identities,
    prefab_textures::Bindings,
    road_object_source, road_paint_source,
    road_paint_stage::{RoadsideChanges, RoadsideInput},
    roadside_stamps, water_treatment_source,
};
use papyrus_terrain::{
    footprints, native_layers::Layers, placement::Solution, prefab_records, roadside_decor,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: &str = "mapper-committed-water-treatment-1";
const MAX_SPANS: usize = 262144;
#[cfg(test)]
mod tests;

pub struct PreparedSources {
    source: water_treatment_source::Source,
    tags: std::sync::Arc<monument_tag_source::Source>,
    identities: std::sync::Arc<prefab_identities::Source>,
    road_source_sha256: String,
    catalog_sha256: String,
    known: u32,
    protected: u32,
}

pub struct Boundary<'a, 'path> {
    pub project: &'a Project,
    pub solution: &'a Solution,
    pub roadside: &'a [roadside_stamps::Placement],
    pub original: &'a Layers,
    pub alpha: &'a [u8],
    pub frame: RoadsideInput<'a, 'path>,
    pub paint: &'a road_paint_source::Profile,
}

#[derive(Serialize, Deserialize)]
struct Prefab {
    path: String,
    identity: u32,
    position: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}

#[derive(Serialize)]
pub struct Receipt {
    engine: &'static str,
    complete: bool,
    tag_source_sha256: String,
    identities_sha256: String,
    road_source_sha256: String,
    catalog_sha256: String,
    report: Report,
    terrain: Option<footprints::Report>,
    prefabs: Vec<Prefab>,
    reserved_spans: Vec<[usize; 2]>,
    work: usize,
    surface_resolution: usize,
    height_resolution: usize,
    height_spans: Vec<[usize; 2]>,
}

/// Private sparse deltas prevent the final repaint from erasing pipe materials.
pub struct Commit {
    pub receipt: Receipt,
    height: Vec<(usize, i16)>,
    topology: Vec<(usize, u32, u32)>,
    splat: Vec<(usize, u8)>,
}

impl PreparedSources {
    pub fn available(
        project: &Project,
        catalog: &Catalog,
        bindings: &Bindings,
        infrastructure: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let root = bindings.installation_root();
        let objects = project
            .objects
            .as_ref()
            .ok_or("Missing pipe object recipe")?;
        let road = road_object_source::Source::cached(
            root,
            &objects.road_source_sha256,
            infrastructure,
            &cancelled,
        )?;
        let flags = infrastructure
            .profile
            .topology_flags
            .as_ref()
            .ok_or("Missing pipe topology roles")?;
        let protected = infrastructure.profile.protected_topology as u32;
        Ok(Self {
            source: water_treatment_source::available(
                root, catalog, &road, flags, protected, &cancelled,
            )?,
            tags: monument_tag_source::available(root, catalog, &cancelled)?,
            identities: prefab_identities::Source::available(root, catalog, &cancelled)?,
            road_source_sha256: road.sha256.clone(),
            catalog_sha256: catalog.sha256.clone(),
            known: flags.values().fold(0, |mask, flag| mask | flag),
            protected,
        })
    }

    pub fn place(
        &self,
        input: Boundary<'_, '_>,
        settings: Settings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<(RoadsideChanges, Commit)>, String> {
        let Some(registry) = registry(
            input.project,
            input.solution,
            input.roadside,
            &self.tags,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let frame = input.frame.layers;
        let roads = input
            .frame
            .roads
            .iter()
            .map(|road| {
                let family = input
                    .paint
                    .policy
                    .families
                    .iter()
                    .find(|family| family.hierarchy == road.hierarchy)
                    .ok_or("Missing pipe road family width")?;
                Ok(roadside_decor::Road {
                    hierarchy: road.hierarchy,
                    path: road.path,
                    spline: true,
                    width: family.width,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        // No terrain copy is needed when the global eligibility gate is closed.
        let live = (!registry.matched_monuments().is_empty()).then(|| Layers {
            size: input.original.size,
            terrain_seed: input.original.terrain_seed,
            biome_seed: input.original.biome_seed,
            height_resolution: input.original.height_resolution,
            surface_resolution: input.original.surface_resolution,
            height: frame.height.to_vec(),
            topology: frame.topology.to_vec(),
            splat: frame.splat.to_vec(),
            biome: frame.biome.to_vec(),
            normalized_biome_cells: input.original.normalized_biome_cells,
            normalized_splat_cells: input.original.normalized_splat_cells,
        });
        let Some(output) = generate(
            Input {
                layers: live.as_ref().unwrap_or(input.original),
                alpha: input.alpha,
                roads: &roads,
                source: &self.source,
                registry: &registry,
                known_topology: self.known,
                protected_topology: self.protected,
                water: None,
                height_reservations: None,
            },
            settings,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let Some(records) = super::prefab_records(
            &output.report,
            |path| self.identities.resolve(path),
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let prefabs = output
            .report
            .programs
            .iter()
            .flat_map(|program| &program.admitted)
            .zip(records)
            .map(|(instance, record)| Prefab {
                path: record.path,
                identity: record.id,
                position: instance.transform.position,
                rotation: instance.transform.rotation,
                scale: instance.transform.scale,
            })
            .collect();
        let mut committed = Commit {
            receipt: Receipt {
                engine: VERSION,
                complete: true,
                tag_source_sha256: self.tags.sha256().into(),
                identities_sha256: self.identities.sha256.clone(),
                road_source_sha256: self.road_source_sha256.clone(),
                catalog_sha256: self.catalog_sha256.clone(),
                report: output.report,
                terrain: None,
                prefabs,
                reserved_spans: Vec::new(),
                work: 0,
                surface_resolution: frame.surface_resolution,
                height_resolution: frame.height_resolution,
                height_spans: Vec::new(),
            },
            height: Vec::new(),
            topology: Vec::new(),
            splat: Vec::new(),
        };
        let mut changes = RoadsideChanges::default();
        if let Some(mut terrain) = output.terrain {
            if committed.receipt.report.admitted_instances != 0 {
                let work = frame
                    .height
                    .len()
                    .checked_add(frame.topology.len() * 2)
                    .and_then(|work| work.checked_add(frame.splat.len()))
                    .ok_or("Pipe delta work overflow")?;
                if work > settings.maximum_terrain_work {
                    return Err("Pipe delta capture exceeds its terrain allowance".into());
                }
                committed.receipt.work = work;
                for (index, (&before, &after)) in
                    frame.height.iter().zip(&terrain.height).enumerate()
                {
                    if index % 4096 == 0 && cancelled() {
                        return Ok(None);
                    }
                    if before != after {
                        committed.height.push((index, after));
                        match committed.receipt.height_spans.last_mut() {
                            Some([_, end]) if *end == index => *end = index + 1,
                            _ => committed.receipt.height_spans.push([index, index + 1]),
                        }
                        if committed.receipt.height_spans.len() > MAX_SPANS {
                            return Err("Pipe height reservation span allowance exceeded".into());
                        }
                    }
                }
                if let Some(topology) = &terrain.topology {
                    for (index, (&before, &after)) in
                        frame.topology.iter().zip(topology).enumerate()
                    {
                        if index % 4096 == 0 && cancelled() {
                            return Ok(None);
                        }
                        if before != after {
                            committed.topology.push((
                                index,
                                after as u32 & !(before as u32),
                                before as u32 & !(after as u32),
                            ));
                        }
                    }
                }
                if let Some(splat) = &terrain.splat {
                    for (index, (&before, &after)) in frame.splat.iter().zip(splat).enumerate() {
                        if index % 4096 == 0 && cancelled() {
                            return Ok(None);
                        }
                        if before != after {
                            committed.splat.push((index, after));
                        }
                    }
                }
                let Some(spans) = owned_spans(
                    &terrain.report.occupancy,
                    terrain
                        .report
                        .blocking
                        .as_ref()
                        .map(|blocking| blocking.raster.as_slice()),
                    &cancelled,
                )?
                else {
                    return Ok(None);
                };
                committed.receipt.reserved_spans = spans;
                changes = RoadsideChanges {
                    height: (!committed.height.is_empty()).then_some(terrain.height),
                    topology: (!committed.topology.is_empty())
                        .then_some(terrain.topology)
                        .flatten(),
                    splat: (!committed.splat.is_empty())
                        .then_some(terrain.splat)
                        .flatten(),
                };
            }
            terrain.report.occupancy.clear();
            if let Some(blocking) = &mut terrain.report.blocking {
                blocking.raster.clear();
            }
            committed.receipt.terrain = Some(terrain.report);
        }
        Ok((!cancelled()).then_some((changes, committed)))
    }
}

fn owned_spans(
    occupancy: &[u8],
    blocking: Option<&[u8]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<[usize; 2]>>, String> {
    if blocking.is_some_and(|cells| cells.len() != occupancy.len()) {
        return Err("Pipe blocker and occupancy layouts differ".into());
    }
    let mut spans = Vec::new();
    let mut start = None;
    for (index, &occupied) in occupancy.iter().enumerate() {
        if index % 4096 == 0 && cancelled() {
            return Ok(None);
        }
        let blocker = blocking.map_or(0, |cells| cells[index]);
        if occupied > 2 || blocker > 1 {
            return Err("Invalid pipe reservation cell".into());
        }
        // Footprints uses 1 for accepted object reservations and 2 for the
        // pre-existing protected topology. Preserve only this stage's ownership.
        match (start, occupied == 1 || blocker == 1) {
            (None, true) => start = Some(index),
            (Some(first), false) => {
                spans.push([first, index]);
                start = None;
            }
            _ => {}
        }
        if spans.len() > MAX_SPANS {
            return Err("Pipe reservation span allowance exceeded".into());
        }
    }
    if let Some(first) = start {
        spans.push([first, occupancy.len()]);
    }
    if spans.len() > MAX_SPANS {
        return Err("Pipe reservation span allowance exceeded".into());
    }
    Ok(Some(spans))
}

impl Commit {
    pub fn work(&self) -> u64 {
        let report = &self.receipt.report;
        (self.receipt.work as u64)
            .saturating_add(report.sampling_work as u64)
            .saturating_add(report.fitting_samples as u64)
            .saturating_add(report.clearance_work as u64)
            .saturating_add(
                self.receipt
                    .terrain
                    .as_ref()
                    .map_or(0, |report| report.sample_visits) as u64,
            )
            .saturating_add((report.tagged_volumes as u64).saturating_mul(128))
    }

    pub fn preserve(
        &self,
        frame: RoadsideInput<'_, '_>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<RoadsideChanges>, String> {
        for &(index, value) in &self.height {
            if cancelled() {
                return Ok(None);
            }
            if frame.layers.height.get(index) != Some(&value) {
                return Err("Road correction changed an admitted pipe terrain effect".into());
            }
        }
        let mut changes = RoadsideChanges::default();
        if !self.topology.is_empty() {
            let mut topology = frame.layers.topology.to_vec();
            for &(index, added, removed) in &self.topology {
                if cancelled() {
                    return Ok(None);
                }
                let cell = topology
                    .get_mut(index)
                    .ok_or("Pipe topology delta is outside the final grid")?;
                *cell = ((*cell as u32 & !removed) | added) as i32;
            }
            changes.topology = Some(topology);
        }
        if !self.splat.is_empty() {
            let mut splat = frame.layers.splat.to_vec();
            for &(index, value) in &self.splat {
                if cancelled() {
                    return Ok(None);
                }
                *splat
                    .get_mut(index)
                    .ok_or("Pipe material delta is outside the final grid")? = value;
            }
            changes.splat = Some(splat);
        }
        Ok(Some(changes))
    }
}

#[derive(Deserialize)]
struct SavedReport {
    engine: String,
    source_sha256: String,
    eligible_monuments: usize,
    admitted_instances: usize,
    settings: Settings,
}
#[derive(Deserialize)]
struct SavedReceipt {
    engine: String,
    complete: bool,
    tag_source_sha256: String,
    identities_sha256: String,
    road_source_sha256: String,
    catalog_sha256: String,
    report: SavedReport,
    prefabs: Vec<Prefab>,
    reserved_spans: Vec<[usize; 2]>,
    surface_resolution: usize,
    height_resolution: usize,
    height_spans: Vec<[usize; 2]>,
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn read(value: &Value) -> Result<SavedReceipt, String> {
    if value["prefabs"]
        .as_array()
        .is_none_or(|rows| rows.len() > papyrus_terrain::content::MAX_PREFABS)
        || value["reserved_spans"]
            .as_array()
            .is_none_or(|rows| rows.len() > MAX_SPANS)
        || value["height_spans"]
            .as_array()
            .is_none_or(|rows| rows.len() > MAX_SPANS)
    {
        return Err("Invalid committed pipe roster or reservation allowance".into());
    }
    let saved = SavedReceipt::deserialize(value).map_err(|error| error.to_string())?;
    saved.report.settings.validate()?;
    if saved.engine != VERSION
        || !saved.complete
        || saved.report.engine != super::VERSION
        || ![
            saved.report.source_sha256.as_str(),
            &saved.tag_source_sha256,
            &saved.identities_sha256,
            &saved.road_source_sha256,
            &saved.catalog_sha256,
        ]
        .into_iter()
        .all(hash)
        || saved.report.admitted_instances != saved.prefabs.len()
        || (saved.report.eligible_monuments == 0
            && (!saved.prefabs.is_empty() || !saved.reserved_spans.is_empty()))
        || (saved.prefabs.is_empty() && !saved.reserved_spans.is_empty())
        || (saved.prefabs.is_empty() && !saved.height_spans.is_empty())
        || !(1..=4096).contains(&saved.surface_resolution)
        || !(2..=4097).contains(&saved.height_resolution)
    {
        return Err("Committed pipe receipt differs from its completed stage".into());
    }
    validate_spans(
        &saved.reserved_spans,
        saved.surface_resolution * saved.surface_resolution,
    )?;
    validate_spans(
        &saved.height_spans,
        saved.height_resolution * saved.height_resolution,
    )?;
    for prefab in &saved.prefabs {
        if prefab.identity == 0
            || prefab.path.len() > 512
            || !prefab.path.starts_with("assets/")
            || !prefab.path.ends_with(".prefab")
            || prefab
                .position
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 10000.)
            || prefab
                .scale
                .iter()
                .any(|value| !value.is_finite() || *value == 0. || value.abs() > 100.)
        {
            return Err("Invalid committed pipe prefab".into());
        }
        prefab_records::euler(prefab.rotation)?;
    }
    Ok(saved)
}

/// Older saved worlds can omit this newly introduced procedural stage.
pub fn validate_committed(value: &Value, project: &Project) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    let saved = read(value)?;
    let objects = project
        .objects
        .as_ref()
        .ok_or("Unbound committed pipe stage")?;
    let (height_resolution, surface_resolution) =
        papyrus_terrain::native_layers::dimensions(project.size)?;
    if saved.surface_resolution != surface_resolution
        || saved.height_resolution != height_resolution
        || saved.road_source_sha256 != objects.road_source_sha256
        || project
            .monument_assets
            .as_ref()
            .is_none_or(|assets| assets.catalog_sha256 != saved.catalog_sha256)
        || saved.report.settings.seed != project.seeds[5].value
        || saved.report.settings.realm_mask != objects.realm_mask
        || saved.report.settings.maximum_terrain_work > objects.maximum_terrain_work
        || saved.report.settings.maximum_sampling_attempts > objects.maximum_sampling_attempts
    {
        return Err("Committed pipe sources or seeds differ from the recipe".into());
    }
    Ok(())
}

fn validate_spans(spans: &[[usize; 2]], cells: usize) -> Result<(), String> {
    let mut previous = 0;
    for &[start, end] in spans {
        if start < previous || start >= end || end > cells {
            return Err("Invalid committed pipe reservation span".into());
        }
        previous = end;
    }
    Ok(())
}

/// Extend the caller's current road-height guard with committed pipe earthwork.
pub fn reserve_heights(
    value: &Value,
    destination: &mut papyrus_terrain::height_reservations::HeightReservations,
    grid: papyrus_terrain::path_terrain::Grid<'_>,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<usize>, String> {
    if cancelled() {
        return Ok(None);
    }
    if value.is_null() {
        return Ok(Some(0));
    }
    let saved = read(value)?;
    if saved.height_resolution != grid.resolution || !destination.layout_matches(grid) {
        return Err("Committed pipe heights belong to another grid".into());
    }
    let work = saved
        .height_spans
        .iter()
        .try_fold(0usize, |work, &[start, end]| work.checked_add(end - start))
        .ok_or("Pipe height reservation work overflow")?;
    if work > maximum_work {
        return Err("Pipe height reservation work exhausted".into());
    }
    for &[start, end] in &saved.height_spans {
        for index in start..end {
            if index % 4096 == 0 && cancelled() {
                return Ok(None);
            }
            destination.reserve(index)?;
        }
    }
    Ok(Some(work))
}

pub fn receipt_records(
    value: &Value,
    resolve: impl Fn(&str) -> Option<u32>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<prefab_records::Record>>, String> {
    if cancelled() {
        return Ok(None);
    }
    if value.is_null() {
        return Ok(Some(Vec::new()));
    }
    let saved = read(value)?;
    let mut instances = Vec::with_capacity(saved.prefabs.len());
    for prefab in &saved.prefabs {
        if cancelled() {
            return Ok(None);
        }
        if prefab.path.len() > 512
            || !prefab.path.starts_with("assets/")
            || !prefab.path.ends_with(".prefab")
            || resolve(&prefab.path) != Some(prefab.identity)
            || prefab.identity == 0
        {
            return Err("Committed pipe identity differs from the selected installation".into());
        }
        instances.push(prefab_records::Instance {
            path: &prefab.path,
            category: "Monument",
            position: prefab.position,
            rotation: prefab.rotation,
            scale: prefab.scale,
        });
    }
    prefab_records::assemble(&instances, resolve, cancelled)
}

/// The destination is the caller's private, unpublished object reservation grid.
pub fn reserve_committed(
    value: &Value,
    destination: &mut [u8],
    cancelled: impl Fn() -> bool,
) -> Result<Option<bool>, String> {
    if cancelled() {
        return Ok(None);
    }
    if value.is_null() {
        return Ok(Some(false));
    }
    let saved = read(value)?;
    if destination.len() != saved.surface_resolution * saved.surface_resolution {
        return Err("Committed pipe reservations belong to another grid".into());
    }
    for &[start, end] in &saved.reserved_spans {
        for chunk in destination[start..end].chunks_mut(4096) {
            if cancelled() {
                return Ok(None);
            }
            chunk.fill(1);
        }
    }
    Ok(Some(true))
}
