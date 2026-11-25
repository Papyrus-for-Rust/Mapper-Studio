//! Opt-in, data-only road-fit captures for local numerical diagnosis.
use papyrus_terrain::{path_mesh, path_terrain, road_bed, road_grade};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};

const FORMAT: &str = "mapper-private-road-fit-capture-1";
const RETAINED_FORMAT: &str = "mapper-private-road-fit-capture-2";
const LIMIT: u64 = 128 * 1024 * 1024;
/// Capture the exact numeric inputs before the initial sampled-control proposal.
/// Separate from mesh feedback captures, which happen only after this succeeds.
#[allow(clippy::too_many_arguments)]
pub fn capture_initial_grade_if_requested(
    input: papyrus_terrain::road_grade_terrain::Input<'_>,
    settings: road_bed::Settings,
    maximum_work: usize,
    coupled_junctions: bool,
    retained_samples: Option<&[usize]>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(destination) = std::env::var_os("MAPPER_ROAD_INITIAL_GRADE_CAPTURE") else {
        return Ok(true);
    };
    if cancelled() {
        return Ok(false);
    }
    capture_initial_grade(
        Path::new(&destination),
        input,
        settings,
        maximum_work,
        coupled_junctions,
        retained_samples,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn capture_initial_grade(
    destination: &Path,
    input: papyrus_terrain::road_grade_terrain::Input<'_>,
    settings: road_bed::Settings,
    maximum_work: usize,
    coupled_junctions: bool,
    retained_samples: Option<&[usize]>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    if destination.exists() {
        return Err("Initial grade capture already exists".into());
    }
    if !(2..=4097).contains(&input.grid.resolution)
        || !(2..=2048).contains(&input.surface_resolution)
        || input.grid.height.len() != input.grid.resolution.pow(2)
        || input.topology.len() != input.surface_resolution.pow(2)
        || input.roads.len() > 4096
        || input
            .roads
            .iter()
            .map(|r| r.path.points().len())
            .sum::<usize>()
            > 131072
        || input.grid.height.len() > 4097 * 4097
        || input.ground.len() != input.grid.height.len()
        || input.topology.len() > 2048 * 2048
        || retained_samples.is_some_and(|samples| {
            samples.len() > 262144
                || samples.iter().any(|&i| i >= input.grid.height.len())
                || samples.windows(2).any(|pair| pair[0] >= pair[1])
        })
    {
        return Err("Initial grade capture exceeds limits".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let private = tempfile::Builder::new()
        .prefix(".initial-grade-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    for (name, words, width) in [
        ("height.i16le", input.grid.height.len(), 2),
        ("ground.i16le", input.ground.len(), 2),
        ("topology.i32le", input.topology.len(), 4),
    ] {
        let mut raw = Vec::with_capacity(words * width);
        for start in (0..words).step_by(4096) {
            if cancelled() {
                return Ok(false);
            }
            let end = (start + 4096).min(words);
            match name {
                "height.i16le" => raw.extend(
                    input.grid.height[start..end]
                        .iter()
                        .flat_map(|v| v.to_le_bytes()),
                ),
                "ground.i16le" => raw.extend(
                    input.ground[start..end]
                        .iter()
                        .flat_map(|v| v.to_le_bytes()),
                ),
                _ => raw.extend(
                    input.topology[start..end]
                        .iter()
                        .flat_map(|v| v.to_le_bytes()),
                ),
            }
        }
        if raw.len() as u64 > LIMIT {
            return Err("Initial grade buffer exceeds limit".into());
        }
        fs::write(private.path().join(name), &raw).map_err(|e| e.to_string())?;
        files.insert(
            name,
            Receipt {
                bytes: raw.len(),
                sha256: hash(&raw),
            },
        );
    }
    let mut roads = Vec::new();
    for road in input.roads {
        let mut grounds = Vec::new();
        let mut fixed = Vec::new();
        for &p in road.path.points() {
            if cancelled() {
                return Ok(false);
            }
            grounds.push(path_terrain::sample_height(
                path_terrain::Grid {
                    height: input.ground,
                    ..input.grid
                },
                p,
            )?);
            let n = input.surface_resolution;
            let [x, z] = [p[0], p[2]].map(|v| {
                (((v + input.grid.size * 0.5) * (1. / input.grid.size)) * n as f32)
                    .clamp(0., (n - 1) as f32) as usize
            });
            fixed.push(input.topology[z * n + x] & input.protected_topology != 0);
        }
        roads.push(
            serde_json::json!({"points":road.path.points(),"ground":grounds,"fixed_topology":fixed,
            "radius":road.radius,"shoulder":road.shoulder,"closed":road.path.is_circular()}),
        );
    }
    let quantum = path_terrain::encoded_height_quantum(path_terrain::Layout {
        size: input.grid.size,
        resolution: input.grid.resolution,
        origin_y: input.grid.origin_y,
        vertical_span: input.grid.vertical_span,
    })?;
    let mut doc = serde_json::json!({"format":if retained_samples.is_some() {"mapper-initial-road-grade-capture-2"} else {"mapper-initial-road-grade-capture-1"},"size":input.grid.size,"coupled_junctions":coupled_junctions,
        "resolution":input.grid.resolution,"origin_y":input.grid.origin_y,"vertical_span":input.grid.vertical_span,
        "surface_resolution":input.surface_resolution,"protected_topology":input.protected_topology,
        "settings":settings,"maximum_initial_work":maximum_work,
        "height_reserve":quantum*4.,
        "roads":roads,"junctions":input.junctions.iter().map(|j|[j.a,j.b]).collect::<Vec<_>>(),"files":files});
    if let Some(samples) = retained_samples {
        doc["retained_samples"] = serde_json::json!(samples);
    }
    let raw = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    if raw.len() as u64 > LIMIT {
        return Err("Initial grade metadata exceeds limit".into());
    }
    fs::write(private.path().join("capture.json"), raw).map_err(|e| e.to_string())?;
    if cancelled() {
        return Ok(false);
    }
    fs::rename(private.path(), destination).map_err(|e| e.to_string())?;
    Ok(true)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    bytes: usize,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Collision {
    road: usize,
    chunk_index: usize,
    geometry: path_mesh::Chunk,
    template: path_mesh::Template,
    heights: Vec<path_mesh::HeightBinding>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    format: String,
    round: usize,
    size: f32,
    resolution: usize,
    origin_y: f32,
    vertical_span: f32,
    surface_resolution: usize,
    protected_topology: i32,
    allowed_topology: i32,
    settings: road_bed::Settings,
    controls: Vec<Vec<[f32; 3]>>,
    junctions: Vec<[(usize, usize); 2]>,
    collisions: Vec<Collision>,
    #[serde(deserialize_with = "exact_probes")]
    probes: Vec<road_bed::ClearanceProbe>,
    frozen_controls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retained: Option<Vec<(usize, i16)>>,
    files: BTreeMap<String, Receipt>,
}
// The ordinary fast JSON parser can round distinct adjacent f64 witnesses to
// the same value. Preserve their original decimal tokens and use Rust's exact
// float parser locally, without changing installation/cache decoding globally.
fn exact_probes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<road_bed::ClearanceProbe>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Probe {
        road: usize,
        chunk: usize,
        triangle: usize,
        position: [Box<serde_json::value::RawValue>; 2],
        maximum_terrain_excess: f32,
    }
    Vec::<Probe>::deserialize(deserializer)?
        .into_iter()
        .map(|p| {
            let parse = |v: &serde_json::value::RawValue| {
                v.get().parse::<f64>().map_err(serde::de::Error::custom)
            };
            Ok(road_bed::ClearanceProbe {
                road: p.road,
                chunk: p.chunk,
                triangle: p.triangle,
                position: [parse(&p.position[0])?, parse(&p.position[1])?],
                maximum_terrain_excess: p.maximum_terrain_excess,
            })
        })
        .collect()
}
fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() as u64 > LIMIT {
        return Err("Capture file exceeds allowance".into());
    }
    Ok(raw)
}
/// Capture environment variables are developer-only and inactive without this
/// Cargo feature. Destination must be new; incomplete captures are discarded.
pub fn capture_if_requested(
    round: usize,
    input: road_bed::Input<'_>,
    settings: road_bed::Settings,
    probes: &[road_bed::ClearanceProbe],
    envelope: road_bed::PaintEnvelope<'_>,
    frozen_controls: bool,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    capture_with_retained_if_requested(
        round,
        input,
        settings,
        probes,
        envelope,
        frozen_controls,
        None,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn capture_with_retained_if_requested(
    round: usize,
    input: road_bed::Input<'_>,
    settings: road_bed::Settings,
    probes: &[road_bed::ClearanceProbe],
    envelope: road_bed::PaintEnvelope<'_>,
    frozen_controls: bool,
    retained: Option<&crate::retained_rail_height::RetainedHeight>,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    if retained.is_none() && std::env::var_os("MAPPER_RETAINED_ROAD_FIT_CAPTURE").is_some() {
        return Err("Retained capture requires original height constraints".into());
    }
    let key = if retained.is_some() {
        "MAPPER_RETAINED_ROAD_FIT_CAPTURE"
    } else {
        "MAPPER_ROAD_FIT_CAPTURE"
    };
    let Some(destination) = std::env::var_os(key) else {
        return Ok(());
    };
    if envelope.source_rounding.is_some() {
        return Err("Legacy road-fit capture cannot omit source quantization constraints".into());
    }
    let selected: usize = std::env::var(if retained.is_some() {
        "MAPPER_RETAINED_ROAD_FIT_CAPTURE_ROUND"
    } else {
        "MAPPER_ROAD_FIT_CAPTURE_ROUND"
    })
    .unwrap_or_else(|_| {
        if retained.is_some() {
            "0".into()
        } else {
            "2".into()
        }
    })
    .parse()
    .map_err(|_| "Invalid capture round")?;
    if round != selected {
        return Ok(());
    }
    capture(
        Path::new(&destination),
        round,
        input,
        settings,
        probes,
        envelope,
        frozen_controls,
        retained,
        cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
fn capture(
    destination: &Path,
    round: usize,
    input: road_bed::Input<'_>,
    settings: road_bed::Settings,
    probes: &[road_bed::ClearanceProbe],
    envelope: road_bed::PaintEnvelope<'_>,
    frozen_controls: bool,
    retained: Option<&crate::retained_rail_height::RetainedHeight>,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    if cancelled() {
        return Err("Capture cancelled".into());
    }
    if destination.exists() {
        return Err("Capture destination already exists".into());
    }
    if let Some(retained) = retained
        && (!retained.validate_retained_grid(input.grid, &cancelled)?
            || !retained.validate_retained_grid(
                path_terrain::Grid {
                    height: envelope.source_painted,
                    ..input.grid
                },
                &cancelled,
            )?)
    {
        return Err("Retained capture cancelled".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let private = tempfile::Builder::new()
        .prefix(".mapper-road-fit-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    let mut write = |name: &str, raw: Vec<u8>| -> Result<(), String> {
        if cancelled() {
            return Err("Capture cancelled".into());
        }
        if raw.len() as u64 > LIMIT {
            return Err("Capture file exceeds allowance".into());
        }
        fs::write(private.path().join(name), &raw).map_err(|e| e.to_string())?;
        files.insert(
            name.into(),
            Receipt {
                bytes: raw.len(),
                sha256: hash(&raw),
            },
        );
        Ok(())
    };
    for (name, data) in [
        ("height.i16le", input.grid.height),
        ("ground.i16le", input.ground),
        ("painted.i16le", envelope.source_painted),
    ] {
        write(name, data.iter().flat_map(|v| v.to_le_bytes()).collect())?;
    }
    write(
        "topology.i32le",
        input
            .topology
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
    )?;
    let metadata = Capture {
        format: if retained.is_some() {
            RETAINED_FORMAT.into()
        } else {
            FORMAT.into()
        },
        round,
        size: input.grid.size,
        resolution: input.grid.resolution,
        origin_y: input.grid.origin_y,
        vertical_span: input.grid.vertical_span,
        surface_resolution: input.surface_resolution,
        protected_topology: input.protected_topology,
        allowed_topology: envelope.allowed_topology,
        settings,
        controls: input.controls.to_vec(),
        junctions: input.junctions.iter().map(|j| [j.a, j.b]).collect(),
        collisions: input
            .collisions
            .iter()
            .map(|c| Collision {
                road: c.road,
                chunk_index: c.chunk_index,
                geometry: c.geometry.clone(),
                template: c.template.clone(),
                heights: c.heights.to_vec(),
            })
            .collect(),
        probes: probes.to_vec(),
        frozen_controls,
        retained: retained.map(|r| r.captured_samples()),
        files,
    };
    let raw = serde_json::to_vec(&metadata).map_err(|e| e.to_string())?;
    if raw.len() as u64 > LIMIT {
        return Err("Capture metadata exceeds allowance".into());
    }
    let mut file =
        fs::File::create(private.path().join("capture.json")).map_err(|e| e.to_string())?;
    file.write_all(&raw)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    if cancelled() {
        return Err("Capture cancelled".into());
    }
    if destination.exists() {
        return Err("Capture destination already exists".into());
    }
    fs::rename(private.path(), destination).map_err(|e| e.to_string())?;
    Ok(())
}

/// Replay just the captured numerical fit, without installation assets/workers.
/// Returning a candidate does not certify a rebuilt map or playable output.
pub fn replay(
    path: &Path,
    frozen: Option<bool>,
    iterations: Option<usize>,
) -> Result<serde_json::Value, String> {
    replay_inner(path, frozen, iterations, false, false, false, false)
}
pub fn replay_local(path: &Path, iterations: Option<usize>) -> Result<serde_json::Value, String> {
    replay_inner(path, Some(false), iterations, true, false, false, false)
}
pub fn replay_feedback(
    path: &Path,
    iterations: Option<usize>,
) -> Result<serde_json::Value, String> {
    replay_inner(path, Some(false), iterations, true, true, false, false)
}
pub fn replay_mesh_local(
    path: &Path,
    iterations: Option<usize>,
) -> Result<serde_json::Value, String> {
    replay_inner(path, Some(false), iterations, true, false, true, false)
}
pub fn replay_quantized(
    path: &Path,
    iterations: Option<usize>,
) -> Result<serde_json::Value, String> {
    replay_inner(path, Some(false), iterations, false, false, false, true)
}
fn replay_inner(
    path: &Path,
    frozen: Option<bool>,
    iterations: Option<usize>,
    local: bool,
    feedback: bool,
    mesh_local: bool,
    quantized: bool,
) -> Result<serde_json::Value, String> {
    let metadata: Capture =
        serde_json::from_slice(&read(&path.join("capture.json"))?).map_err(|e| e.to_string())?;
    if ![FORMAT, RETAINED_FORMAT].contains(&metadata.format.as_str())
        || (metadata.format == RETAINED_FORMAT) != metadata.retained.is_some()
        || !(2..=4097).contains(&metadata.resolution)
        || !(2..=2048).contains(&metadata.surface_resolution)
        || metadata.files.len() != 4
    {
        return Err("Unsupported capture".into());
    }
    let raw = |name: &str, bytes: usize| -> Result<Vec<u8>, String> {
        let receipt = metadata.files.get(name).ok_or("Missing capture receipt")?;
        let value = read(&path.join(name))?;
        if value.len() != bytes || receipt.bytes != bytes || receipt.sha256 != hash(&value) {
            return Err(format!("Capture receipt differs: {name}"));
        }
        Ok(value)
    };
    let shorts = |name| -> Result<Vec<i16>, String> {
        Ok(raw(name, metadata.resolution * metadata.resolution * 2)?
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect())
    };
    let height = shorts("height.i16le")?;
    let ground = shorts("ground.i16le")?;
    let painted = shorts("painted.i16le")?;
    let topology: Vec<_> = raw(
        "topology.i32le",
        metadata.surface_resolution * metadata.surface_resolution * 4,
    )?
    .chunks_exact(4)
    .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    .collect();
    let collisions: Vec<_> = metadata
        .collisions
        .iter()
        .map(|c| road_bed::Collision {
            road: c.road,
            chunk_index: c.chunk_index,
            geometry: &c.geometry,
            template: &c.template,
            heights: &c.heights,
        })
        .collect();
    let junctions: Vec<_> = metadata
        .junctions
        .iter()
        .map(|j| road_grade::Junction { a: j[0], b: j[1] })
        .collect();
    let input = road_bed::Input {
        grid: path_terrain::Grid {
            size: metadata.size,
            resolution: metadata.resolution,
            origin_y: metadata.origin_y,
            vertical_span: metadata.vertical_span,
            height: &height,
        },
        ground: &ground,
        topology: &topology,
        surface_resolution: metadata.surface_resolution,
        protected_topology: metadata.protected_topology,
        controls: &metadata.controls,
        junctions: &junctions,
        collisions: &collisions,
    };
    let mut settings = metadata.settings;
    if let Some(value) = iterations {
        settings.maximum_iterations = value;
    }
    let envelope = road_bed::PaintEnvelope {
        source_painted: &painted,
        source_rounding: None,
        allowed_topology: metadata.allowed_topology,
    };
    let retained = metadata
        .retained
        .as_ref()
        .map(|samples| {
            crate::retained_rail_height::RetainedHeight::from_capture(
                path_terrain::Grid {
                    height: &painted,
                    ..input.grid
                },
                samples,
                || false,
            )
            .and_then(|r| r.ok_or("Retained replay preparation cancelled".into()))
        })
        .transpose()?;
    if let Some(retained) = retained.as_ref() {
        retained.validate_retained_grid(input.grid, || false)?;
    }
    if feedback {
        let mut report = trace_feedback(input, settings, envelope, retained.as_ref())?;
        if let Some(held) = retained.as_ref() {
            report["retained_samples"] = held.samples().len().into();
            report["retained_constraints_enforced"] = true.into();
        }
        return Ok(report);
    }
    let frozen = frozen.unwrap_or(metadata.frozen_controls);
    let mut last = None;
    let fit = if frozen {
        road_bed::fit_clearance_for_rebuild
    } else {
        road_bed::fit_for_rebuild
    };
    let start = std::time::Instant::now();
    let selected = if mesh_local {
        road_bed::select_mesh_repair_samples(input, settings, &metadata.probes, 128_000_000, || {
            false
        })
        .map_err(|e| format!("Mesh local selection: {e:?}"))?
        .samples
    } else if local {
        road_bed::select_clearance_samples(input, &metadata.probes, 128_000_000, || false)
            .map_err(|e| format!("Local selection: {e:?}"))?
            .samples
    } else {
        Vec::new()
    };
    let output = if let Some(retained) = retained.as_ref() {
        road_bed::fit_retaining_for_rebuild(
            input,
            settings,
            &metadata.probes,
            road_bed::RebuildPass {
                envelope,
                mode: if quantized {
                    road_bed::RebuildMode::Quantized
                } else if local {
                    road_bed::RebuildMode::Local(&selected)
                } else if frozen {
                    road_bed::RebuildMode::FrozenControls
                } else {
                    road_bed::RebuildMode::Feedback
                },
            },
            retained.samples(),
            |p| last = Some(p),
            || false,
        )
    } else if quantized {
        road_bed::fit_quantized_for_rebuild(
            input,
            settings,
            &metadata.probes,
            envelope,
            |p| last = Some(p),
            || false,
        )
    } else if local {
        road_bed::fit_local_for_rebuild(
            input,
            settings,
            &metadata.probes,
            envelope,
            &selected,
            |p| last = Some(p),
            || false,
        )
    } else {
        fit(
            input,
            settings,
            &metadata.probes,
            envelope,
            |p| last = Some(p),
            || false,
        )
    };
    let mut result = match output {
        Ok(candidate) => {
            if let Some(retained) = retained.as_ref() {
                retained.validate_retained_grid(
                    path_terrain::Grid {
                        height: &candidate.height,
                        ..input.grid
                    },
                    || false,
                )?;
            }
            let clearance = trace_clearance(input, &candidate)?;
            serde_json::json!({"format":"mapper-private-road-fit-replay-1","round":metadata.round,"frozen_controls":frozen,"local_repair":local,"mesh_chunk_selection":mesh_local,"selected_samples":selected.len(),"elapsed_ms":start.elapsed().as_millis(),"mesh_fit_passed":candidate.mesh_fit_passed,"controls_unchanged":candidate.controls==metadata.controls,"report":candidate.report,"last_progress":last,"trace_rebuild_clearance":clearance,"rebuilt_map_validated":false})
        }
        Err(error) => {
            serde_json::json!({"format":"mapper-private-road-fit-replay-1","round":metadata.round,"frozen_controls":frozen,"local_repair":local,"mesh_chunk_selection":mesh_local,"selected_samples":selected.len(),"elapsed_ms":start.elapsed().as_millis(),"error":error,"last_progress":last,"rebuilt_map_validated":false})
        }
    };
    if let Some(held) = retained.as_ref() {
        result["format"] = "mapper-private-road-fit-replay-2".into();
        result["retained_samples"] = held.samples().len().into();
        result["retained_constraints_enforced"] = true.into();
        if result["mesh_fit_passed"].is_null() {
            result["mesh_fit_passed"] = false.into();
        }
    }
    Ok(result)
}
fn trace_geometry(
    input: road_bed::Input<'_>,
    height: &[i16],
) -> Result<crate::road_surface_stage::TracedOutput, String> {
    let grid = path_terrain::Grid {
        height,
        ..input.grid
    };
    let mut chunks = Vec::new();
    let mut traces = Vec::new();
    let mut work = 0;
    for collision in input.collisions {
        let mut geometry = collision.geometry.clone();
        for (vertex, binding) in geometry.mesh.vertices.iter_mut().zip(collision.heights) {
            vertex[1] = binding
                .resolve(|x, z| path_terrain::sample_height(grid, [x, 0., z]).ok())
                .ok_or("Trace rebuild height failed")?
                - geometry.origin[1];
        }
        let lo = geometry
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = geometry
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        geometry.mesh.center[1] = (lo + hi) * 0.5;
        geometry.mesh.extents[1] = (hi - lo) * 0.5;
        work += geometry.mesh.vertices.len() + geometry.mesh.triangles.len();
        chunks.push(papyrus_terrain::road_network::SurfaceChunk {
            road: collision.road,
            lod: 0,
            chunk_index: collision.chunk_index,
            geometry,
        });
        traces.push(crate::road_surface_stage::HeightTrace {
            road: collision.road,
            chunk_index: collision.chunk_index,
            heights: collision.heights.to_vec(),
        });
    }
    Ok(crate::road_surface_stage::TracedOutput {
        surfaces: papyrus_terrain::road_network::SurfaceOutput {
            chunks,
            report: serde_json::json!({"work":work}),
        },
        traces,
    })
}
fn trace_feedback(
    input: road_bed::Input<'_>,
    fit: road_bed::Settings,
    envelope: road_bed::PaintEnvelope<'_>,
    retained: Option<&crate::retained_rail_height::RetainedHeight>,
) -> Result<serde_json::Value, String> {
    let template = input.collisions.first().ok_or("No captured mesh")?.template;
    let mut rounds = Vec::new();
    let started = std::time::Instant::now();
    let result = crate::road_refinement::refine_local(
        crate::road_refinement::Input {
            retained_height: retained,
            layout: path_terrain::Layout {
                size: input.grid.size,
                resolution: input.grid.resolution,
                origin_y: input.grid.origin_y,
                vertical_span: input.grid.vertical_span,
            },
            initial_height: input.grid.height,
            ground: input.ground,
            source_painted: envelope.source_painted,
            source_rounding: None,
            topology: input.topology,
            surface_resolution: input.surface_resolution,
            protected_topology: input.protected_topology,
            allowed_topology: envelope.allowed_topology,
            controls: input.controls,
            junctions: input.junctions,
            template,
        },
        crate::road_refinement::Settings {
            fit,
            clearance: papyrus_terrain::road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: fit.minimum_up_normal,
                maximum_work: 10_000_000,
                maximum_probes: 131072,
            },
            maximum_rounds: 32,
            maximum_total_work: 32_000_000_000,
            maximum_reprojection_distance: 0.25,
        },
        |height, controls, _| {
            if controls != input.controls {
                return Err(
                    "Captured trace feedback needs fresh source geometry after controls change"
                        .into(),
                );
            }
            trace_geometry(input, height).map(Some)
        },
        |r| rounds.push(r.clone()),
        || false,
    );
    Ok(match result {
        Ok(Some(output)) => {
            serde_json::json!({"format":"mapper-captured-trace-feedback-1","elapsed_ms":started.elapsed().as_millis(),"passed":true,"report":output.report,"clearance":output.clearance.report,"source_trace_rebuild_only":true,"runtime_physics_validated":false,"full_generation_validated":false})
        }
        other => {
            serde_json::json!({"format":"mapper-captured-trace-feedback-1","elapsed_ms":started.elapsed().as_millis(),"passed":false,"error":match other {Err(e)=>format!("{e:?}"),_=>"cancelled".into()},"rounds":rounds,"source_trace_rebuild_only":true,"runtime_physics_validated":false,"full_generation_validated":false})
        }
    })
}
fn trace_clearance(
    input: road_bed::Input<'_>,
    candidate: &road_bed::RebuildCandidate,
) -> Result<serde_json::Value, String> {
    if candidate.controls != input.controls {
        return Ok(
            serde_json::json!({"applicable":false,"reason":"controls changed; fresh source mesh generation required"}),
        );
    }
    let grid = path_terrain::Grid {
        height: &candidate.height,
        ..input.grid
    };
    let mut continuous = 0;
    let mut source = 0;
    let mut maximum = f64::NEG_INFINITY;
    let mut work = 0;
    for collision in input.collisions {
        let mut geometry = collision.geometry.clone();
        for (vertex, binding) in geometry.mesh.vertices.iter_mut().zip(collision.heights) {
            vertex[1] = binding
                .resolve(|x, z| path_terrain::sample_height(grid, [x, 0., z]).ok())
                .ok_or("Trace rebuild height failed")?
                - geometry.origin[1];
        }
        let lo = geometry
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = geometry
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        geometry.mesh.center[1] = (lo + hi) * 0.5;
        geometry.mesh.extents[1] = (hi - lo) * 0.5;
        let audited = papyrus_terrain::road_clearance::audit(
            papyrus_terrain::road_clearance::Input {
                grid,
                chunk: &geometry,
                template: collision.template,
                road: collision.road,
                chunk_index: collision.chunk_index,
            },
            papyrus_terrain::road_clearance::Settings {
                tolerance: 0.02,
                minimum_up_normal: candidate.report.settings.minimum_up_normal,
                maximum_work: 10_000_000,
                maximum_probes: 131072,
            },
            || false,
        )
        .map_err(|e| format!("Trace clearance audit: {e:?}"))?;
        continuous += audited.report.continuous_intersections;
        source += audited.report.source_sampler_confirmed_intersections;
        maximum = maximum.max(audited.report.maximum_continuous_terrain_excess);
        work += audited.report.work;
    }
    Ok(
        serde_json::json!({"applicable":true,"kind":"captured source height bindings, unchanged controls","continuous_intersections":continuous,"source_sampler_intersections":source,"maximum_continuous_terrain_excess":maximum,"work":work,"passed":continuous==0&&source==0,"runtime_physics_validated":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_grade_capture_preserves_retained_samples_and_transaction_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let height = vec![16383; 9];
        let topology = vec![0; 9];
        let input = || papyrus_terrain::road_grade_terrain::Input {
            grid: path_terrain::Grid {
                size: 1000.,
                resolution: 3,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 3,
            protected_topology: 1024,
            roads: &[],
            junctions: &[],
        };
        let settings = road_bed::Settings {
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            minimum_up_normal: 0.5,
            maximum_iterations: 16,
            maximum_work: 1_000_000,
            quantization_reserve: 0.51,
            reserve_roundoff: true,
        };
        for (name, retained) in [
            ("legacy", None),
            ("retained", Some(&[0, 4, 8][..])),
            ("empty-retained", Some(&[][..])),
        ] {
            let destination = directory.path().join(name);
            assert!(
                capture_initial_grade(
                    &destination,
                    input(),
                    settings,
                    1000,
                    true,
                    retained,
                    || false
                )
                .unwrap()
            );
            let raw = fs::read(destination.join("capture.json")).unwrap();
            let doc: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(
                doc["format"],
                if retained.is_some() {
                    "mapper-initial-road-grade-capture-2"
                } else {
                    "mapper-initial-road-grade-capture-1"
                }
            );
            if let Some(samples) = retained {
                assert_eq!(doc["retained_samples"], serde_json::json!(samples));
            } else {
                assert!(doc.get("retained_samples").is_none());
            }
            for (file, receipt) in doc["files"].as_object().unwrap() {
                let bytes = fs::read(destination.join(file)).unwrap();
                assert_eq!(receipt["bytes"], bytes.len());
                assert_eq!(receipt["sha256"], hash(&bytes));
            }
            assert!(
                capture_initial_grade(
                    &destination,
                    input(),
                    settings,
                    1000,
                    true,
                    retained,
                    || false
                )
                .is_err()
            );
            assert_eq!(fs::read(destination.join("capture.json")).unwrap(), raw);
        }
        for invalid in [&[9][..], &[4, 4][..], &[8, 0][..]] {
            let destination = directory.path().join("invalid");
            assert!(
                capture_initial_grade(
                    &destination,
                    input(),
                    settings,
                    1000,
                    true,
                    Some(invalid),
                    || false
                )
                .is_err()
            );
            assert!(!destination.exists());
        }
        let cancelled = directory.path().join("cancelled");
        assert!(
            !capture_initial_grade(
                &cancelled,
                input(),
                settings,
                1000,
                true,
                Some(&[4]),
                || true
            )
            .unwrap()
        );
        assert!(!cancelled.exists());
    }
    #[test]
    fn adjacent_decimal_witnesses_round_trip_bit_exactly() {
        #[derive(Deserialize)]
        struct Probes {
            #[serde(deserialize_with = "exact_probes")]
            probes: Vec<road_bed::ClearanceProbe>,
        }
        let base = 1446_f64.to_bits();
        let probes: Vec<_> = (0..4096)
            .map(|i| road_bed::ClearanceProbe {
                road: 0,
                chunk: 0,
                triangle: 0,
                position: [f64::from_bits(base + i), -f64::from_bits(base + i)],
                maximum_terrain_excess: 0.02,
            })
            .collect();
        let raw = serde_json::to_string(&serde_json::json!({"probes":probes})).unwrap();
        let decoded: Probes = serde_json::from_str(&raw).unwrap();
        for (before, after) in probes.iter().zip(decoded.probes) {
            assert_eq!(
                before.position.map(f64::to_bits),
                after.position.map(f64::to_bits)
            );
        }
    }
    #[test]
    fn numerical_capture_replays_without_workers_checks_receipts_and_preserves_existing_outputs() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("capture");
        let height = vec![16383; 9];
        let topology = vec![0; 9];
        let controls = vec![vec![[-500., 0., -500.], [500., 0., -500.]]];
        let template = path_mesh::Template {
            vertices: vec![[-100., 0., -100.], [100., 0., -100.], [0., 0., 100.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., 1.]; 3],
            uv: vec![[0.; 2]; 3],
            triangles: vec![[0, 1, 2]],
            center: [0.; 3],
            extents: [100., 0., 100.],
        };
        let chunk = path_mesh::Chunk {
            first_repetition: 0,
            repetitions: 1,
            origin: [0.; 3],
            mesh: template.clone(),
            work: 0,
        };
        let bindings: Vec<_> = template
            .vertices
            .iter()
            .map(|p| path_mesh::HeightBinding::Endpoint {
                position: [p[0], p[2]],
            })
            .collect();
        let collisions = [road_bed::Collision {
            road: 0,
            chunk_index: 0,
            geometry: &chunk,
            template: &template,
            heights: &bindings,
        }];
        let input = || road_bed::Input {
            grid: path_terrain::Grid {
                size: 1000.,
                resolution: 3,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &height,
            },
            ground: &height,
            topology: &topology,
            surface_resolution: 3,
            protected_topology: 1024,
            controls: &controls,
            junctions: &[],
            collisions: &collisions,
        };
        let settings = road_bed::Settings {
            maximum_grade: 0.2,
            maximum_cut_fill: 8.,
            minimum_up_normal: 0.5,
            maximum_iterations: 16,
            maximum_work: 1_000_000,
            quantization_reserve: 0.51,
            reserve_roundoff: true,
        };
        let envelope = road_bed::PaintEnvelope {
            source_painted: &height,
            source_rounding: None,
            allowed_topology: 1024,
        };
        capture(
            &destination,
            2,
            input(),
            settings,
            &[],
            envelope,
            true,
            None,
            || false,
        )
        .unwrap();
        let original = fs::read(destination.join("capture.json")).unwrap();
        for mode in [None, Some(false)] {
            let result = replay(&destination, mode, None).unwrap();
            assert_eq!(result["mesh_fit_passed"], true, "{result}");
            assert_eq!(result["controls_unchanged"], true);
            assert_eq!(result["rebuilt_map_validated"], false);
        }
        assert!(
            capture(
                &destination,
                2,
                input(),
                settings,
                &[],
                envelope,
                true,
                None,
                || false
            )
            .is_err()
        );
        assert_eq!(
            fs::read(destination.join("capture.json")).unwrap(),
            original
        );
        let cancelled = directory.path().join("cancelled");
        assert!(
            capture(
                &cancelled,
                2,
                input(),
                settings,
                &[],
                envelope,
                true,
                None,
                || true
            )
            .is_err()
        );
        assert!(!cancelled.exists());
        let polls = std::cell::Cell::new(0);
        let interrupted = directory.path().join("interrupted");
        assert!(
            capture(
                &interrupted,
                2,
                input(),
                settings,
                &[],
                envelope,
                true,
                None,
                || {
                    polls.set(polls.get() + 1);
                    polls.get() >= 3
                }
            )
            .is_err()
        );
        assert!(!interrupted.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        let retained =
            crate::retained_rail_height::RetainedHeight::test_capture(input().grid, &[4]);
        let protected_path = directory.path().join("retained");
        capture(
            &protected_path,
            0,
            input(),
            settings,
            &[road_bed::ClearanceProbe {
                road: 0,
                chunk: 0,
                triangle: 0,
                position: [0., 0.],
                maximum_terrain_excess: 0.02,
            }],
            envelope,
            true,
            Some(&retained),
            || false,
        )
        .unwrap();
        let bytes = fs::read(protected_path.join("capture.json")).unwrap();
        let mut metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(metadata["format"], RETAINED_FORMAT);
        assert_eq!(metadata["retained"], serde_json::json!([[4, 16383]]));
        for frozen in [None, Some(false), Some(true)] {
            let result = replay(&protected_path, frozen, None).unwrap();
            assert_eq!(result["mesh_fit_passed"], true, "{result}");
        }
        for result in [
            replay_local(&protected_path, None),
            replay_mesh_local(&protected_path, None),
            replay_quantized(&protected_path, None),
        ] {
            let result = result.unwrap();
            if result["selected_samples"] == 0 && result["local_repair"] == true {
                assert_eq!(
                    result["error"]["Invalid"], "settings or buffers",
                    "{result}"
                );
            } else {
                assert_eq!(result["mesh_fit_passed"], true, "{result}");
            }
        }
        assert_eq!(
            replay_feedback(&protected_path, None).unwrap()["passed"],
            true
        );
        metadata["retained"][0][1] = 16384.into();
        fs::write(
            protected_path.join("capture.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        assert!(replay(&protected_path, None, None).is_err());
        metadata["retained"] = serde_json::Value::Null;
        fs::write(
            protected_path.join("capture.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        assert!(replay(&protected_path, None, None).is_err());
        fs::write(protected_path.join("capture.json"), bytes).unwrap();
        let mut raised = height.clone();
        raised[4] += 64;
        let raised_grid = path_terrain::Grid {
            height: &raised,
            ..input().grid
        };
        let all = (0..raised.len()).collect::<Vec<_>>();
        let hard = crate::retained_rail_height::RetainedHeight::test_capture(raised_grid, &all);
        let blocked = directory.path().join("blocked-retained");
        let mut raised_chunk = chunk.clone();
        for (point, binding) in raised_chunk.mesh.vertices.iter_mut().zip(&bindings) {
            point[1] = binding
                .resolve(|x, z| path_terrain::sample_height(raised_grid, [x, 0., z]).ok())
                .unwrap();
        }
        let lo = raised_chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        let hi = raised_chunk
            .mesh
            .vertices
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        raised_chunk.mesh.center[1] = (lo + hi) * 0.5;
        raised_chunk.mesh.extents[1] = (hi - lo) * 0.5;
        let raised_collisions = [road_bed::Collision {
            road: 0,
            chunk_index: 0,
            geometry: &raised_chunk,
            template: &template,
            heights: &bindings,
        }];
        capture(
            &blocked,
            0,
            road_bed::Input {
                grid: raised_grid,
                collisions: &raised_collisions,
                ..input()
            },
            settings,
            &[road_bed::ClearanceProbe {
                road: 0,
                chunk: 0,
                triangle: 0,
                position: [0., 0.],
                maximum_terrain_excess: 0.02,
            }],
            road_bed::PaintEnvelope {
                source_painted: &raised,
                source_rounding: None,
                ..envelope
            },
            true,
            Some(&hard),
            || false,
        )
        .unwrap();
        for result in [
            replay(&blocked, None, None),
            replay(&blocked, Some(false), None),
            replay(&blocked, Some(true), None),
            replay_local(&blocked, None),
            replay_mesh_local(&blocked, None),
            replay_quantized(&blocked, None),
        ] {
            let result = result.unwrap();
            assert_eq!(
                result["mesh_fit_passed"], false,
                "Constraints were lost: {result}"
            );
            assert!(
                result["error"].get("Invalid").is_none(),
                "Fixture must fail a physical constraint: {result}"
            );
            assert_eq!(result["retained_constraints_enforced"], true);
        }
        assert_eq!(replay_feedback(&blocked, None).unwrap()["passed"], false);
        fs::write(destination.join("height.i16le"), [0; 18]).unwrap();
        assert!(
            replay(&destination, None, None)
                .unwrap_err()
                .contains("receipt differs")
        );
    }
}
