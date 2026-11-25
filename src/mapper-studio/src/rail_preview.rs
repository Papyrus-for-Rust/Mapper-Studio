//! Read-only rail planning against verified final terrain from a preview bundle.
//! This does not import a playable world or enable a complete rail generation stage.
use crate::{Project, native_preview};
use papyrus_terrain::{native_layers, rail_costmap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashSet}, fs::File, io::Read, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub settings: rail_costmap::RouteSettings,
}
pub struct Terrain {
    memory: Option<Memory>,
    water: Option<Vec<f32>>,
    project: Project,
    size: u32,
    hr: usize,
    n: usize,
    height: Vec<i16>,
    topology: Vec<i32>,
    placement: Vec<u8>,
    placement_origin: &'static str,
    receipts: BTreeMap<String, Value>,
    manifest_sha256: String,
    layout_receipt: Option<Value>,
    manifest_files: Value,
    bed_channels: bool,
}
struct Memory {
    biome: Vec<u8>,
    splat: Vec<u8>,
    alpha: Vec<u8>,
    layout: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Socket {
    pub monument: u64,
    pub connector: i64,
    pub prefab: String,
    pub position: [f32; 3],
    pub forward: [f32; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sockets {
    pub layout_sha256: String,
    pub infrastructure_sha256: String,
    pub sockets: Vec<Socket>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<GeneratedConnections>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedConnections {
    pub source_sha256: String,
    pub occurrences_sha256: String,
    pub monuments: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct MonumentPose {
    pub monument: u64,
    pub prefab: String,
    pub position: [f32; 3],
    pub yaw: f32,
}
#[derive(Debug, Serialize)]
pub struct Monuments {
    pub layout_sha256: String,
    pub poses: Vec<MonumentPose>,
}
fn yaw_vector(p: [f32; 3], yaw: f32) -> [f32; 3] {
    let (s, c) = yaw.to_radians().sin_cos();
    [c * p[0] - s * p[2], p[1], s * p[0] + c * p[2]]
}
/// Shared diagnostic bridge. Full-width clearance, bed shaping, alpha and actual
/// prefab TrainTrackSpline connections are not established by pose matches.
pub fn approach_report(
    grid: &rail_costmap::Grid,
    source: &crate::rail_source::Source,
    sockets: &Sockets,
    ring: &papyrus_terrain::rail_ring::Outcome,
    settings: papyrus_terrain::rail_approaches::Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Value, String> {
    approach_report_prepared(grid, source, sockets, ring, settings, None, cancelled)
}
/// Reuse the primary curve prepared at the earlier ring stage. Its work remains
/// charged to the same approach geometry allowance used by the combined entry.
pub(crate) enum ApproachCurve<'a> {
    Generated(&'a papyrus_terrain::path_geometry::Path),
    Edited {
        original: &'a papyrus_terrain::path_geometry::Path,
        revised: &'a papyrus_terrain::path_geometry::Path,
    },
}

pub(crate) fn approach_report_prepared(
    grid: &rail_costmap::Grid,
    source: &crate::rail_source::Source,
    sockets: &Sockets,
    ring: &papyrus_terrain::rail_ring::Outcome,
    settings: papyrus_terrain::rail_approaches::Settings,
    prepared: Option<ApproachCurve<'_>>,
    cancelled: impl Fn() -> bool,
) -> Result<Value, String> {
    use papyrus_terrain::{
        path_geometry,
        rail_approaches::{self, Network},
        rail_ring,
    };
    if cancelled() {
        return Err("Rail approach planning cancelled".into());
    }
    settings.validate()?;
    let policy =
        source.profile.approaches.as_ref().ok_or(
            "This rail snapshot has no source-bound approach adapter; prepare a new snapshot",
        )?;
    if sockets.infrastructure_sha256 != source.infrastructure_digest() {
        return Err("Rail connector source differs from the approach source".into());
    }
    let mut report = json!({"engine":rail_approaches::VERSION,"settings":settings,"policy":policy,
        "input_socket_count":sockets.sockets.len(),"generation_ready":false,
        "connector_source":sockets,"terrain_applied":false,"full_corridor_validated":false,
        "socket_connections_validated":false,"fallback_network_joins_implemented":false});
    let rail_ring::Outcome::Found { cells, points, .. } = ring else {
        report["result"] = json!({"status":"unresolved","reason":"No primary ring candidate"});
        return Ok(report);
    };
    let curve_budget = settings.maximum_geometry_work.min(32000000);
    let owned;
    let curve = match &prepared {
        Some(ApproachCurve::Generated(curve) | ApproachCurve::Edited { revised: curve, .. }) => *curve,
        None => {
            owned = match path_geometry::build(
                points, source.profile.path_settings(curve_budget), None, &cancelled,
            ) {
                Ok(Some(curve)) => curve,
                Ok(None) => return Err("Primary ring curve preparation cancelled".into()),
                Err(path_geometry::Failure::Budget) => {
                    report["result"] = json!({"status":"budget_exceeded","expansions":0,"geometry_work":curve_budget});
                    return Ok(report);
                }
                Err(e) => return Err(format!("Primary ring curve preparation failed: {e:?}")),
            };
            &owned
        }
    };
    let mut remaining = settings;
    remaining.maximum_geometry_work = settings.maximum_geometry_work.saturating_sub(curve.work());
    if remaining.maximum_geometry_work == 0 {
        report["result"] =
            json!({"status":"budget_exceeded","expansions":0,"geometry_work":curve.work()});
        return Ok(report);
    }
    let edited_cells;
    let cells = if let Some(ApproachCurve::Edited { original, revised }) = prepared {
        edited_cells = edited_ring_targets(
            grid, cells, original, revised, &mut remaining.maximum_geometry_work, &cancelled,
        )?;
        edited_cells.as_slice()
    } else {
        cells.as_slice()
    };
    let inputs = sockets
        .sockets
        .iter()
        .map(|s| rail_approaches::Socket {
            monument: s.monument,
            connector: s.connector,
            position: s.position,
            forward: s.forward,
        })
        .collect::<Vec<_>>();
    let result = rail_approaches::build(
        grid,
        Network { cells, path: curve },
        &inputs,
        policy,
        remaining,
        source.profile.path_settings(32000000),
        &cancelled,
    )?;
    if matches!(result, rail_approaches::Outcome::Cancelled) {
        return Err("Rail approach planning cancelled".into());
    }
    report["result"] = serde_json::to_value(result).map_err(|e| e.to_string())?;
    report["network_curve"] = serde_json::to_value(curve).map_err(|e| e.to_string())?;
    Ok(report)
}

fn edited_ring_targets(
    grid: &rail_costmap::Grid,
    source_cells: &[[usize; 2]],
    original: &papyrus_terrain::path_geometry::Path,
    curve: &papyrus_terrain::path_geometry::Path,
    remaining_work: &mut usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<[usize; 2]>, String> {
    const MAX_TARGET_COMPARISONS: usize = 4_000_000;
    let count = original.points().len() - 1;
    let comparison_bound = source_cells.len().saturating_mul(count)
        .saturating_add(count + curve.points().len());
    if comparison_bound > MAX_TARGET_COMPARISONS.min(*remaining_work / 4) {
        return sample_ring_targets(grid, curve, 0..curve.points().len() - 1,
            Vec::new(), remaining_work, cancelled);
    }
    let project = |path: &papyrus_terrain::path_geometry::Path| {
        path.points().iter().map(|&[x, _, z]| [x, z]).collect::<Vec<_>>()
    };
    let old = project(original);
    let new = project(curve);
    let removed = crate::path_edit::changed_segments(&new, &old);
    let added = crate::path_edit::changed_segments(&old, &new);
    let mut changed = vec![false; count];
    for span in removed {
        changed[span].fill(true);
    }
    let mut retained = Vec::with_capacity(source_cells.len());
    let n = grid.resolution();
    *remaining_work -= count + curve.points().len();
    for &cell in source_cells {
        if cancelled() {
            return Err("Rail approach planning cancelled".into());
        }
        let [x, _, z] = grid.point(cell).ok_or("Invalid original rail approach target")?;
        if grid.costs()[cell[1] * n + cell[0]] == 0 {
            continue;
        }
        // Search cells are drawn from the unsmoothed ring and need not be
        // directly under the curve. Assign them to the nearest original span.
        // Keep ties with unchanged spans at an intersection or boundary.
        let nearest = old.windows(2).zip(&changed).map(|(pair, &changed)| {
            (segment_distance_squared([x, z], pair[0], pair[1]), changed)
        }).min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        *remaining_work -= count;
        if nearest.is_some_and(|(_, changed)| !changed) {
            retained.push(cell);
        }
    }
    let retained_count = retained.len();
    let result = sample_ring_targets(
        grid, curve, added.into_iter().flatten(), retained, remaining_work, cancelled,
    )?;
    if std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some() {
        eprintln!("MAPPER_RAIL_EDIT_TARGETS {}", json!({
            "source":source_cells.len(), "retained":retained_count, "final":result.len(),
        }));
    }
    Ok(result)
}

fn segment_distance_squared([x, z]: [f32; 2], [ax, az]: [f32; 2], [bx, bz]: [f32; 2]) -> f32 {
    let (dx, dz) = (bx - ax, bz - az);
    let length_squared = dx * dx + dz * dz;
    let t = if length_squared > 0. {
        (((x - ax) * dx + (z - az) * dz) / length_squared).clamp(0., 1.)
    } else {
        0.
    };
    let (x, z) = (x - ax - t * dx, z - az - t * dz);
    x * x + z * z
}

fn sample_ring_targets(
    grid: &rail_costmap::Grid,
    curve: &papyrus_terrain::path_geometry::Path,
    segments: impl Iterator<Item = usize>,
    mut cells: Vec<[usize; 2]>,
    remaining_work: &mut usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<[usize; 2]>, String> {
    let n = grid.resolution();
    let spacing = grid.size() / n as f32;
    let controls = curve.points();
    let segment_length = curve.length() / (controls.len() - 1) as f32;
    let mut seen: HashSet<_> = cells.iter().copied().collect();
    for i in segments {
        let [a, b] = [controls[i], controls[i + 1]];
        let chord = (a[0] - b[0]).hypot(a[2] - b[2]);
        // Bound Hermite speed using the chord and two unit tangents so
        // even a long, manually displaced segment is sampled within a cell.
        let steps = ((1.5 * chord + 2. * curve.step_size()) / (spacing * 0.5)).ceil() as usize;
        let steps = steps.max(1);
        *remaining_work = remaining_work.checked_sub(steps)
            .ok_or("Edited rail ring exhausted the approach work allowance")?;
        for step in 0..steps {
            if cancelled() {
                return Err("Rail approach planning cancelled".into());
            }
            let distance = (i as f32 + step as f32 / steps as f32) * segment_length;
            let [x, _, z] = curve.point(distance).map_err(|e| format!("Invalid edited rail ring: {e:?}"))?;
            let cell = grid.cell([x, z])?;
            if grid.costs()[cell[1] * n + cell[0]] > 0 && seen.insert(cell) {
                cells.push(cell);
                if cells.len() > 32768 {
                    return Err("Edited rail ring exceeds the approach target allowance".into());
                }
            }
        }
    }
    if cells.len() < 2 {
        return Err("Edited rail ring needs at least two passable approach targets".into());
    }
    Ok(cells)
}
/// Decode a layout produced by the shared placement stage. Disk callers must
/// verify its file receipt first; the digest records these exact layout bytes.
pub fn layout_sockets(
    project: &Project,
    infrastructure: &crate::infrastructure::Source,
    rail_type: i32,
    raw: &[u8],
    cancelled: impl Fn() -> bool,
) -> Result<Sockets, String> {
    project.validate()?;
    if raw.len() > 4 * 1024 * 1024
        || project
            .monument_assets
            .as_ref()
            .is_none_or(|p| p.catalog_sha256 != infrastructure.catalog_sha256)
    {
        return Err("Rail connector layout requires a bounded saved catalog binding".into());
    }
    let digest = format!("{:x}", Sha256::digest(raw));
    let layout: Value = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    if layout["engine"].as_str() != Some(project.placement_engine.as_str())
        || layout["seed"].as_u64() != Some(project.seeds[4].value.into())
    {
        return Err("Rail connector layout recipe mismatch".into());
    }
    let outcomes = layout["outcomes"]
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or("Invalid rail connector layout outcomes")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut sockets = Vec::new();
    for outcome in outcomes {
        if cancelled() {
            return Err("Rail connector inspection cancelled".into());
        }
        let id = outcome["id"].as_u64().ok_or("Invalid rail monument id")?;
        if !seen.insert(id) {
            return Err("Duplicate rail monument outcome".into());
        }
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == id)
            .ok_or("Unknown rail monument outcome")?;
        let pose = outcome
            .get("position")
            .ok_or("Rail monument outcome lacks position status")?;
        if pose.is_null() {
            continue;
        }
        let value = |name: &str| -> Result<f32, String> {
            pose[name]
                .as_f64()
                .map(|v| v as f32)
                .filter(|v| v.is_finite() && v.abs() <= 10000.)
                .ok_or_else(|| format!("Invalid accepted rail monument {name}"))
        };
        let origin = [value("x")?, value("y")?, value("z")?];
        let yaw = value("yaw")?;
        let path = target
            .prefab_path
            .as_ref()
            .ok_or("Accepted rail monument lacks prefab identity")?;
        let prefab = infrastructure
            .prefabs
            .get(path)
            .filter(|p| p.complete)
            .ok_or("Accepted monument has incomplete connector metadata")?;
        for connector in prefab
            .connectors
            .iter()
            .filter(|c| c.infrastructure_type == rail_type)
        {
            let offset = yaw_vector(connector.position, yaw);
            sockets.push(Socket {
                monument: id,
                connector: connector.object_id,
                prefab: path.clone(),
                position: std::array::from_fn(|i| origin[i] + offset[i]),
                forward: yaw_vector(connector.forward, yaw),
            });
            if sockets.len() > 256 {
                return Err("Rail connector count exceeds supported budget".into());
            }
        }
    }
    Ok(Sockets {
        layout_sha256: digest,
        infrastructure_sha256: infrastructure.sha256.clone(),
        sockets,
        generated: None,
    })
}
fn read(path: &Path, limit: usize, cancelled: &impl Fn() -> bool) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 65536];
    loop {
        if cancelled() {
            return Err("Rail terrain inspection cancelled".into());
        }
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > limit {
            return Err("Rail preview input exceeds byte bound".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    Ok(bytes)
}
impl Terrain {
    pub fn from_composed(
        project: &Project,
        maps: &papyrus_terrain::monument_composition::Composed,
        layout: &papyrus_terrain::placement::Solution,
        placement: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        project.validate()?;
        if cancelled() {
            return Ok(None);
        }
        let (hr, n) = native_layers::dimensions(project.size)?;
        let cells = n * n;
        if !native_preview::is_native(&project.preview_engine)
            || maps.layers.size != project.size
            || maps.layers.terrain_seed != project.seeds[0].value
            || maps.layers.biome_seed != project.seeds[1].value
            || maps.layers.height_resolution != hr
            || maps.layers.surface_resolution != n
            || maps.layers.height.len() != hr * hr
            || maps.layers.topology.len() != cells
            || maps.layers.biome.len() != cells * 5
            || maps.layers.splat.len() != cells * 8
            || maps.alpha.len() != cells
            || placement.len() != cells
            || placement.iter().any(|v| *v > 1)
            || layout.outcomes.len() != project.targets.len()
        {
            return Err("Invalid in-memory rail composition inputs".into());
        }
        let layout=serde_json::to_vec(&json!({"engine":project.placement_engine,"seed":project.seeds[4].value,
            "outcomes":layout.outcomes.iter().map(|o|json!({"id":o.id,"position":o.position.as_ref().map(|p|json!({"x":p.x,"y":p.y,"z":p.z,"yaw":p.yaw}))})).collect::<Vec<_>>()})).map_err(|e|e.to_string())?;
        let receipt =
            |raw: &[u8]| json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(raw))});
        let mut receipts = BTreeMap::new();
        receipts.insert(
            "project".into(),
            receipt(&serde_json::to_vec(project).map_err(|e| e.to_string())?),
        );
        receipts.insert("layout".into(), receipt(&layout));
        receipts.insert("placement".into(), receipt(placement));
        let mut height_hash = Sha256::new();
        if crate::road_planning::hash_raster_words(
            &mut height_hash,
            &maps.layers.height,
            |v| v.to_le_bytes(),
            &cancelled,
        ).is_none() {
            return Ok(None);
        }
        receipts.insert(
            "height".into(),
            json!({"sha256":format!("{:x}",height_hash.finalize())}),
        );
        let mut topology_hash = Sha256::new();
        if crate::road_planning::hash_raster_words(
            &mut topology_hash,
            &maps.layers.topology,
            |v| v.to_le_bytes(),
            &cancelled,
        ).is_none() {
            return Ok(None);
        }
        receipts.insert(
            "topology".into(),
            json!({"sha256":format!("{:x}",topology_hash.finalize())}),
        );
        for (name, raw) in [
            ("biome", maps.layers.biome.as_slice()),
            ("splat", maps.layers.splat.as_slice()),
            ("alpha", maps.alpha.as_slice()),
        ] {
            receipts.insert(name.into(), receipt(raw));
        }
        let water = if let Some(rivers) = &maps.rivers {
            let Some(raw) = rivers.collider_heights(project.size, n, &cancelled)? else {
                return Ok(None);
            };
            let mut hash = Sha256::new();
            if crate::road_planning::hash_raster_words(
                &mut hash,
                &raw.heights,
                |h| h.to_le_bytes(),
                &cancelled,
            ).is_none() {
                return Ok(None);
            }
            receipts.insert(
                "river_collider_heights".into(),
                json!({
                    "sha256":format!("{:x}",hash.finalize()), "report":raw.report,
                    "no_hit_height":0., "sampling":"nearest cell-centre triangle proxy",
                    "other_water_families_complete":false,
                }),
            );
            Some(raw.heights)
        } else {
            None
        };
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&receipts).map_err(|e| e.to_string())?)
        );
        Ok(Some(Self {
            water,
            memory: Some(Memory {
                biome: maps.layers.biome.clone(),
                splat: maps.layers.splat.clone(),
                alpha: maps.alpha.clone(),
                layout: layout.clone(),
            }),
            project: project.clone(),
            size: project.size,
            hr,
            n,
            height: maps.layers.height.clone(),
            topology: maps.layers.topology.clone(),
            placement: placement.to_vec(),
            placement_origin: "in-memory-monument-and-network-subset",
            receipts,
            manifest_sha256: digest,
            layout_receipt: Some(receipt(&layout)),
            manifest_files: Value::Null,
            bed_channels: true,
        }))
    }
    pub fn load(
        root: &Path,
        project: &Project,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        project.validate()?;
        if !native_preview::is_native(&project.preview_engine) {
            return Err("Rail terrain planning requires a native preview recipe".into());
        }
        let raw = read(&root.join("manifest.json"), 1024 * 1024, &cancelled)?;
        let manifest: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        let (hr, n) = native_layers::dimensions(project.size)?;
        if manifest["format"] != "papyrus-preview-bundle"
            || manifest["schema"] != 1
            || manifest["playable"] != false
            || manifest["composed"] != true
            || manifest["size"].as_u64() != Some(project.size.into())
            || manifest["height_resolution"].as_u64() != Some(hr as u64)
            || manifest["surface_resolution"].as_u64() != Some(n as u64)
            || manifest["grid_rows"] != "world -Z to +Z; planar channels; little-endian words"
            || manifest["height_encoding"] != "signed i16 normalized by 32766 over -500..500 metres"
        {
            return Err("Rail terrain requires a supported composed native preview bundle".into());
        }
        if let Some(declaration) = manifest.get("decoration")
            && declaration
                != &serde_json::to_value(&project.decoration).map_err(|e| e.to_string())?
        {
            return Err("Preview decoration declaration differs from its saved recipe".into());
        }
        let mut receipts = BTreeMap::new();
        let mut checked =
            |name: &str, limit: usize, exact: Option<usize>| -> Result<Vec<u8>, String> {
                let bytes = read(&root.join(name), limit, &cancelled)?;
                let receipt =
                    json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))});
                if manifest["files"][name] != receipt || exact.is_some_and(|n| bytes.len() != n) {
                    return Err(format!(
                        "Rail terrain preview receipt/length mismatch: {name}"
                    ));
                }
                receipts.insert(name.to_owned(), receipt);
                Ok(bytes)
            };
        let saved: Project =
            serde_json::from_slice(&checked("project.mapper", 4 * 1024 * 1024, None)?)
                .map_err(|e| e.to_string())?;
        saved.validate()?;
        if &saved != project {
            return Err("Rail terrain bundle belongs to a different saved recipe".into());
        }
        let height = checked("height.i16le", hr * hr * 2, Some(hr * hr * 2))?
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        let topology = checked("topology.i32le", n * n * 4, Some(n * n * 4))?
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let water = if project.rivers.is_some() {
            if manifest["rivers"]["collider_heights"] != "river-collider-heights.f32le"
                || manifest["rivers"]["collider_no_hit_height"] != 0.
                || manifest["rivers"]["collider_sampling"] != "nearest cell-centre triangle proxy"
            {
                return Err("River bundle lacks its collider-height declaration".into());
            }
            let bytes = checked("river-collider-heights.f32le", n * n * 4, Some(n * n * 4))?;
            let mut levels = Vec::with_capacity(n * n);
            for chunk in bytes.chunks(4096) {
                if cancelled() {
                    return Err("Rail water loading cancelled".into());
                }
                for bytes in chunk.chunks_exact(4) {
                    let h = f32::from_le_bytes(bytes.try_into().unwrap());
                    if !h.is_finite() || !(-500. ..=500.).contains(&h) {
                        return Err("Invalid saved river collider height".into());
                    }
                    levels.push(h);
                }
            }
            Some(levels)
        } else {
            None
        };
        // Only an explicit no-decoration declaration can stand in for this
        // decoration-only raster. Unknown/requested occupancy remains an error.
        let (placement, placement_origin) = if manifest["files"]
            .get("decoration-blocked.u8")
            .is_some()
        {
            (
                checked("decoration-blocked.u8", n * n, Some(n * n))?,
                "receipted-decoration-raster",
            )
        } else if project.decoration.is_none()
            && manifest.get("decoration") == Some(&Value::Null)
            && !root.join("decoration-blocked.u8").exists()
        {
            (vec![0; n * n], "explicit-no-decoration")
        } else {
            return Err("Rail preview lacks a verified decoration blocker raster or explicit disabled declaration".into());
        };
        if placement.iter().any(|&v| v > 1) {
            return Err("Invalid preview placement blocker".into());
        }
        if cancelled() {
            return Err("Rail terrain inspection cancelled".into());
        }
        Ok(Self {
            memory: None,
            water,
            project: saved,
            size: project.size,
            hr,
            n,
            height,
            topology,
            placement,
            placement_origin,
            receipts,
            manifest_sha256: format!("{:x}", Sha256::digest(&raw)),
            layout_receipt: manifest["files"].get("layout.json").cloned(),
            manifest_files: manifest["files"].clone(),
            bed_channels: manifest["biome_channels"] == 5 && manifest["splat_channels"] == 8,
        })
    }
    /// Only accepted poses from the matching receipted layout participate. Source
    /// rail connectors include inactive descendants, as in the inspected server.
    pub fn sockets(
        &self,
        root: &Path,
        project: &Project,
        infrastructure: &crate::infrastructure::Source,
        rail_type: i32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Sockets, String> {
        let raw = self.layout_bytes(root, project, infrastructure, &cancelled)?;
        layout_sockets(project, infrastructure, rail_type, &raw, cancelled)
    }
    fn layout_bytes(
        &self,
        root: &Path,
        project: &Project,
        infrastructure: &crate::infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<u8>, String> {
        if project != &self.project
            || project
                .monument_assets
                .as_ref()
                .is_none_or(|p| p.catalog_sha256 != infrastructure.catalog_sha256)
        {
            return Err("Rail connector layout requires the saved recipe and catalog".into());
        }
        let raw = if let Some(memory) = &self.memory {
            memory.layout.clone()
        } else {
            read(&root.join("layout.json"), 4 * 1024 * 1024, &cancelled)?
        };
        let digest = format!("{:x}", Sha256::digest(&raw));
        if self.layout_receipt.as_ref() != Some(&json!({"bytes":raw.len(),"sha256":digest})) {
            return Err("Rail connector layout receipt mismatch".into());
        }
        Ok(raw)
    }
    pub fn monuments(
        &self,
        root: &Path,
        infrastructure: &crate::infrastructure::Source,
        rail_type: i32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Monuments, String> {
        let raw = self.layout_bytes(root, &self.project, infrastructure, &cancelled)?;
        let sockets = layout_sockets(&self.project, infrastructure, rail_type, &raw, &cancelled)?;
        let ids: std::collections::BTreeSet<_> =
            sockets.sockets.iter().map(|s| s.monument).collect();
        let layout: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        let mut poses = Vec::new();
        for outcome in layout["outcomes"]
            .as_array()
            .ok_or("Missing accepted outcomes")?
        {
            if cancelled() {
                return Err("Authored monument pose load cancelled".into());
            }
            let id = outcome["id"]
                .as_u64()
                .ok_or("Invalid authored monument id")?;
            if !ids.contains(&id) {
                continue;
            }
            let target = self
                .project
                .targets
                .iter()
                .find(|t| t.instance_id == id)
                .ok_or("Missing authored monument target")?;
            let pose = &outcome["position"];
            let number = |key: &str| {
                pose[key]
                    .as_f64()
                    .map(|v| v as f32)
                    .ok_or("Missing accepted monument coordinate")
            };
            poses.push(MonumentPose {
                monument: id,
                prefab: target
                    .prefab_path
                    .clone()
                    .ok_or("Missing authored monument prefab")?,
                position: [number("x")?, number("y")?, number("z")?],
                yaw: number("yaw")?,
            });
        }
        poses.sort_by_key(|p| p.monument);
        Ok(Monuments {
            layout_sha256: sockets.layout_sha256,
            poses,
        })
    }
    pub fn grid(
        &self,
        policy: &rail_costmap::Policy,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<rail_costmap::Grid>, String> {
        rail_costmap::build(
            rail_costmap::Terrain {
                size: self.size as f32,
                height_resolution: self.hr,
                height: &self.height,
                topology_resolution: self.n,
                topology: &self.topology,
                placement_resolution: self.n,
                placement: &self.placement,
            },
            policy,
            128_000_000,
            cancelled,
        )
    }
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_sha256
    }
    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn road_report(&self, root: &Path, cancelled: impl Fn() -> bool) -> Result<Value, String> {
        let raw = read(&root.join("roads.json"), 64 * 1024 * 1024, &cancelled)?;
        if self.manifest_files["roads.json"]
            != json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))})
        {
            return Err("Road report receipt differs from terrain bundle".into());
        }
        let value: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        let recipe = self
            .project
            .roads
            .as_ref()
            .ok_or("Road report lacks a saved recipe")?;
        let expected_engine = if let Some((staged, layout)) = recipe
            .staged
            .as_ref()
            .and_then(|staged| staged.layout.as_ref().map(|layout| (staged, layout)))
        {
            // Ordered source plans retain their family/network report engine.
            // The cost-routing engine identifies the enclosing recipe instead.
            let handoff = match staged.engine.as_str() {
                crate::road_generation::RAW_SOURCE_VERSION
                | crate::road_generation::SOURCE_COUPLED_VERSION => {
                    crate::source_road_handoff::RAW_VERSION
                }
                crate::road_generation::SOURCE_VERSION => {
                    crate::source_road_handoff::STRIP_CHECKED_VERSION
                }
                crate::road_generation::CONNECTED_SOURCE_VERSION
                | crate::road_generation::CONNECTED_COUPLED_VERSION => {
                    crate::source_road_handoff::JUNCTION_CHECKED_VERSION
                }
                _ => crate::source_road_handoff::VERSION,
            };
            let source = &value["source_layout"];
            if source["engine"] != handoff
                || source["source_sha256"] != recipe.source_sha256
                || source["assets_sha256"] != layout.assets_sha256
                || source["roles_sha256"] != layout.roles_sha256
            {
                return Err("Road report layout differs from saved recipe".into());
            }
            if value["families"]
                .as_array()
                .is_some_and(|families| !families.is_empty())
            {
                papyrus_terrain::road_network::FAMILY_VERSION
            } else {
                papyrus_terrain::road_network::NETWORK_VERSION
            }
        } else {
            recipe.engine.as_str()
        };
        if value["engine"] != expected_engine {
            return Err("Road report engine differs from saved recipe".into());
        }
        Ok(value)
    }
    /// Verified implemented placement subset, not Unity's complete runtime map.
    pub fn placement_subset(&self) -> (&[u8], &str) {
        (&self.placement, self.placement_origin)
    }
    pub fn bed_water(&self) -> Option<&[f32]> {
        self.water.as_deref()
    }
    pub fn bed_layers(
        &self,
        root: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<(papyrus_terrain::native_layers::Layers, Vec<u8>), String> {
        if !self.bed_channels {
            return Err(
                "Rail bed preview requires declared native biome/splat channel layouts".into(),
            );
        }
        let cells = self.n * self.n;
        let checked = |name: &str, length: usize| -> Result<Vec<u8>, String> {
            if let Some(memory) = &self.memory {
                if cancelled() {
                    return Err("In-memory rail channels cancelled".into());
                }
                let bytes = match name {
                    "biome.u8" => &memory.biome,
                    "splat.u8" => &memory.splat,
                    "alpha.u8" => &memory.alpha,
                    _ => return Err("Unknown in-memory rail channel".into()),
                };
                if bytes.len() != length {
                    return Err("In-memory rail channel length changed".into());
                }
                return Ok(bytes.clone());
            }
            let raw = read(&root.join(name), length, &cancelled)?;
            if raw.len() != length
                || self.manifest_files[name]
                    != json!({"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(&raw))})
            {
                return Err(format!("Rail bed input receipt mismatch: {name}"));
            }
            Ok(raw)
        };
        Ok((
            papyrus_terrain::native_layers::Layers {
                size: self.size,
                terrain_seed: self.project.seeds[0].value,
                biome_seed: self.project.seeds[1].value,
                height_resolution: self.hr,
                surface_resolution: self.n,
                height: self.height.clone(),
                topology: self.topology.clone(),
                biome: checked("biome.u8", cells * 5)?,
                splat: checked("splat.u8", cells * 8)?,
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            },
            checked("alpha.u8", cells)?,
        ))
    }
    pub fn audit(
        &self,
        grid: &rail_costmap::Grid,
        query: Option<&Query>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Value, String> {
        let mut report = json!({"manifest_sha256":self.manifest_sha256,"files":self.receipts,
            "costmap":grid.audit(),"placement_scope":"Implemented decoration blockers only; full runtime placement map unavailable",
            "placement_origin":self.placement_origin,
            "generation_ready":false,"terrain_applied":false,"socket_connections_validated":false,
            "alpha_clearance_validated":false,"continuous_curve_validated":false});
        if let Some(query) = query {
            let start = grid.cell(query.start)?;
            let end = grid.cell(query.end)?;
            let route = rail_costmap::route(grid, start, end, query.settings, &cancelled)?;
            if matches!(route, rail_costmap::Route::Cancelled) {
                return Err("Rail routing cancelled".into());
            }
            report["route"] = json!({"engine":rail_costmap::ROUTING_VERSION,"settings":query.settings,
                "requested_start":query.start,"requested_end":query.end,"snapped_start":start,"snapped_end":end,
                "snapped_endpoints_only":true,"result":route});
        }
        if cancelled() {
            return Err("Rail terrain inspection cancelled".into());
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installation::{Installation, InstallationKind, SourceFile};

    #[test]
    fn edited_ring_approaches_target_the_new_alignment_only() {
        let policy = rail_costmap::Policy {
            spacing: 10., near_radius: 0.1, clearance_radius: 0.1,
            blocked_topology: 1, clearance_topology: 2, water_topology: 4, cliff_topology: 8,
            maximum_slope: 45., steep_slope: 30., moderate_slope: 15.,
            normal_cost: 1, moderate_cost: 2, steep_cost: 4,
        };
        let grid = rail_costmap::build(rail_costmap::Terrain {
            size: 1000., height_resolution: 3, height: &[16711; 9],
            topology_resolution: 2, topology: &[0; 4],
            placement_resolution: 2, placement: &[0; 4],
        }, &policy, 2_000_000, || false).unwrap().unwrap();
        let path = |east| papyrus_terrain::path_geometry::Path::from_controls(
            &[
                [-200., 10., -200.], [0., 10., -200.], [200., 10., -200.],
                [200., 10., -100.], [east, 10., 0.], [200., 10., 100.],
                [200., 10., 200.], [0., 10., 200.], [-200., 10., 200.],
                [-200., 10., 0.], [-200., 10., -200.],
            ], 100_000, || false,
        ).unwrap().unwrap();
        let original = path(200.);
        let revised = path(300.);
        let remote_raw = grid.cell([-220., 0.]).unwrap();
        let raw_near_old = grid.cell([190., 0.]).unwrap();
        let old_center = grid.cell([200., 0.]).unwrap();
        let source = [remote_raw, raw_near_old, old_center];
        let mut remaining = 100_000;
        let targets = edited_ring_targets(&grid, &source, &original, &revised, &mut remaining, &|| false).unwrap();
        assert_eq!(targets[0], remote_raw);
        assert!(targets.iter().any(|&cell| {
            let [x, _, z] = grid.point(cell).unwrap();
            (x - 300.).hypot(z) < 10.
        }));
        assert!(!targets.contains(&old_center) && !targets.contains(&raw_near_old));
        assert_eq!(targets.iter().filter(|cell| cell[0] < grid.resolution() / 2).count(), 1);
        assert_eq!(edited_ring_targets(&grid, &source, &original, &original, &mut remaining, &|| false).unwrap(), source);
        assert!(remaining < 100_000);
        assert!(edited_ring_targets(&grid, &source, &original, &revised, &mut 1, &|| false).is_err());
        assert!(edited_ring_targets(&grid, &source, &original, &revised, &mut remaining, &|| true).is_err());
    }

    #[test]
    fn in_memory_input_reads_owned_channels_without_a_bundle_and_rejects_wrong_seed() {
        let (_, mut project) = fixture();
        project.targets.clear();
        let (hr, n) = native_layers::dimensions(project.size).unwrap();
        let layers = native_layers::Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16711; hr * hr],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![0; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut maps =
            papyrus_terrain::monument_composition::compose(&layers, &vec![255; n * n], &[], || false)
                .unwrap()
                .unwrap();
        let layout = papyrus_terrain::placement::Solution {
            outcomes: vec![],
            required_missing: 0,
            attempts: 0,
        };
        let terrain = Terrain::from_composed(&project, &maps, &layout, &vec![0; n * n], || false)
            .unwrap()
            .unwrap();
        let (copy, alpha) = terrain
            .bed_layers(Path::new("does-not-exist"), || false)
            .unwrap();
        assert_eq!(copy.height, maps.layers.height);
        assert_eq!(alpha, maps.alpha);
        assert_eq!(terrain.placement_subset().0.len(), n * n);
        maps.layers.terrain_seed += 1;
        assert!(
            Terrain::from_composed(&project, &maps, &layout, &vec![0; n * n], || false).is_err()
        );
    }
    fn fixture() -> (tempfile::TempDir, Project) {
        let root = tempfile::tempdir().unwrap();
        let mut profile: crate::native_profile::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-0f703d43.json"
        ))
        .unwrap();
        profile.native_sha256 = "a".repeat(64);
        let project = Project {
            size: 1000,
            preview_engine: native_preview::VERSION.into(),
            native_profile: Some(profile),
            installation: Some(Installation {
                kind: InstallationKind::Client,
                native: SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let (h, n) = native_layers::dimensions(project.size).unwrap();
        let height = 19660i16.to_le_bytes().repeat(h * h); // approximately 100 m
        let topo = 0i32.to_le_bytes().repeat(n * n);
        let mut files = BTreeMap::new();
        for (name, bytes) in [
            ("project.mapper", serde_json::to_vec(&project).unwrap()),
            ("height.i16le", height),
            ("topology.i32le", topo),
            ("decoration-blocked.u8", vec![0; n * n]),
        ] {
            std::fs::write(root.path().join(name), &bytes).unwrap();
            files.insert(
                name,
                json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}),
            );
        }
        std::fs::write(root.path().join("manifest.json"),serde_json::to_vec(&json!({
            "format":"papyrus-preview-bundle","schema":1,"playable":false,"composed":true,"size":1000,
            "height_resolution":h,"surface_resolution":n,
            "height_encoding":"signed i16 normalized by 32766 over -500..500 metres",
            "grid_rows":"world -Z to +Z; planar channels; little-endian words","files":files})).unwrap()).unwrap();
        (root, project)
    }
    #[test]
    fn saved_road_reports_bind_ordered_layout_identity_and_preserve_legacy_engines() {
        use crate::{road_generation, source_road_handoff};
        use papyrus_terrain::road_network;
        let (root, project) = fixture();
        let mut terrain = Terrain::load(root.path(), &project, || false).unwrap();
        let write_report = |terrain: &mut Terrain, value: &Value| {
            let raw = serde_json::to_vec(value).unwrap();
            std::fs::write(root.path().join("roads.json"), &raw).unwrap();
            terrain.manifest_files["roads.json"] = json!({
                "bytes":raw.len(), "sha256":format!("{:x}", Sha256::digest(&raw)),
            });
        };
        let mut recipe: crate::infrastructure::Recipe = serde_json::from_value(json!({
            "engine":road_network::COST_VERSION, "source_sha256":"b".repeat(64),
            "maximum_grade":0.2, "maximum_cut_fill":8.,
        }))
        .unwrap();
        for (engine, handoff) in [
            (road_generation::VERSION, source_road_handoff::VERSION),
            (
                road_generation::SOURCE_VERSION,
                source_road_handoff::STRIP_CHECKED_VERSION,
            ),
            (
                road_generation::RAW_SOURCE_VERSION,
                source_road_handoff::RAW_VERSION,
            ),
            (
                road_generation::SOURCE_COUPLED_VERSION,
                source_road_handoff::RAW_VERSION,
            ),
            (
                road_generation::CONNECTED_SOURCE_VERSION,
                source_road_handoff::JUNCTION_CHECKED_VERSION,
            ),
            (
                road_generation::CONNECTED_COUPLED_VERSION,
                source_road_handoff::JUNCTION_CHECKED_VERSION,
            ),
        ] {
            recipe.staged = Some(road_generation::Policy {
                engine: engine.into(),
                roadside_source_sha256: "c".repeat(64),
                roadside_preferences: Vec::new(),
                layout: Some(crate::source_road_planning::Policy {
                    engine: crate::source_road_planning::VERSION.into(),
                    assets_sha256: "d".repeat(64),
                    roles_sha256: "e".repeat(64),
                    ring_clearance: None,
                    terrain_entrances: None,
                }),
            });
            terrain.project.roads = Some(recipe.clone());
            let value = json!({"engine":road_network::FAMILY_VERSION,"families":[{}],
                "source_layout":{"engine":handoff,"source_sha256":recipe.source_sha256,
                "assets_sha256":"d".repeat(64),"roles_sha256":"e".repeat(64)}});
            write_report(&mut terrain, &value);
            assert_eq!(terrain.road_report(root.path(), || false).unwrap(), value);
            assert!(terrain.road_report(root.path(), || true).is_err());
            for pointer in [
                "/engine",
                "/source_layout/engine",
                "/source_layout/source_sha256",
                "/source_layout/assets_sha256",
                "/source_layout/roles_sha256",
            ] {
                let mut changed = value.clone();
                *changed.pointer_mut(pointer).unwrap() = "foreign".into();
                write_report(&mut terrain, &changed);
                assert!(
                    terrain.road_report(root.path(), || false).is_err(),
                    "{engine} {pointer}"
                );
            }
            let mut empty = value;
            empty.as_object_mut().unwrap().remove("families");
            empty["engine"] = road_network::NETWORK_VERSION.into();
            write_report(&mut terrain, &empty);
            assert!(terrain.road_report(root.path(), || false).is_ok());
        }
        recipe.staged = None;
        terrain.project.roads = Some(recipe.clone());
        let legacy = json!({"engine":recipe.engine});
        write_report(&mut terrain, &legacy);
        assert_eq!(terrain.road_report(root.path(), || false).unwrap(), legacy);
        write_report(
            &mut terrain,
            &json!({"engine":road_network::FAMILY_VERSION}),
        );
        assert!(terrain.road_report(root.path(), || false).is_err());
        write_report(&mut terrain, &legacy);
        std::fs::write(root.path().join("roads.json"), b"{}").unwrap();
        assert!(terrain.road_report(root.path(), || false).is_err());
        terrain.project.roads = None;
        write_report(&mut terrain, &legacy);
        assert!(terrain.road_report(root.path(), || false).is_err());
    }
    #[test]
    fn saved_river_collider_heights_require_receipts_and_finite_values() {
        let (root, mut project) = fixture();
        project.rivers = Some(crate::river_generation::Recipe {
            engine: papyrus_terrain::river_pipeline::VERSION.into(),
            source_sha256: "b".repeat(64),
            source_document: None,
            objects: None,
        });
        let manifest_path = root.path().join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let saved = serde_json::to_vec(&project).unwrap();
        std::fs::write(root.path().join("project.mapper"), &saved).unwrap();
        manifest["files"]["project.mapper"] = json!({
            "bytes":saved.len(), "sha256":format!("{:x}",Sha256::digest(&saved)),
        });
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
        manifest["rivers"] = json!({"collider_heights":"river-collider-heights.f32le",
            "collider_no_hit_height":0., "collider_sampling":"nearest cell-centre triangle proxy"});
        let (_, n) = native_layers::dimensions(project.size).unwrap();
        let path = root.path().join("river-collider-heights.f32le");
        let good = (-0.5_f32).to_le_bytes().repeat(n * n);
        let mut bad = good.clone();
        bad[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        for bytes in [&good, &bad] {
            std::fs::write(&path, bytes).unwrap();
            manifest["files"]["river-collider-heights.f32le"] = json!({
                "bytes":bytes.len(), "sha256":format!("{:x}",Sha256::digest(bytes)),
            });
            std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            let loaded = Terrain::load(root.path(), &project, || false);
            if bytes == &good {
                assert!(
                    loaded
                        .unwrap()
                        .bed_water()
                        .unwrap()
                        .iter()
                        .all(|&h| h == -0.5)
                );
            } else {
                assert!(loaded.is_err());
            }
        }
        // Same-length corruption also fails the manifest receipt.
        std::fs::write(path, good).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
    }

    #[test]
    fn bed_inputs_require_explicit_channels_and_immutable_layer_receipts() {
        let (root, project) = fixture();
        let terrain = Terrain::load(root.path(), &project, || false).unwrap();
        assert!(terrain.bed_layers(root.path(), || false).is_err());
        let path = root.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["biome_channels"] = 5.into();
        manifest["splat_channels"] = 8.into();
        let cells = terrain.n * terrain.n;
        for (name, channels) in [("biome.u8", 5), ("splat.u8", 8), ("alpha.u8", 1)] {
            let mut bytes = vec![0; cells * channels];
            bytes[..cells].fill(255);
            std::fs::write(root.path().join(name), &bytes).unwrap();
            manifest["files"][name] =
                json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))});
        }
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let terrain = Terrain::load(root.path(), &project, || false).unwrap();
        let (layers, alpha) = terrain.bed_layers(root.path(), || false).unwrap();
        assert_eq!(layers.biome.len(), cells * 5);
        assert!(alpha.iter().all(|&v| v == 255));
        assert!(terrain.bed_layers(root.path(), || true).is_err());
        // Same-length corruption must fail even after the planning grids loaded.
        for name in ["splat.u8", "biome.u8", "alpha.u8"] {
            let path = root.path().join(name);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[0] ^= 1;
            std::fs::write(&path, &bytes).unwrap();
            assert!(terrain.bed_layers(root.path(), || false).is_err());
            bytes[0] ^= 1;
            std::fs::write(&path, &bytes).unwrap();
        }
    }
    #[test]
    fn final_terrain_planning_is_deterministic_read_only_and_explicitly_incomplete() {
        let (root, project) = fixture();
        let before = std::fs::read(root.path().join("project.mapper")).unwrap();
        let loaded = Terrain::load(root.path(), &project, || false).unwrap();
        let profile: Value = serde_json::from_str(include_str!(
            "../../../compatibility/rails/client-0f703d43.json"
        ))
        .unwrap();
        let policy = serde_json::from_value(profile["costmap"].clone()).unwrap();
        let grid = loaded.grid(&policy, || false).unwrap().unwrap();
        let query = Query {
            start: [-300., 0.],
            end: [300., 0.],
            settings: rail_costmap::RouteSettings {
                seed: 7,
                maximum_grade: 0.1,
                maximum_expansions: 100000,
            },
        };
        let report = loaded.audit(&grid, Some(&query), || false).unwrap();
        assert_eq!(report["route"]["result"]["status"], "found");
        assert_eq!(report["costmap"]["resolution"], 133);
        assert_eq!(report["costmap"]["counts"]["normal"], 133 * 133);
        assert_eq!(report["generation_ready"], false);
        assert_eq!(report["terrain_applied"], false);
        assert_eq!(report["continuous_curve_validated"], false);
        assert_eq!(report, loaded.audit(&grid, Some(&query), || false).unwrap());
        assert_eq!(
            before,
            std::fs::read(root.path().join("project.mapper")).unwrap()
        );
        assert!(loaded.audit(&grid, Some(&query), || true).is_err());
    }
    #[test]
    fn stale_recipe_corruption_missing_blockers_bad_conventions_and_cancel_fail() {
        let (root, mut project) = fixture();
        assert!(Terrain::load(root.path(), &project, || true).is_err());
        project.size = 1001;
        assert!(Terrain::load(root.path(), &project, || false).is_err());
        project.size = 1000;
        let path = root.path().join("height.i16le");
        let good = std::fs::read(&path).unwrap();
        let mut bad = good.clone();
        bad[0] ^= 1;
        std::fs::write(&path, bad).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
        std::fs::write(path, good).unwrap();
        std::fs::remove_file(root.path().join("decoration-blocked.u8")).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
        std::fs::write(
            root.path().join("decoration-blocked.u8"),
            vec![0; 512 * 512],
        )
        .unwrap();
        let manifest = root.path().join("manifest.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        v["grid_rows"] = "other encoding".into();
        std::fs::write(manifest, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
    }
    #[test]
    fn accepted_world_rail_sockets_include_inactive_children_and_verify_layout_receipts() {
        let (root, mut project) = fixture();
        let (catalog, mut infra) = crate::infrastructure::tests::fixture();
        let prefab = catalog.entries.keys().next().unwrap().clone();
        infra["prefabs"][&prefab]["connectors"][0]["infrastructureType"] = 6.into();
        infra["prefabs"][&prefab]["connectors"][0]["hierarchyActiveWithoutRoot"] = false.into();
        let infra_path = root.path().join("infrastructure.json");
        std::fs::write(&infra_path, serde_json::to_vec(&infra).unwrap()).unwrap();
        let infrastructure =
            crate::infrastructure::Source::load(&infra_path, &catalog, || false).unwrap();
        project.targets.clear();
        project
            .add_target("Rail test".into(), prefab.clone())
            .unwrap();
        project.monument_assets = Some(crate::AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: "a".repeat(64),
        });
        let layout = json!({"engine":project.placement_engine,"seed":project.seeds[4].value,
            "outcomes":[{"id":project.targets[0].instance_id,"position":{"x":10.,"y":20.,"z":30.,"yaw":90.}}]});
        let manifest_path = root.path().join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        for (name, bytes) in [
            ("project.mapper", serde_json::to_vec(&project).unwrap()),
            ("layout.json", serde_json::to_vec(&layout).unwrap()),
        ] {
            std::fs::write(root.path().join(name), &bytes).unwrap();
            manifest["files"][name] =
                json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))});
        }
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let terrain = Terrain::load(root.path(), &project, || false).unwrap();
        let first = terrain
            .sockets(root.path(), &project, &infrastructure, 6, || false)
            .unwrap();
        assert_eq!(first.sockets.len(), 1);
        let monuments = terrain
            .monuments(root.path(), &infrastructure, 6, || false)
            .unwrap();
        assert_eq!(monuments.poses.len(), 1);
        assert_eq!(monuments.layout_sha256, first.layout_sha256);
        assert_eq!(monuments.poses[0].position, [10., 20., 30.]);
        assert_eq!(monuments.poses[0].yaw, 90.);
        let socket = &first.sockets[0];
        assert!(
            socket
                .position
                .iter()
                .zip([7., 22., 31.])
                .all(|(a, b)| (*a - b).abs() < 0.0001)
        );
        assert!(
            socket
                .forward
                .iter()
                .zip([-1., 0., 0.])
                .all(|(a, b)| (*a - b).abs() < 0.0001)
        );
        // Arbitrary yaw uses the same parent transform for position and forward.
        let v = yaw_vector([2., 0., 0.], 37.);
        assert!((v[0] - 2. * 37f32.to_radians().cos()).abs() < 0.0001);
        assert!((v[2] - 2. * 37f32.to_radians().sin()).abs() < 0.0001);
        assert!(
            terrain
                .sockets(root.path(), &project, &infrastructure, 6, || true)
                .is_err()
        );
        std::fs::write(root.path().join("layout.json"), b"{}").unwrap();
        assert!(
            terrain
                .sockets(root.path(), &project, &infrastructure, 6, || false)
                .is_err()
        );
    }
    #[test]
    fn only_explicit_disabled_decoration_can_supply_known_empty_blockers() {
        let (root, project) = fixture();
        let path = root.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["files"]
            .as_object_mut()
            .unwrap()
            .remove("decoration-blocked.u8");
        std::fs::remove_file(root.path().join("decoration-blocked.u8")).unwrap();
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
        manifest["decoration"] = Value::Null;
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let loaded = Terrain::load(root.path(), &project, || false).unwrap();
        assert!(loaded.placement.iter().all(|&v| v == 0));
        assert_eq!(loaded.placement_origin, "explicit-no-decoration");
        manifest["decoration"] = json!({"unexpected":true});
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(Terrain::load(root.path(), &project, || false).is_err());
    }
}
