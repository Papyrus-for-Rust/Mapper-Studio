//! Explicit diagnostic rail-bed checkpoint. Never publishes a playable .map or
//! changes a project's pinned generation algorithms/immutable terrain cache.
use crate::{Project, rail_preview::Terrain, rail_source::Source};
use papyrus_terrain::{path_geometry::Path, rail_bed};
use serde_json::{Value, json};
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub maximum_work: usize,
    pub maximum_cut_fill: f32,
    #[serde(default)]
    pub grade_fit: Option<papyrus_terrain::rail_grade_fit::Settings>,
    #[serde(default)]
    pub corridor: Option<papyrus_terrain::rail_corridor::Settings>,
    #[serde(default)]
    pub track_network: Option<papyrus_terrain::rail_track_graph::Settings>,
    #[serde(default)]
    pub meshes: Option<crate::rail_mesh_stage::Settings>,
    #[serde(default)]
    pub gameplay_height_work: Option<usize>,
    #[serde(default)]
    pub branching: Option<papyrus_terrain::rail_branching::Settings>,
    #[serde(default)]
    pub siding: Option<papyrus_terrain::rail_siding::Settings>,
    #[serde(default)]
    pub reservation_work: Option<usize>,
}
impl Query {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .reservation_work
            .is_some_and(|v| !(1..=128000000).contains(&v))
        {
            return Err("Invalid rail reservation work allowance".into());
        }
        if let Some(siding) = &self.siding {
            siding.validate()?;
            if self.grade_fit.is_none() || self.track_network.is_some() {
                return Err("Rail sidings require constrained grade fitting; authored track-graph integration is pending".into());
            }
        }
        if let Some(branching) = &self.branching {
            branching.validate()?;
            if self.grade_fit.is_none() || self.track_network.is_some() {
                return Err("Rail branches require constrained grade fitting; authored track-graph integration is pending".into());
            }
        }
        if self
            .gameplay_height_work
            .is_some_and(|v| !(1..=256_000_000).contains(&v))
            || self.gameplay_height_work.is_some() && self.meshes.is_none()
        {
            return Err(
                "Rail gameplay height requires mesh generation and a bounded work allowance".into(),
            );
        }
        self.settings().validate()?;
        if let Some(s) = self.corridor {
            s.validate()?;
        }
        if let Some(s) = self.track_network {
            s.validate()?;
        }
        if let Some(s) = self.meshes {
            s.validate()?;
        }
        Ok(())
    }
    fn settings(&self) -> rail_bed::Settings {
        rail_bed::Settings {
            maximum_work: self.maximum_work,
            maximum_cut_fill: self.maximum_cut_fill,
            grade_fit: self.grade_fit,
        }
    }
}
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path as FilePath,
};
pub struct Candidate {
    pub reservations: Option<papyrus_terrain::rail_reservations::Output>,
    pub gameplay_height: Option<papyrus_terrain::gameplay_height::Output>,
    pub output: rail_bed::Output,
    pub alpha: Vec<u8>,
    pub ocean: papyrus_terrain::ocean::Ocean,
    pub report: Value,
    pub mesh_geometry: Option<crate::rail_mesh_stage::Output>,
}
struct ReservationRun<'a> {
    policy: &'a papyrus_terrain::rail_reservations::Policy,
    maximum_work: usize,
    output: Option<papyrus_terrain::rail_reservations::Output>,
    steps: Vec<Value>,
}
impl ReservationRun<'_> {
    fn apply(
        &mut self,
        phase: &str,
        layers: &papyrus_terrain::native_layers::Layers,
        paths: &[Path],
        range: std::ops::Range<usize>,
        initial: &[u8],
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        use papyrus_terrain::rail_reservations::{self, Track};
        let first = range.start;
        let paths = paths.get(range).ok_or("Invalid reservation path slice")?;
        let tracks = paths
            .iter()
            .enumerate()
            .map(|(i, path)| Track {
                id: (first + i + 1) as u64,
                path,
            })
            .collect::<Vec<_>>();
        let work = self.output.as_ref().map_or(0, |v| v.report.work);
        let remaining = self.maximum_work.saturating_sub(work);
        if remaining == 0 {
            return Err("Rail reservation work allowance reached".into());
        }
        let initial = self
            .output
            .as_ref()
            .map_or(initial, |o| o.raster.as_slice());
        let Some(mut next) = rail_reservations::generate(
            layers.size as f32,
            layers.surface_resolution,
            initial,
            &tracks,
            self.policy,
            remaining,
            cancelled,
        )?
        else {
            return Ok(false);
        };
        self.steps
            .push(json!({"phase":phase,"changes":next.report}));
        if let Some(previous) = self.output.take() {
            next.report.tracks += previous.report.tracks;
            next.report.sweeps += previous.report.sweeps;
            next.report.initial_blocked = previous.report.initial_blocked;
            next.report.newly_blocked = next.report.final_blocked - next.report.initial_blocked;
            next.report.cell_visits += previous.report.cell_visits;
            next.report.work += previous.report.work;
            let mut samples = previous.samples;
            samples.extend(next.samples);
            next.samples = samples;
        }
        self.output = Some(next);
        Ok(true)
    }
}
fn controls(value: &Value) -> Result<Vec<[f32; 3]>, String> {
    let points: Vec<[f32; 3]> = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if !(2..=65536).contains(&points.len()) {
        return Err("Invalid bed path control count".into());
    }
    Ok(points)
}
fn vector(value: &Value) -> Result<[f32; 3], String> {
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}
fn gap(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f32>()
        .sqrt()
}
fn angle(a: [f32; 3], b: [f32; 3]) -> f32 {
    let la = gap(a, [0.; 3]);
    let lb = gap(b, [0.; 3]);
    if la < 0.00001 || lb < 0.00001 {
        return 180.;
    }
    (a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>() / (la * lb))
        .clamp(-1., 1.)
        .acos()
        .to_degrees()
}
#[allow(clippy::too_many_arguments)]
pub fn build(
    terrain: &Terrain,
    root: &FilePath,
    source: &Source,
    infrastructure: &crate::infrastructure::Source,
    plan: &Value,
    query: Query,
    authored: Option<&crate::authored_tracks::Source>,
    meshes: Option<&crate::rail_mesh_source::Source>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Candidate>, String> {
    build_inner(
        terrain,
        root,
        source,
        infrastructure,
        plan,
        query,
        authored,
        meshes,
        None,
        false,
        cancelled,
    )
}
/// Prepare rail earthwork while deferring texture and topology to the later
/// source-ordered stages. This does not upgrade legacy saved recipes.
#[allow(clippy::too_many_arguments)]
pub fn build_deferred_surfaces(
    terrain: &Terrain,
    root: &FilePath,
    source: &Source,
    infrastructure: &crate::infrastructure::Source,
    plan: &Value,
    query: Query,
    authored: Option<&crate::authored_tracks::Source>,
    meshes: Option<&crate::rail_mesh_source::Source>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Candidate>, String> {
    build_inner(
        terrain,
        root,
        source,
        infrastructure,
        plan,
        query,
        authored,
        meshes,
        None,
        true,
        cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_from_ring(
    terrain: &Terrain,
    grid: &papyrus_terrain::rail_costmap::Grid,
    source: &Source,
    infrastructure: &crate::infrastructure::Source,
    plan: &Value,
    query: Query,
    meshes: &crate::rail_mesh_source::Source,
    defer_surfaces: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Candidate>, String> {
    build_inner(
        terrain,
        FilePath::new(""),
        source,
        infrastructure,
        plan,
        query,
        None,
        Some(meshes),
        Some(grid),
        defer_surfaces,
        cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_inner(
    terrain: &Terrain,
    root: &FilePath,
    source: &Source,
    infrastructure: &crate::infrastructure::Source,
    plan: &Value,
    query: Query,
    authored: Option<&crate::authored_tracks::Source>,
    meshes: Option<&crate::rail_mesh_source::Source>,
    ring_grid: Option<&papyrus_terrain::rail_costmap::Grid>,
    defer_surfaces: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Candidate>, String> {
    let mut timing = crate::composition::StageTimer::start("rail_bed");
    let settings = query.settings();
    query.validate()?;
    if defer_surfaces && (query.meshes.is_some() || query.gameplay_height_work.is_some()) {
        return Err(
            "Deferred rail surfaces require final meshes and gameplay height to remain deferred"
                .into(),
        );
    }
    if query.track_network.is_some() && authored.is_none() {
        return Err("World track graph requires an explicit --tracks snapshot".into());
    }
    if query.meshes.is_some() {
        if meshes.is_none() {
            return Err("Rail bed meshes require an explicit --meshes snapshot".into());
        }
        if source.profile.mesh.is_none() {
            return Err(
                "This rail source snapshot has no mesh-stage adapter; prepare a new snapshot"
                    .into(),
            );
        }
        if meshes.unwrap().parent_digest() != source.sha256 {
            return Err("Rail mesh templates differ from bed source".into());
        }
    }
    let policy = source
        .profile
        .bed
        .as_ref()
        .ok_or("This rail source snapshot has no bed adapter; prepare a new snapshot")?;
    if plan["source_sha256"] != source.sha256
        || plan["terrain_plan"]["manifest_sha256"] != terrain.manifest_digest()
    {
        return Err("Rail bed plan differs from verified source/terrain".into());
    }
    let approaches = &plan["terrain_plan"]["approaches"];
    if approaches["result"]["status"] != "proposed"
        || approaches["connector_source"]["infrastructure_sha256"] != source.infrastructure_digest()
        || approaches["result"]["unresolved"]
            .as_array()
            .is_none_or(|v| !v.is_empty())
    {
        return Err(
            "Rail bed requires an explicit primary network and resolved approach proposals".into(),
        );
    }
    let list = approaches["result"]["approaches"]
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or("Invalid approach set")?;
    let edits = &terrain.project().path_edits;
    let mut paths = Vec::new();
    let points = controls(&approaches["network_curve"]["points"])?;
    paths.push(
        Path::from_controls(&points, 32000000, &cancelled)
            .map_err(|e| format!("Invalid bed ring: {e:?}"))?
            .ok_or("Bed preparation cancelled")?,
    );
    let expects_ring = terrain.project().rails.as_ref()
        .map(|recipe| recipe.settings().map(|settings| settings.main_ring))
        .transpose()?.unwrap_or(true);
    if paths[0].is_circular() != expects_ring {
        return Err("Primary rail line closure differs from the recipe".into());
    }
    for entry in list {
        let points = controls(&entry["curve"]["points"])?;
        let Some(path) = Path::from_controls(&points, 32_000_000, &cancelled)
            .map_err(|e| format!("Invalid approach bed: {e:?}"))?
        else { return Ok(None); };
        let Some(path) = crate::path_edit::edit_rail(paths.len(), path, edits, &cancelled)?
        else { return Ok(None); };
        paths.push(path);
    }
    let (layers, alpha) = terrain.bed_layers(root, &cancelled)?;
    let mut reservations = if let Some(maximum_work) = query.reservation_work {
        Some(ReservationRun{policy:source.profile.reservations.as_ref().ok_or("This rail source has no placement reservation adapter; prepare a matching snapshot")?,maximum_work,output:None,steps:Vec::new()})
    } else {
        None
    };
    if let Some(run) = &mut reservations
        && !run.apply(
            "ring_layout",
            &layers,
            &paths,
            0..paths.len(),
            terrain.placement_subset().0,
            &cancelled,
        )?
    {
        return Ok(None);
    }
    let branch_first_track = paths.len();
    let mut branch_joins = Vec::new();
    let mut branch_report = None;
    if let Some(branch_settings) = query.branching {
        use papyrus_terrain::rail_branching::{self, Outcome, Track};
        let policy = source
            .profile
            .branching
            .as_ref()
            .ok_or("This rail source has no branching adapter; prepare a matching snapshot")?;
        let stage = source
            .stages
            .iter()
            .find(|s| s.kind == crate::rail_source::Kind::GenerateRailBranching)
            .ok_or("Missing source branching stage")?;
        if let Some(reason) = source.skip(stage, layers.size, false, true) {
            return Err(format!("Rail branching stage unavailable: {reason}"));
        }
        let rebuilt_grid;
        let grid = match ring_grid {
            Some(grid) => grid,
            None => {
                let Some(grid) = terrain.grid(&source.profile.costmap, &cancelled)? else {
                    return Ok(None);
                };
                rebuilt_grid = grid;
                &rebuilt_grid
            }
        };
        let sockets = if approaches["connector_source"]["generated"].is_object() {
            let sockets: crate::rail_preview::Sockets =
                serde_json::from_value(approaches["connector_source"].clone())
                    .map_err(|e| format!("Invalid generated rail connector handoff: {e}"))?;
            if sockets.sockets.len() > 256 {
                return Err("Generated rail connector handoff exceeds its allowance".into());
            }
            sockets
        } else {
            terrain.sockets(
                root,
                terrain.project(),
                infrastructure,
                source.profile.rail_type,
                &cancelled,
            )?
        };
        let connectors = sockets
            .sockets
            .iter()
            .map(|s| [s.position[0], s.position[2]])
            .collect::<Vec<_>>();
        let placement = reservations
            .as_ref()
            .and_then(|r| r.output.as_ref())
            .map_or_else(|| terrain.placement_subset().0, |r| r.raster.as_slice());
        let n = layers.surface_resolution;
        let blocked = |v: [f32; 3]| {
            let index = |w: f32| {
                ((w / layers.size as f32 + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize
            };
            placement[index(v[2]) * n + index(v[0])] != 0
        };
        let parents = paths
            .iter()
            .enumerate()
            .map(|(i, path)| Track {
                id: i as u64 + 1,
                path,
            })
            .collect::<Vec<_>>();
        let outcome = rail_branching::generate(
            grid,
            &parents,
            &connectors,
            policy,
            branch_settings,
            blocked,
            &cancelled,
        )?;
        branch_report = Some(
            json!({"engine":rail_branching::VERSION,"policy":policy,"settings":branch_settings,"result":outcome,
            "placement_scope":"implemented decoration subset","official_seed_parity":false,"runtime_connections_validated":false}),
        );
        if sockets.generated.is_some() {
            branch_report.as_mut().unwrap()["input_connector_count"] = json!(connectors.len());
        }
        match outcome {
            Outcome::Cancelled => return Ok(None),
            Outcome::BudgetExceeded { .. } => {
                return Err(
                    "Rail branching work allowance reached; no bed checkpoint produced".into(),
                );
            }
            Outcome::Proposed { branches, .. } => {
                for branch in branches {
                    let id = paths.len() as u64 + 1;
                    branch_joins.push((
                        id,
                        branch.parent,
                        branch.start_control,
                        branch.end_control,
                    ));
                    let Some(path) = crate::path_edit::edit_rail(paths.len(), branch.path, edits, &cancelled)?
                    else { return Ok(None); };
                    paths.push(path);
                }
            }
        }
    }
    if let Some(run) = &mut reservations
        && paths.len() > branch_first_track
        && !run.apply(
            "branches",
            &layers,
            &paths,
            branch_first_track..paths.len(),
            terrain.placement_subset().0,
            &cancelled,
        )?
    {
        return Ok(None);
    }
    let siding_first_track = paths.len() + 1;
    let mut siding_joins = Vec::new();
    let mut siding_report = None;
    if let Some(siding_settings) = query.siding {
        use papyrus_terrain::rail_siding::{self, Outcome, Track};
        let policy = source
            .profile
            .siding
            .as_ref()
            .ok_or("This rail source has no siding adapter; prepare a matching snapshot")?;
        let stage = source
            .stages
            .iter()
            .find(|s| s.kind == crate::rail_source::Kind::GenerateRailSiding)
            .ok_or("Missing source siding stage")?;
        if let Some(reason) = source.skip(stage, layers.size, false, true) {
            return Err(format!("Rail siding stage unavailable: {reason}"));
        }
        let parents = paths
            .iter()
            .enumerate()
            .map(|(i, path)| Track {
                id: i as u64 + 1,
                path,
            })
            .collect::<Vec<_>>();
        let outcome = rail_siding::generate(
            layers.size as f32,
            &parents,
            policy,
            siding_settings,
            &cancelled,
        )?;
        siding_report = Some(
            json!({"engine":rail_siding::VERSION,"policy":policy,"settings":siding_settings,"result":outcome,
            "placement_reservations_applied":false,"official_seed_parity":false,"runtime_connections_validated":false}),
        );
        match outcome {
            Outcome::Cancelled => return Ok(None),
            Outcome::BudgetExceeded { .. } => {
                return Err(
                    "Rail siding work allowance reached; no bed checkpoint produced".into(),
                );
            }
            Outcome::Proposed { sidings, .. } => {
                for siding in sidings {
                    if paths.len() >= 257 {
                        return Err("Rail checkpoint track allowance reached".into());
                    }
                    let id = paths.len() as u64 + 1;
                    siding_joins.push((
                        id,
                        siding.parent,
                        siding.start_control,
                        siding.end_control,
                    ));
                    let Some(path) = crate::path_edit::edit_rail(paths.len(), siding.path, edits, &cancelled)?
                    else { return Ok(None); };
                    paths.push(path);
                }
            }
        }
    }
    if let Some(run) = &mut reservations
        && paths.len() + 1 > siding_first_track
        && !run.apply(
            "sidings",
            &layers,
            &paths,
            siding_first_track - 1..paths.len(),
            terrain.placement_subset().0,
            &cancelled,
        )?
    {
        return Ok(None);
    }
    if edits.iter().any(|e| e.route.kind == crate::path_edit::Kind::Rail && e.route.index >= paths.len()) {
        return Err("An edited rail spline is missing from the new layout".into());
    }
    let inputs = paths
        .iter()
        .enumerate()
        .map(|(i, path)| rail_bed::Track {
            id: i as u64 + 1,
            path,
            preserve_start: i > 0 && i <= list.len(),
        })
        .collect::<Vec<_>>();
    let mut joins = Vec::new();
    if settings.grade_fit.is_some() {
        for &(id, parent, start, end) in branch_joins.iter().chain(&siding_joins) {
            joins.push(papyrus_terrain::rail_bed_fit::Join {
                a_track: id,
                a_control: 0,
                b_track: parent,
                b_control: start,
            });
            joins.push(papyrus_terrain::rail_bed_fit::Join {
                a_track: id,
                a_control: paths[id as usize - 1].points().len() - 1,
                b_track: parent,
                b_control: end,
            });
        }
        for (i, entry) in list.iter().enumerate() {
            let index = entry["join_index"]
                .as_u64()
                .filter(|&v| v < paths[0].points().len() as u64)
                .ok_or("Invalid constrained rail join control")? as usize;
            joins.push(papyrus_terrain::rail_bed_fit::Join {
                a_track: i as u64 + 2,
                a_control: paths[i + 1].points().len() - 1,
                b_track: 1,
                b_control: index,
            });
        }
    }
    let build_bed = if defer_surfaces {
        rail_bed::build_terrain_with_joins
    } else {
        rail_bed::build_with_joins
    };
    crate::composition::record_timing(&mut timing, "routes_and_reservations");
    let Some(mut output) = build_bed(
        &layers,
        &alpha,
        terrain.bed_water(),
        &inputs,
        &joins,
        policy,
        source.profile.bed_shape()?,
        settings,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    crate::composition::record_timing(&mut timing, "earthwork");
    let Some(ocean) = papyrus_terrain::ocean::reconstruct(
        &output.layers.height,
        output.layers.height_resolution,
        &output.layers.topology,
        output.layers.surface_resolution,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let preserve = source.profile.bed_shape()?.preserve_topology;
    let mut ocean_changes = 0;
    let mut final_topology_changes = 0;
    for (i, (&before, &after)) in output
        .layers
        .topology
        .iter()
        .zip(&ocean.topology)
        .enumerate()
    {
        if i % layers.surface_resolution == 0 && cancelled() {
            return Ok(None);
        }
        if layers.topology[i] as u32 & preserve != 0 && after != layers.topology[i] {
            return Err("Rail bed water reconciliation would change protected topology".into());
        }
        ocean_changes += usize::from(before != after);
        final_topology_changes += usize::from(layers.topology[i] != after);
    }
    output.layers.topology = ocean.topology.clone();
    output.report.changed_topology_cells = final_topology_changes;
    crate::composition::record_timing(&mut timing, "water_reconciliation");
    let pose_policy = source
        .profile
        .approaches
        .as_ref()
        .ok_or("Bed source lacks endpoint pose tolerances")?;
    let primary = &output.tracks[0].path;
    let mut poses = Vec::new();
    let mut grades = Vec::new();
    for track in &output.tracks {
        let Some(audit) = papyrus_terrain::rail_grade::analyze(
            &track.path,
            papyrus_terrain::rail_grade::Settings {
                absolute_tolerance: 0.0001,
                maximum_work: 1000000,
                maximum_depth: 24,
            },
            &cancelled,
        )
        .map_err(|e| format!("Rail curve grade analysis failed: {e:?}"))?
        else {
            return Ok(None);
        };
        grades.push(json!({"track_id":track.id,"audit":audit}));
    }
    for (i, entry) in list.iter().enumerate() {
        let track = &output.tracks[i + 1].path;
        let end = track.points().len() - 1;
        let join = entry["join_index"]
            .as_u64()
            .filter(|&v| v < (primary.points().len() as u64))
            .ok_or("Invalid bed join index")? as usize;
        let direction = entry["join_direction"]
            .as_i64()
            .filter(|v| [-1, 1].contains(v))
            .ok_or("Invalid bed join direction")? as f32;
        let start_gap = gap(track.points()[0], vector(&entry["socket"]["position"])?);
        let start_angle = angle(track.tangents()[0], vector(&entry["socket"]["forward"])?);
        let end_gap = gap(track.points()[end], primary.points()[join]);
        let end_angle = angle(
            track.tangents()[end],
            primary.tangents()[join].map(|v| v * direction),
        );
        poses.push(json!({"socket":entry["socket"],"start_gap":start_gap,"start_angle":start_angle,"join_gap":end_gap,"join_angle":end_angle,
            "path_pose_matches":start_gap<pose_policy.position_tolerance && end_gap<pose_policy.position_tolerance
                && start_angle<pose_policy.tangent_tolerance_degrees && end_angle<pose_policy.tangent_tolerance_degrees}));
    }
    if settings.grade_fit.is_some() && poses.iter().any(|p| p["path_pose_matches"] != true) {
        return Err("Constrained rail bed failed a monument socket or merge pose check".into());
    }
    let engine = if defer_surfaces && settings.grade_fit.is_some() {
        "papyrus-rail-bed-terrain-constrained-1"
    } else if defer_surfaces {
        rail_bed::TERRAIN_VERSION
    } else if settings.grade_fit.is_some() {
        "papyrus-rail-bed-constrained-1"
    } else {
        rail_bed::VERSION
    };
    let mut report = json!({"engine":engine,"settings":settings,"policy":policy,"shape":source.profile.bed_shape()?,
        "source_sha256":source.sha256,"base_manifest_sha256":terrain.manifest_digest(),"changes":output.report,
        "paths":output.tracks,"terrain_applied_to_private_candidate":true,"playable":false,"generation_ready":false,
        "alpha_preserved":true,"biome_preserved":true,"ocean_reconciled":true,
        "ocean_topology_changes":ocean_changes,"post_bed_endpoint_poses":poses,
        "continuous_grade_audits":grades,
        "raster":"bounded Hermite sampling / world vertices and texel centers; not Unity swept-quad parity",
        "full_corridor_validated":false,"runtime_track_connections_validated":false});
    if approaches["connector_source"]["generated"].is_object() {
        report["connector_source"] = approaches["connector_source"].clone();
    }
    if let Some(run) = &reservations {
        report["placement_reservations"] = json!({"policy":run.policy,"phases":run.steps,"changes":run.output.as_ref().unwrap().report,
            "path_stage":"before terrain fitting","initial_scope":"implemented input placement subset","full_runtime_placement_map_reconstructed":false,
            "self_clearance_uses_input_subset":true});
    }
    for (name, extra_report, extra_joins) in [
        ("branching", branch_report, &branch_joins),
        ("sidings", siding_report, &siding_joins),
    ] {
        if let Some(extra_report) = extra_report {
            let mut poses = Vec::new();
            for &(id, parent, start, end) in extra_joins {
                let branch = &output.tracks[id as usize - 1].path;
                let base = &output.tracks[parent as usize - 1].path;
                let first_gap = gap(branch.points()[0], base.points()[start]);
                let last_gap = gap(*branch.points().last().unwrap(), base.points()[end]);
                if first_gap >= pose_policy.position_tolerance
                    || last_gap >= pose_policy.position_tolerance
                {
                    return Err(format!("Constrained rail {name} lost a parent join"));
                }
                poses.push(json!({"track_id":id,"parent":parent,"start_control":start,"end_control":end,"first_gap":first_gap,"last_gap":last_gap,
                "start_angle":angle(branch.tangents()[0],base.tangents()[start]),"end_angle":angle(*branch.tangents().last().unwrap(),base.tangents()[end])}));
            }
            report[name] = extra_report;
            report[name]["fitted_joins"] = json!(poses);
        }
    }
    crate::composition::record_timing(&mut timing, "grade_and_join_checks");
    if let Some(settings) = query.corridor {
        let shape = source.profile.bed_shape()?;
        let policy = papyrus_terrain::rail_corridor::Policy {
            half_width: f64::from(shape.width) * 0.5,
            terrain_offset: f64::from(shape.terrain_offset),
            blocked_topology: source.profile.costmap.blocked_topology
                | source.profile.costmap.clearance_topology,
            protected_topology: shape.preserve_topology,
            water_topology: source.profile.costmap.water_topology,
        };
        let (placement, origin) = terrain.placement_subset();
        let mut auditor =
            papyrus_terrain::rail_corridor::Auditor::new(&output.layers, &alpha, Some(placement))?;
        let mut entries = Vec::new();
        let mut work = 0;
        for track in &output.tracks {
            let remaining = settings.maximum_work.saturating_sub(work);
            if remaining == 0 {
                return Err("Rail network corridor work allowance reached".into());
            }
            let Some(audit) = auditor.analyze(
                &track.path,
                policy,
                papyrus_terrain::rail_corridor::Settings {
                    maximum_work: remaining,
                    ..settings
                },
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            work += audit.work;
            entries.push(json!({"track_id":track.id,"audit":audit}));
        }
        let engine = if settings.ground_refinement.is_some() {
            papyrus_terrain::rail_corridor::REFINED_VERSION
        } else {
            papyrus_terrain::rail_corridor::VERSION
        };
        report["corridor"] = json!({"engine":engine,"settings":settings,"policy":policy,
            "placement_subset":origin,"runtime_placement_map_reconstructed":false,
            "scope":"Conservative source bed-width raster coverage and bilinear ground bounds; not rail meshes or rolling-stock clearance",
            "tracks":entries,"work":work,"full_corridor_validated":false});
    }
    crate::composition::record_timing(&mut timing, "corridor_audit");
    if let Some(settings) = query.track_network {
        let poses =
            terrain.monuments(root, infrastructure, source.profile.rail_type, &cancelled)?;
        let ports = list
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                Ok(crate::authored_tracks::Port {
                    path_id: i as u64 + 2,
                    monument: entry["socket"]["monument"]
                        .as_u64()
                        .ok_or("Missing approach monument binding")?,
                    connector: entry["socket"]["connector"]
                        .as_i64()
                        .ok_or("Missing approach connector binding")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        report["track_network"] = authored
            .unwrap()
            .world_network(&poses, &output.tracks, &ports, source, settings, &cancelled)?
            .ok_or("World track graph cancelled")?;
    }
    let mesh_geometry = if let Some(settings) = query.meshes {
        if poses.iter().any(|p| p["path_pose_matches"] != true) {
            return Err("Rail meshes require verified final approach endpoint poses".into());
        }
        let tracks = output
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| crate::rail_mesh_stage::Track {
                id: t.id,
                path: &t.path,
                source_start_flag: if i + 1 >= siding_first_track {
                    source.profile.siding.as_ref().unwrap().start_flag
                } else if i > list.len() {
                    source.profile.branching.as_ref().unwrap().start_flag
                } else if i == 0 {
                    source.profile.mesh.as_ref().unwrap().ring_start_flag
                } else {
                    source.profile.mesh.as_ref().unwrap().approach_start_flag
                },
                source_end_flag: if i + 1 >= siding_first_track {
                    source.profile.siding.as_ref().unwrap().end_flag
                } else if i > list.len() {
                    source.profile.branching.as_ref().unwrap().end_flag
                } else if i == 0 {
                    source.profile.mesh.as_ref().unwrap().ring_end_flag
                } else {
                    source.profile.mesh.as_ref().unwrap().approach_end_flag
                },
            })
            .collect::<Vec<_>>();
        let Some(geometry) = crate::rail_mesh_stage::build(
            source,
            meshes.unwrap(),
            &output.layers,
            &tracks,
            settings,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        report["meshes"] = geometry.report.clone();
        Some(geometry)
    } else {
        None
    };
    let gameplay_height = if let Some(work) = query.gameplay_height_work {
        let Some(height) = crate::rail_mesh_stage::gameplay_height(
            &output.layers,
            mesh_geometry
                .as_ref()
                .ok_or("Missing generated rail meshes")?,
            work,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        report["gameplay_height"] = json!({"engine":height.report.engine,"scope":"rail collision triangles only",
            "other_late_contributors_complete":false,"physics_cooking_validated":false,"changes":height.report});
        Some(height)
    } else {
        None
    };
    Ok(Some(Candidate {
        reservations: reservations.and_then(|r| r.output),
        gameplay_height,
        output,
        alpha,
        ocean,
        report,
        mesh_geometry,
    }))
}
pub fn publish(
    candidate: &Candidate,
    project: &Project,
    destination: &FilePath,
    cancelled: impl Fn() -> bool,
) -> Result<Value, String> {
    if destination.exists()
        || destination
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("map"))
    {
        return Err("Choose a new diagnostic preview directory, not a .map file".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(FilePath::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stage = tempfile::Builder::new()
        .prefix(".rail-bed-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let root = stage.path();
    let write = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(name))
            .map_err(|e| e.to_string())?;
        for chunk in bytes.chunks(65536) {
            if cancelled() {
                return Err("Bed export cancelled".into());
            }
            file.write_all(chunk).map_err(|e| e.to_string())?;
        }
        file.sync_all().map_err(|e| e.to_string())
    };
    project.save(&root.join("project.mapper"))?;
    write(
        "bed.json",
        &serde_json::to_vec_pretty(&candidate.report).map_err(|e| e.to_string())?,
    )?;
    if let Some(meshes) = &candidate.mesh_geometry {
        for chunk in &meshes.chunks {
            write(
                &chunk.filename(),
                &serde_json::to_vec(chunk).map_err(|e| e.to_string())?,
            )?;
        }
    }
    let layers = &candidate.output.layers;
    if let Some(reservations) = &candidate.reservations {
        if reservations.raster.len() != layers.surface_resolution.pow(2)
            || reservations.raster.iter().any(|v| *v > 1)
        {
            return Err("Invalid rail reservation raster".into());
        }
        write("rail-placement-blocked.u8", &reservations.raster)?;
        write(
            "rail-placement-sweeps.json",
            &serde_json::to_vec(&reservations.samples).map_err(|e| e.to_string())?,
        )?;
    }
    if let Some(height) = &candidate.gameplay_height {
        if height.height.len() != layers.height.len()
            || height
                .height
                .iter()
                .zip(&layers.height)
                .any(|(h, t)| h < t || *h > 32766)
        {
            return Err("Invalid rail gameplay-height grid".into());
        }
        write(
            "gameplay-height.i16le",
            &height
                .height
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )?;
        write(
            "gameplay-height.json",
            &serde_json::to_vec_pretty(&height.report).map_err(|e| e.to_string())?,
        )?;
    }
    write(
        "height.i16le",
        &layers
            .height
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write(
        "topology.i32le",
        &layers
            .topology
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    write("splat.u8", &layers.splat)?;
    write("biome.u8", &layers.biome)?;
    write("alpha.u8", &candidate.alpha)?;
    let image = crate::map_image::render(
        layers,
        Some(&candidate.alpha),
        Some(&candidate.ocean),
        crate::map_image::DISPLAY_RESOLUTION,
        &cancelled,
    )?
    .ok_or("Bed render cancelled")?;
    crate::headless::write_png(&root.join("overview.png"), &image)?;
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let mut file = File::open(entry.path()).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut bytes = 0;
        let mut buffer = [0; 65536];
        loop {
            if cancelled() {
                return Err("Bed export cancelled".into());
            }
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            bytes += n;
        }
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            json!({"bytes":bytes,"sha256":format!("{:x}",hash.finalize())}),
        );
    }
    let mut manifest = json!({"format":"papyrus-rail-bed-preview","schema":1,"playable":false,"generation_ready":false,
        "size":layers.size,"height_resolution":layers.height_resolution,"surface_resolution":layers.surface_resolution,
        "source_sha256":candidate.report["source_sha256"],"base_manifest_sha256":candidate.report["base_manifest_sha256"],
        "replay":"diagnostic checkpoint; project recipe is unchanged and does not enable rail generation","files":files});
    if candidate.gameplay_height.is_some() {
        manifest["gameplay_height"] = json!({"file":"gameplay-height.i16le","report":"gameplay-height.json",
            "engine":papyrus_terrain::gameplay_height::VERSION,"scope":"rail collision triangles only",
            "other_late_contributors_complete":false,"physics_cooking_validated":false});
    }
    if candidate.reservations.is_some() {
        manifest["placement_reservations"] = json!({"file":"rail-placement-blocked.u8","sweeps":"rail-placement-sweeps.json","scope":"input subset plus generated rail reservations","full_runtime_placement_map_reconstructed":false});
    }
    write(
        "manifest.json",
        &serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )?;
    if cancelled() {
        return Err("Bed export cancelled".into());
    }
    if destination.exists() {
        return Err("Bed output appeared during preparation".into());
    }
    std::fs::rename(root, destination).map_err(|e| e.to_string())?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deferred_preview_preserves_authored_channels_and_excludes_final_mesh_stages() {
        use papyrus_terrain::{monument_composition, native_layers, placement::Solution};
        let (_root, parent, source) = crate::retained_rail_height::tests::source_fixture();
        let profile: crate::native_profile::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-0f703d43.json"
        ))
        .unwrap();
        let mut project = Project {
            size: 1000,
            preview_engine: crate::native_preview::VERSION.into(),
            installation: Some(crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: crate::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: profile.native_sha256.clone(),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            native_profile: Some(profile),
            ..Default::default()
        };
        project.targets.clear();
        let (hr, n) = native_layers::dimensions(project.size).unwrap();
        let cells = n * n;
        let mut layers = native_layers::Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18021; hr * hr],
            topology: vec![1; cells],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[..cells].fill(255);
        layers.splat[..cells].fill(255);
        let shape = source.profile.bed_shape().unwrap();
        layers.topology[0] |= shape.rail as i32;
        layers.topology[cells - 1] |= shape.railside as i32;
        let maps = monument_composition::compose(&layers, &vec![255; cells], &[], || false)
            .unwrap()
            .unwrap();
        let layout = Solution {
            outcomes: vec![],
            required_missing: 0,
            attempts: 0,
        };
        let terrain = Terrain::from_composed(&project, &maps, &layout, &vec![0; cells], || false)
            .unwrap()
            .unwrap();
        let plan = json!({"source_sha256":source.sha256,"terrain_plan":{
        "manifest_sha256":terrain.manifest_digest(),"approaches":{
            "connector_source":{"infrastructure_sha256":source.infrastructure_digest()},
            "network_curve":{"points":[[-100.,50.,-100.],[100.,50.,-100.],[100.,50.,100.],[-100.,50.,100.],[-100.,50.,-100.]]},
            "result":{"status":"proposed","unresolved":[],"approaches":[]}
        }}});
        let query = || {
            serde_json::from_value::<Query>(json!({"maximum_work":128000000,"maximum_cut_fill":24}))
                .unwrap()
        };
        let old = build(
            &terrain,
            FilePath::new(""),
            &source,
            &parent,
            &plan,
            query(),
            None,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        let new = build_deferred_surfaces(
            &terrain,
            FilePath::new(""),
            &source,
            &parent,
            &plan,
            query(),
            None,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(new.output.layers.height, old.output.layers.height);
        assert_eq!(
            serde_json::to_value(&new.output.tracks).unwrap(),
            serde_json::to_value(&old.output.tracks).unwrap()
        );
        assert_eq!(new.output.layers.topology, layers.topology);
        assert_eq!(new.output.layers.splat, layers.splat);
        assert_eq!(new.alpha, maps.alpha);
        assert_eq!(new.report["engine"], rail_bed::TERRAIN_VERSION);
        assert_eq!(new.report["changes"]["surface_paint_deferred"], true);
        assert!(
            old.report["changes"]
                .get("surface_paint_deferred")
                .is_none()
        );
        let mut premature = query();
        premature.meshes = Some(crate::rail_mesh_stage::Settings {
            maximum_work: 10000,
            maximum_chunks: 10,
            maximum_geometry_bytes: 100000,
        });
        let error = build_deferred_surfaces(
            &terrain,
            FilePath::new(""),
            &source,
            &parent,
            &plan,
            premature,
            None,
            None,
            || false,
        )
        .err()
        .unwrap();
        assert!(error.contains("remain deferred"));
    }
    #[test]
    fn bed_queries_preserve_old_requests_and_validate_explicit_corridor_limits() {
        let old = serde_json::json!({"maximum_work":512000000,"maximum_cut_fill":24});
        let q: Query = serde_json::from_value(old.clone()).unwrap();
        q.validate().unwrap();
        assert!(q.corridor.is_none());
        assert!(q.grade_fit.is_none());
        assert!(q.branching.is_none());
        assert!(q.siding.is_none());
        assert!(q.reservation_work.is_none());
        let mut reservation = old.clone();
        reservation["reservation_work"] = json!(0);
        assert!(
            serde_json::from_value::<Query>(reservation.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        reservation["reservation_work"] = json!(128000000);
        serde_json::from_value::<Query>(reservation)
            .unwrap()
            .validate()
            .unwrap();
        let mut siding = old.clone();
        siding["siding"] = json!({"seed":37,"maximum_work":128000000,"maximum_sidings":64});
        assert!(
            serde_json::from_value::<Query>(siding.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        siding["grade_fit"] =
            json!({"maximum_grade":0.15,"maximum_work":8000000,"maximum_attempts":8});
        serde_json::from_value::<Query>(siding)
            .unwrap()
            .validate()
            .unwrap();
        let mut branching = old.clone();
        branching["branching"] = json!({"seed":17,"maximum_grade":0.15,"maximum_expansions":8000000,"maximum_work":128000000,"maximum_branches":64});
        assert!(
            serde_json::from_value::<Query>(branching.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        branching["grade_fit"] =
            json!({"maximum_grade":0.15,"maximum_work":8000000,"maximum_attempts":8});
        serde_json::from_value::<Query>(branching)
            .unwrap()
            .validate()
            .unwrap();
        assert!(q.gameplay_height_work.is_none());
        let mut height = old.clone();
        height["gameplay_height_work"] = json!(256000000);
        assert!(
            serde_json::from_value::<Query>(height.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        height["meshes"] =
            json!({"maximum_work":1000,"maximum_chunks":100,"maximum_geometry_bytes":100000});
        serde_json::from_value::<Query>(height.clone())
            .unwrap()
            .validate()
            .unwrap();
        height["gameplay_height_work"] = json!(0);
        assert!(
            serde_json::from_value::<Query>(height)
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(q.meshes.is_none());
        let mut mesh_query = old.clone();
        mesh_query["meshes"] = json!({"maximum_work":32000000,"maximum_chunks":8192,"maximum_geometry_bytes":536870912});
        serde_json::from_value::<Query>(mesh_query.clone())
            .unwrap()
            .validate()
            .unwrap();
        mesh_query["meshes"]["maximum_geometry_bytes"] = 0.into();
        assert!(
            serde_json::from_value::<Query>(mesh_query)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut d = old.clone();
        d["corridor"] = serde_json::json!({"maximum_work":16000000,"maximum_depth":16,
            "maximum_chord":2.,"maximum_deviation":0.01,"maximum_ground_error":0.5});
        serde_json::from_value::<Query>(d.clone())
            .unwrap()
            .validate()
            .unwrap();
        d["corridor"]["ground_refinement"] = json!({"maximum_depth":10,"minimum_extent":0.025});
        serde_json::from_value::<Query>(d.clone())
            .unwrap()
            .validate()
            .unwrap();
        d["corridor"]["ground_refinement"]["maximum_depth"] = 0.into();
        assert!(
            serde_json::from_value::<Query>(d.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        d["corridor"]["ground_refinement"]["maximum_depth"] = 10.into();
        d["corridor"]["maximum_work"] = 0.into();
        assert!(
            serde_json::from_value::<Query>(d)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut d = old;
        d["unknown_stage"] = true.into();
        assert!(serde_json::from_value::<Query>(d).is_err());
    }
    #[test]
    fn checkpoint_publication_is_atomic_receipted_and_never_overwrites() {
        let (h, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: h,
            surface_resolution: n,
            height: vec![16711; h * h], // normalized i16, approximately 10 metres
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[..n * n].fill(255);
        layers.splat[..n * n].fill(255);
        let ocean =
            papyrus_terrain::ocean::reconstruct(&layers.height, h, &layers.topology, n, || false)
                .unwrap()
                .unwrap();
        let mut candidate = Candidate {
            reservations: None,
            gameplay_height: None,
            output: rail_bed::Output {
                layers,
                tracks: vec![],
                report: Default::default(),
            },
            alpha: vec![255; n * n],
            ocean,
            report: json!({"source_sha256":"a".repeat(64),"base_manifest_sha256":"b".repeat(64)}),
            mesh_geometry: Some(crate::rail_mesh_stage::Output {
                report: json!({}),
                chunks: vec![crate::rail_mesh_stage::PlacedChunk {
                    track_id: 1,
                    lod: 0,
                    chunk_index: 0,
                    geometry: papyrus_terrain::path_mesh::Chunk {
                        first_repetition: 0,
                        repetitions: 1,
                        origin: [0., 0., 0.],
                        work: 4,
                        mesh: papyrus_terrain::path_mesh::Template {
                            vertices: vec![[0., 0., 0.], [1., 0., 0.], [0., 0., 1.]],
                            normals: vec![[0., 1., 0.]; 3],
                            tangents: vec![[1., 0., 0., 1.]; 3],
                            uv: vec![[0., 0.]; 3],
                            triangles: vec![[0, 1, 2]],
                            center: [0.5, 0., 0.5],
                            extents: [0.5, 0., 0.5],
                        },
                    },
                }],
            }),
        };
        let project = Project {
            size: 1000,
            ..Default::default()
        };
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("checkpoint");
        let calls = std::cell::Cell::new(0);
        assert!(
            publish(&candidate, &project, &destination, || {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .is_err()
        );
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(
            publish(&candidate, &project, &root.path().join("world.map"), || {
                false
            })
            .is_err()
        );
        let manifest = publish(&candidate, &project, &destination, || false).unwrap();
        assert!(
            manifest["files"]
                .get("rail-mesh-track-1-lod-0-chunk-0.json")
                .is_some()
        );
        for (name, receipt) in manifest["files"].as_object().unwrap() {
            let bytes = std::fs::read(destination.join(name)).unwrap();
            assert_eq!(
                receipt,
                &json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))})
            );
        }
        assert_eq!(manifest["playable"], false);
        assert_eq!(manifest["generation_ready"], false);
        let before = std::fs::read(destination.join("manifest.json")).unwrap();
        assert!(publish(&candidate, &project, &destination, || false).is_err());
        assert_eq!(
            before,
            std::fs::read(destination.join("manifest.json")).unwrap()
        );
        assert_eq!(
            Project::load(&destination.join("project.mapper"))
                .unwrap()
                .size,
            project.size
        );
        assert!(manifest.get("gameplay_height").is_none());
        let mut heights = candidate.output.layers.height.clone();
        heights[0] += 1;
        candidate.gameplay_height = Some(papyrus_terrain::gameplay_height::Output {
            height: heights,
            report: papyrus_terrain::gameplay_height::Report {
                engine: papyrus_terrain::gameplay_height::VERSION,
                contributors: 1,
                changed_samples: 1,
                work: 1,
                physics_cooking_validated: false,
            },
        });
        let with_height = root.path().join("with-gameplay-height");
        let manifest = publish(&candidate, &project, &with_height, || false).unwrap();
        let bytes = std::fs::read(with_height.join("gameplay-height.i16le")).unwrap();
        assert_eq!(&bytes[..2], &16712_i16.to_le_bytes());
        assert_eq!(
            manifest["files"]["gameplay-height.i16le"]["sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(
            manifest["gameplay_height"]["scope"],
            "rail collision triangles only"
        );
        assert_eq!(
            std::fs::read(with_height.join("height.i16le")).unwrap(),
            std::fs::read(destination.join("height.i16le")).unwrap()
        );
        let reservation_policy = papyrus_terrain::rail_reservations::Policy {
            engine: papyrus_terrain::rail_reservations::VERSION.into(),
            width: 20.,
            sample_spacing: 1.,
            terrain_origin_y: -500.,
            terrain_height: 1000.,
            spline: true,
        };
        let path = Path::from_controls(&[[0., 10., -50.], [0., 10., 50.]], 1000, || false)
            .unwrap()
            .unwrap();
        let tracks = [papyrus_terrain::rail_reservations::Track { id: 1, path: &path }];
        candidate.reservations = Some(
            papyrus_terrain::rail_reservations::generate(
                1000.,
                n,
                &vec![0; n * n],
                &tracks,
                &reservation_policy,
                128000000,
                || false,
            )
            .unwrap()
            .unwrap(),
        );
        let with_reservations = root.path().join("with-reservations");
        let manifest = publish(&candidate, &project, &with_reservations, || false).unwrap();
        let raster = std::fs::read(with_reservations.join("rail-placement-blocked.u8")).unwrap();
        assert_eq!(raster, candidate.reservations.as_ref().unwrap().raster);
        assert_eq!(
            manifest["files"]["rail-placement-blocked.u8"]["sha256"],
            format!("{:x}", Sha256::digest(&raster))
        );
        assert_eq!(
            manifest["placement_reservations"]["full_runtime_placement_map_reconstructed"],
            false
        );
        candidate.gameplay_height.as_mut().unwrap().height[0] = 0;
        let invalid = root.path().join("invalid-gameplay-height");
        assert!(publish(&candidate, &project, &invalid, || false).is_err());
        assert!(!invalid.exists());
    }
}
