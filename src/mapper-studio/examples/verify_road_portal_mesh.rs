//! CONFIG PLANNING_REPORT FIT_CAPTURE ROAD_INDEX start|end NEW_REPORT [CONTROLS_JSON].
//! Rebuilds one captured entrance chunk using the installation's native noise.
use mapper_studio::{
    Project, infrastructure, native_noise, native_profile, prefab_assets,
    road_portal_mesh::{self, Endpoint},
    road_surface_source,
};
use papyrus_terrain::{path_mesh, path_terrain, road_bed, road_clearance};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};
const LIMIT: u64 = 128 * 1024 * 1024;
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostic input exceeds bound".into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Deserialize)]
struct Receipt {
    bytes: usize,
    sha256: String,
}
#[derive(Deserialize)]
struct Collision {
    road: usize,
    chunk_index: usize,
    geometry: path_mesh::Chunk,
}
#[derive(Deserialize)]
struct Capture {
    format: String,
    size: f32,
    resolution: usize,
    surface_resolution: usize,
    origin_y: f32,
    vertical_span: f32,
    files: BTreeMap<String, Receipt>,
    controls: Vec<Vec<[f32; 3]>>,
    collisions: Vec<Collision>,
    protected_topology: i32,
    allowed_topology: i32,
    settings: road_bed::Settings,
    frozen_controls: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    label: String,
    controls: Vec<[f32; 3]>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalSet {
    format: String,
    proposals: Vec<Proposal>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum ProposalInput {
    Single(Vec<[f32; 3]>),
    Batch(ProposalSet),
}
fn cases(raw: Option<&[u8]>, original: &[[f32; 3]]) -> Result<(bool, Vec<Proposal>), String> {
    let baseline = || Proposal {
        label: "captured-baseline".into(),
        controls: original.to_vec(),
    };
    let Some(raw) = raw else {
        return Ok((false, vec![baseline()]));
    };
    match serde_json::from_slice::<ProposalInput>(raw).map_err(|e| e.to_string())? {
        ProposalInput::Single(controls) => Ok((
            false,
            vec![Proposal {
                label: "proposal".into(),
                controls,
            }],
        )),
        ProposalInput::Batch(batch) => {
            if batch.format != "mapper-native-portal-proposal-set-1"
                || batch.proposals.is_empty()
                || batch.proposals.len() > 63
            {
                return Err("Unsupported proposal set or more than 63 proposals".into());
            }
            let mut labels = std::collections::HashSet::from(["captured-baseline".to_owned()]);
            for p in &batch.proposals {
                if p.label.is_empty()
                    || p.label.len() > 80
                    || p.label.chars().any(char::is_control)
                    || !labels.insert(p.label.clone())
                {
                    return Err(
                        "Proposal labels must be unique printable names of at most 80 bytes".into(),
                    );
                }
            }
            let mut out = vec![baseline()];
            out.extend(batch.proposals);
            Ok((true, out))
        }
    }
}
fn exact(a: &[[f32; 3]], b: &[[f32; 3]]) -> bool {
    a.len() == b.len()
        && a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(a, b)| a.to_bits() == b.to_bits())
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !(6..=7).contains(&args.len()) {
        return Err(
            "CONFIG PLANNING_REPORT FIT_CAPTURE ROAD_INDEX start|end NEW_REPORT [CONTROLS_JSON]"
                .into(),
        );
    }
    let output = Path::new(&args[5]);
    if output.exists() {
        return Err("Output must be new".into());
    }
    let config: Value = serde_json::from_slice(&read(Path::new(&args[0]), 4 * 1024 * 1024)?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| -> Result<PathBuf, String> {
        config[key]
            .as_str()
            .map(PathBuf::from)
            .ok_or(format!("Missing {key}"))
    };
    let planning_raw = read(Path::new(&args[1]), LIMIT)?;
    let planning: Value = serde_json::from_slice(&planning_raw).map_err(|e| e.to_string())?;
    let project: Project =
        serde_json::from_value(planning["recipe"].clone()).map_err(|e| e.to_string())?;
    let road = args[3]
        .to_str()
        .ok_or("Invalid road")?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    let endpoint = match args[4].to_str() {
        Some("start") => Endpoint::Start,
        Some("end") => Endpoint::End,
        _ => return Err("Invalid endpoint".into()),
    };
    let capture_root = Path::new(&args[2]);
    let capture_raw = read(&capture_root.join("capture.json"), LIMIT)?;
    let capture: Capture = serde_json::from_slice(&capture_raw).map_err(|e| e.to_string())?;
    if capture.format != "mapper-private-road-fit-capture-1"
        || !(2..=4097).contains(&capture.resolution)
        || !(2..=2048).contains(&capture.surface_resolution)
        || capture.files.len() != 4
    {
        return Err("Unsupported capture dimensions/format".into());
    }
    let mut rasters = BTreeMap::new();
    let mut topology = Vec::new();
    for name in [
        "height.i16le",
        "ground.i16le",
        "painted.i16le",
        "topology.i32le",
    ] {
        let receipt = capture.files.get(name).ok_or("Missing raster receipt")?;
        let bytes = read(&capture_root.join(name), LIMIT)?;
        let expected = if name == "topology.i32le" {
            capture.surface_resolution * capture.surface_resolution * 4
        } else {
            capture.resolution * capture.resolution * 2
        };
        if bytes.len() != expected || receipt.bytes != expected || receipt.sha256 != hash(&bytes) {
            return Err(format!("Changed capture raster: {name}"));
        }
        if name != "topology.i32le" {
            rasters.insert(
                name,
                bytes
                    .chunks_exact(2)
                    .map(|p| i16::from_le_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>(),
            );
        } else {
            topology = bytes
                .chunks_exact(4)
                .map(|p| i32::from_le_bytes([p[0], p[1], p[2], p[3]]))
                .collect();
        }
    }
    let height = rasters.get("height.i16le").ok_or("Missing height")?;
    let original_controls = capture.controls.get(road).ok_or("Missing road controls")?;
    let families = planning["planning"]["families"]
        .as_array()
        .ok_or("Missing families")?;
    let family = families
        .iter()
        .find(|f| {
            f["first_road"]
                .as_u64()
                .zip(f["road_count"].as_u64())
                .is_some_and(|(first, count)| {
                    road as u64 >= first && (road as u64) < first.saturating_add(count)
                })
        })
        .ok_or("Missing source family")?;
    let width = family["width"].as_f64().ok_or("Missing source width")? as f32;
    let root = path("rust_root")?;
    let installation = project
        .installation
        .as_ref()
        .ok_or("Missing installation")?;
    eprintln!("Verifying installation-bound portal templates");
    let catalog = prefab_assets::load(&root, &path("catalog")?, installation, || false)?;
    let parent = infrastructure::Source::load(&path("infrastructure")?, &catalog, || false)?;
    let source = road_surface_source::Source::read(&path("mesh_source")?, &parent, &|| false)?;
    source.verify_installation(&root, || false)?;
    let recipe = project.roads.as_ref().ok_or("Missing road recipe")?;
    if recipe.source_sha256 != parent.sha256
        || recipe.surface_sha256.as_deref() != Some(source.sha256.as_str())
        || planning["source_sha256"] != parent.sha256
        || capture.size != project.size as f32
        || capture.controls.len()
            != planning["planning"]["roads"]
                .as_array()
                .ok_or("Missing planned roads")?
                .len()
    {
        return Err("Portal recipe, capture or installation source differs".into());
    }
    let profile = native_profile::resolve(&project.terrain_key())?;
    let templates = source.meshes.iter().map(|m| &m.buffer).collect::<Vec<_>>();
    let grid = path_terrain::Grid {
        size: capture.size,
        resolution: capture.resolution,
        origin_y: capture.origin_y,
        vertical_span: capture.vertical_span,
        height,
    };
    let override_raw = args
        .get(6)
        .map(|p| read(Path::new(p), 4 * 1024 * 1024))
        .transpose()?;
    let (batch, cases) = cases(override_raw.as_deref(), original_controls)?;
    if capture.frozen_controls {
        return Err("Frozen-control capture requires a separate fitter mode".into());
    }
    // Validate the complete bounded input before any proposal native calls.
    for case in &cases {
        let controls = &case.controls;
        if controls.len() != original_controls.len()
            || !(2..=65536).contains(&controls.len())
            || !exact(&controls[..1], &original_controls[..1])
            || !exact(
                &controls[controls.len() - 1..],
                &original_controls[original_controls.len() - 1..],
            )
        {
            return Err("Proposal changes control count or authored endpoints".into());
        }
        for (a, b) in controls.iter().zip(original_controls) {
            if a.iter().any(|v| !v.is_finite())
                || (f64::from(a[0]) - f64::from(b[0])).hypot(f64::from(a[2]) - f64::from(b[2]))
                    > f64::from(width)
                || path_terrain::sample_height(grid, *a)
                    .map_err(|e| format!("Proposal height: {e:?}"))?
                    .to_bits()
                    != a[1].to_bits()
            {
                return Err(
                    "Proposal exceeds one source width or is not sampled on the captured revision"
                        .into(),
                );
            }
        }
    }
    let batch_started = Instant::now();
    let mut results = Vec::with_capacity(cases.len());
    for case in &cases {
        let controls = &case.controls;
        let started = Instant::now();
        let measured = road_portal_mesh::measure_traced(
            controls,
            grid,
            road_portal_mesh::Source {
                policy: source.policy(),
                templates: &templates,
            },
            road_portal_mesh::Settings {
                endpoint,
                width,
                maximum_grade: project
                    .roads
                    .as_ref()
                    .ok_or("Missing road recipe")?
                    .maximum_grade,
                maximum_work: 1_000_000,
                clearance: road_clearance::Settings {
                    tolerance: 0.02,
                    minimum_up_normal: 0.5,
                    maximum_work: 1_000_000,
                    maximum_probes: 131072,
                },
            },
            |coordinates| {
                native_noise::sample(
                    &root,
                    &path("worker")?,
                    installation,
                    &profile,
                    coordinates,
                    || false,
                )
            },
            || false,
        )
        .map_err(|e| e.to_string())?
        .ok_or("Cancelled portal measurement")?;
        let original = capture
            .collisions
            .iter()
            .find(|c| c.road == road && c.chunk_index == measured.report.chunk_index);
        let geometry = &measured.geometry;
        let same = original.is_some_and(|original| {
            let previous = &original.geometry;
            geometry.first_repetition == previous.first_repetition
                && geometry.repetitions == previous.repetitions
                && geometry.mesh.triangles == previous.mesh.triangles
                && exact(&[geometry.origin], &[previous.origin])
                && exact(&geometry.mesh.vertices, &previous.mesh.vertices)
        });
        if !same && (override_raw.is_none() || case.label == "captured-baseline") {
            return Err("Portal geometry differs from the captured source mesh".into());
        }
        let mut fit_controls = capture.controls.clone();
        fit_controls[road] = controls.to_vec();
        // Collision/probe road IDs are local to this one-chunk diagnostic. The
        // complete captured control list is retained for exact sampling validation.
        let collisions = [road_bed::Collision {
            road,
            chunk_index: measured.report.chunk_index,
            geometry,
            template: templates[0],
            heights: measured.heights.as_deref().ok_or("Missing height trace")?,
        }];
        let probes = measured
            .probes
            .iter()
            .cloned()
            .map(|mut p| {
                p.road = road;
                p
            })
            .collect::<Vec<_>>();
        let fit_started = Instant::now();
        let fit = road_bed::fit_quantized_for_rebuild(
            road_bed::Input {
                grid,
                ground: rasters.get("ground.i16le").ok_or("Missing ground")?,
                topology: &topology,
                surface_resolution: capture.surface_resolution,
                protected_topology: capture.protected_topology,
                controls: &fit_controls,
                junctions: &[],
                collisions: &collisions,
            },
            road_bed::Settings {
                maximum_work: 40_000_000,
                ..capture.settings
            },
            &probes,
            road_bed::PaintEnvelope {
                source_painted: rasters.get("painted.i16le").ok_or("Missing paint")?,
                source_rounding: None,
                allowed_topology: capture.allowed_topology,
            },
            |_| {},
            || false,
        );
        let fit_result = match fit {
            Ok(out) => json!({"candidate_produced":true,"report":out.report}),
            Err(error) => json!({"candidate_produced":false,"error":error}),
        };
        let result = json!({"format":"mapper-native-portal-mesh-replay-1","label":case.label,"capture_sha256":hash(&capture_raw),
        "planning_sha256":hash(&planning_raw),"mesh_source_sha256":source.sha256,"source_sha256":parent.sha256,
        "raw_file_receipts_verified":true,"installation_verified":true,"native_noise_source_bound":true,
        "milliseconds":started.elapsed().as_secs_f64()*1000.,"road":road,"geometry_exact":same,
        "proposal_sha256":override_raw.as_deref().map(hash),"captured_revision_only":true,
        "height_bindings":measured.heights.as_ref().map(Vec::len),"clearance_probes":probes.len(),
        "fit_milliseconds":fit_started.elapsed().as_secs_f64()*1000.,"fit":fit_result,
        "measurement":measured.report,"full_map_validated":false,"gui_launched":false});
        eprintln!(
            "Portal {}: native clearance {}; fit candidate {}",
            case.label, measured.report.clearance.passed, result["fit"]["candidate_produced"]
        );
        results.push(result);
    }
    let result = if batch {
        json!({"format":"mapper-native-portal-proposal-results-1",
        "proposal_sha256":override_raw.as_deref().map(hash),"cases":results,
        "milliseconds":batch_started.elapsed().as_secs_f64()*1000.,
        "maximum_cases":64,"maximum_fit_work_per_case":40000000,
        "captured_revision_only":true,"full_map_validated":false,"gui_launched":false})
    } else {
        results.remove(0)
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    eprintln!("Portal diagnostic written: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proposal_sets_include_baseline_and_enforce_case_and_label_bounds() {
        let original = [[0., 0., 0.], [1., 0., 1.]];
        let proposal = |label: &str| json!({"label":label,"controls":original});
        let encode = |proposals: Vec<Value>| {
            serde_json::to_vec(
                &json!({"format":"mapper-native-portal-proposal-set-1","proposals":proposals}),
            )
            .unwrap()
        };
        let (batch, rows) = cases(Some(&encode(vec![proposal("turn-20")])), &original).unwrap();
        assert!(batch);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "captured-baseline");
        assert!(exact(&rows[0].controls, &original));
        for invalid in [
            vec![],
            vec![proposal("same"), proposal("same")],
            vec![proposal("captured-baseline")],
            vec![proposal("bad\nlabel")],
            vec![proposal(&"x".repeat(81))],
        ] {
            assert!(cases(Some(&encode(invalid)), &original).is_err());
        }
        assert!(
            cases(
                Some(&encode(
                    (0..64).map(|i| proposal(&format!("p-{i}"))).collect()
                )),
                &original
            )
            .is_err()
        );
        let raw = serde_json::to_vec(&original).unwrap();
        let (batch, rows) = cases(Some(&raw), &original).unwrap();
        assert!(!batch);
        assert_eq!(rows.len(), 1);
        assert!(exact(&rows[0].controls, &original));
    }
}
