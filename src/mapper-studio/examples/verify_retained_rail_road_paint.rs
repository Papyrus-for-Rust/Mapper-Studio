//! CONFIG EXISTING_RAIL_BUNDLE RAIL_SOURCE_JSON NEW_REPORT. Data-only, no worker.
//! Isolates the later road-terrain rail sweeps; never certifies a generated world.
use mapper_studio::{
    Project, infrastructure, prefab_assets, rail_source, retained_rail_height::RetainedRails,
    road_paint_source, road_paint_stage, road_terrain_source,
};
use papyrus_terrain::path_geometry::Path as Curve;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SavedCurve {
    engine: String,
    points: Vec<[f32; 3]>,
    tangents: Vec<[f32; 3]>,
    length: f32,
    step_size: f32,
    circular: bool,
    work: usize,
    min_index: usize,
    max_index: usize,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
// This probe never plans tracks, so it reads only the receipt-covered height
// pass inputs. Legacy bundles need no inferred decoration occupancy declaration.
fn height_inputs(
    bundle: &Path,
    project: &Project,
    manifest: &Value,
) -> Result<papyrus_terrain::native_layers::Layers, String> {
    use std::io::Read;
    let (hr, n) = papyrus_terrain::native_layers::dimensions(project.size)?;
    if manifest["format"] != "papyrus-preview-bundle"
        || manifest["schema"] != 1
        || manifest["composed"] != true
        || manifest["playable"] != false
        || manifest["size"] != project.size
        || manifest["height_resolution"] != hr
        || manifest["surface_resolution"] != n
        || manifest["biome_channels"] != 5
        || manifest["splat_channels"] != 8
        || manifest["grid_rows"] != "world -Z to +Z; planar channels; little-endian words"
        || manifest["height_encoding"] != "signed i16 normalized by 32766 over -500..500 metres"
    {
        return Err("Unsupported height-pass raster declaration".into());
    }
    let checked = |name: &str, length: usize| -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        fs::File::open(bundle.join(name))
            .map_err(|e| e.to_string())?
            .take(length as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() != length
            || manifest["files"][name] != json!({"bytes":bytes.len(),"sha256":hash(&bytes)})
        {
            return Err(format!("Height-pass input receipt/length differs: {name}"));
        }
        Ok(bytes)
    };
    let raw = fs::read(bundle.join("project.mapper")).map_err(|e| e.to_string())?;
    if manifest["files"]["project.mapper"] != json!({"bytes":raw.len(),"sha256":hash(&raw)})
        || serde_json::from_slice::<Project>(&raw).map_err(|e| e.to_string())? != *project
    {
        return Err("Height-pass saved recipe differs".into());
    }
    Ok(papyrus_terrain::native_layers::Layers {
        size: project.size,
        terrain_seed: project.terrain_key().seed,
        biome_seed: project.terrain_key().biome_seed.unwrap_or(0),
        height_resolution: hr,
        surface_resolution: n,
        height: checked("height.i16le", hr * hr * 2)?
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect(),
        topology: checked("topology.i32le", n * n * 4)?
            .chunks_exact(4)
            .map(|v| i32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .collect(),
        biome: checked("biome.u8", n * n * 5)?,
        splat: checked("splat.u8", n * n * 8)?,
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    })
}
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("CONFIG EXISTING_RAIL_BUNDLE RAIL_SOURCE_JSON NEW_REPORT".into());
    }
    let bundle = Path::new(&args[1]);
    let output = Path::new(&args[3]);
    if output.exists()
        || output
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("map"))
    {
        return Err("New JSON report required".into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let resolved_output = parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(output.file_name().ok_or("Missing report filename")?);
    if resolved_output.starts_with(bundle.canonicalize().map_err(|e| e.to_string())?) {
        return Err("Report must be outside the input bundle".into());
    }
    let config: Value = serde_json::from_slice(&fs::read(&args[0]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let path = |key: &str| {
        config[key]
            .as_str()
            .map(Path::new)
            .ok_or(format!("Missing {key}"))
    };
    let project = Project::load(&bundle.join("project.mapper"))?;
    let catalog = prefab_assets::load(
        path("rust_root")?,
        path("catalog")?,
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let parent = infrastructure::Source::load(path("infrastructure")?, &catalog, || false)?;
    let source = rail_source::Source::load(
        path("rust_root")?,
        Path::new(&args[2]),
        &catalog,
        &parent,
        || false,
    )?;
    if project
        .rails
        .as_ref()
        .is_none_or(|r| r.source_sha256 != source.sha256)
        || project
            .roads
            .as_ref()
            .is_none_or(|r| r.source_sha256 != parent.sha256)
    {
        return Err("Source bindings differ from saved rail bundle".into());
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(bundle.join("manifest.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let layers = height_inputs(bundle, &project, &manifest)?;
    use std::io::Read;
    let mut raw = Vec::new();
    fs::File::open(bundle.join("rails.json"))
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 32 * 1024 * 1024
        || manifest["files"]["rails.json"] != json!({"bytes":raw.len(),"sha256":hash(&raw)})
    {
        return Err("Rail path receipt differs".into());
    }
    let saved: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let tracks = saved["paths"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 4096)
        .ok_or("Missing retained rail paths")?;
    let mut paths = Vec::new();
    for track in tracks {
        let saved: SavedCurve =
            serde_json::from_value(track["path"].clone()).map_err(|e| e.to_string())?;
        let path = Curve::from_controls(&saved.points, 32_000_000, || false)
            .map_err(|e| format!("Retained curve: {e:?}"))?
            .ok_or("Cancelled")?;
        let mut rebuilt = serde_json::to_value(&path).map_err(|e| e.to_string())?;
        // Compare the source f32 representation exactly. Comparing parsed JSON
        // f64s instead introduces decimal-parser roundoff unrelated to geometry.
        let mut original = serde_json::to_value(saved).map_err(|e| e.to_string())?;
        rebuilt.as_object_mut().unwrap().remove("work");
        original
            .as_object_mut()
            .ok_or("Invalid saved path")?
            .remove("work");
        if rebuilt != original {
            let differences = rebuilt
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, value)| original[*key] != **value)
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>();
            return Err(format!(
                "Saved retained rail geometry changed while reconstructing track {}: {differences:?}",
                track["id"]
            ));
        }
        paths.push(path);
    }
    let retained = RetainedRails::prepare(&source, &parent, &paths, 32_000_000, || false)?
        .ok_or("Cancelled")?;
    let bed_inputs = paths
        .iter()
        .zip(tracks)
        .map(|(path, saved)| {
            Ok(papyrus_terrain::rail_bed::Track {
                id: saved["id"].as_u64().ok_or("Invalid saved track id")?,
                path,
                preserve_start: saved["preserve_start"]
                    .as_bool()
                    .ok_or("Missing saved start policy")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let bed_policy = source
        .profile
        .bed
        .as_ref()
        .ok_or("Missing source rail bed policy")?;
    let bed_settings = papyrus_terrain::rail_bed::Settings {
        maximum_work: 512_000_000,
        maximum_cut_fill: 24.,
        grade_fit: None,
    };
    let mut alpha = Vec::new();
    fs::File::open(bundle.join("alpha.u8"))
        .map_err(|e| e.to_string())?
        .take(layers.topology.len() as u64 + 1)
        .read_to_end(&mut alpha)
        .map_err(|e| e.to_string())?;
    if alpha.len() != layers.topology.len()
        || manifest["files"]["alpha.u8"] != json!({"bytes":alpha.len(),"sha256":hash(&alpha)})
    {
        return Err("Rail bed alpha receipt differs".into());
    }
    let old_bed = papyrus_terrain::rail_bed::build_with_joins(
        &layers,
        &alpha,
        None,
        &bed_inputs,
        &[],
        bed_policy,
        source.profile.bed_shape()?,
        bed_settings,
        || false,
    )?
    .ok_or("Cancelled")?;
    let deferred_bed = papyrus_terrain::rail_bed::build_terrain_with_joins(
        &layers,
        &alpha,
        None,
        &bed_inputs,
        &[],
        bed_policy,
        source.profile.bed_shape()?,
        bed_settings,
        || false,
    )?
    .ok_or("Cancelled")?;
    if old_bed.layers.height != deferred_bed.layers.height
        || serde_json::to_value(&old_bed.tracks).map_err(|e| e.to_string())?
            != serde_json::to_value(&deferred_bed.tracks).map_err(|e| e.to_string())?
        || deferred_bed.layers.topology != layers.topology
        || deferred_bed.layers.splat != layers.splat
        || deferred_bed.layers.biome != layers.biome
    {
        return Err(
            "Deferred bed changed earthwork/curves or failed to preserve authored channels".into(),
        );
    }
    let deferred_surface_check = json!({"tracks":bed_inputs.len(),
        "same_height_and_curves_as_legacy":true,"authored_topology_splat_biome_preserved":true,
        "legacy_changes":old_bed.report,"deferred_changes":deferred_bed.report,
        "scope":"Unconstrained comparison from already fitted curves and receipt-covered alpha; no water guide; not a complete regeneration"});
    drop(old_bed);
    drop(deferred_bed);
    let paint: road_paint_source::Profile = serde_json::from_str(include_str!(
        "../../../compatibility/infrastructure/client-0f703d43-road-paint.json"
    ))
    .map_err(|e| e.to_string())?;
    let height =
        road_terrain_source::Profile::for_paint(&paint)?.ok_or("Missing road terrain policy")?;
    // Isolated source-order boundary: topology reads the supplied pre-road-height
    // snapshot. It must not be evaluated using the later road-height result.
    let topology = mapper_studio::rail_topology_stage::paint(
        &source,
        &parent,
        &layers,
        &paths,
        mapper_studio::road_surface_stage::HeightGeometry {
            origin_y: paint.height_origin,
            vertical_span: paint.vertical_span,
        },
        512_000_000,
        || false,
    )?
    .ok_or("Cancelled")?;
    let topology_bytes = topology
        .topology
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let input_topology_bytes = layers
        .topology
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let late_topology = json!({"source_stage":topology.report,
        "topology_sha256":hash(&topology_bytes),"input_topology_sha256":hash(&input_topology_bytes),
        "height_snapshot":"input fixture before source road height sweeps",
        "scope":"Isolated late topology over saved rail fixture; no roadside/road-topology coordinator"});
    drop(topology_bytes);
    drop(input_topology_bytes);
    drop(topology);
    let run = |retained| {
        road_paint_stage::paint_with_retained_rails(
            road_paint_stage::Input {
                layers: &layers,
                roads: &[],
                junctions: &[],
                paint: &paint,
                terrain: &height,
            },
            retained,
            512_000_000,
            |p| Ok(Some(vec![0.; p.len()])),
            |_| Err("Rails must not be resampled as road controls".into()),
            || false,
        )?
        .ok_or_else(|| "Cancelled".to_string())
    };
    let baseline = run(None)?;
    let result = run(Some(&retained))?;
    if baseline.topology != result.topology
        || baseline.splat != result.splat
        || !result.roads.is_empty()
    {
        return Err("Retained rail sweep changed road-only outputs".into());
    }
    let changed = result
        .height
        .iter()
        .zip(&baseline.height)
        .filter(|(a, b)| a != b)
        .count();
    let maximum_delta = result
        .height
        .iter()
        .zip(&baseline.height)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).unsigned_abs())
        .max()
        .unwrap_or(0);
    let post_height = papyrus_terrain::native_layers::Layers {
        height: result.height.clone(),
        ..layers
    };
    let support = retained
        .height_support(
            &post_height,
            mapper_studio::road_surface_stage::HeightGeometry {
                origin_y: paint.height_origin,
                vertical_span: paint.vertical_span,
            },
            128_000_000,
            || false,
        )?
        .ok_or("Cancelled")?;
    if !support.validate(&post_height, || false)? {
        return Err("Retained support validation cancelled".into());
    }
    let mut support_hash = Sha256::new();
    for &index in support.samples() {
        support_hash.update((index as u64).to_le_bytes());
    }
    let support_report = json!({"samples":support.samples().len(),"preparation_work":support.preparation_work,"indices_sha256":format!("{:x}",support_hash.finalize()),"post_height_binding_verified":true,"scope":"full Hermite driving strip and bilinear dependencies; excludes wider rail-bed fade"});
    let texture = mapper_studio::rail_texture_stage::paint(
        &source,
        &parent,
        &post_height,
        &paths,
        mapper_studio::road_surface_stage::HeightGeometry {
            origin_y: paint.height_origin,
            vertical_span: paint.vertical_span,
        },
        128_000_000,
        || false,
    )?
    .ok_or("Cancelled")?;
    let late_texture = json!({"source_stage":texture.report,"splat_sha256":hash(&texture.splat),"input_splat_sha256":hash(&post_height.splat),"height_and_topology_borrowed_immutably":true,"scope":"Isolated late texture pass over saved fixture; not a combined generator"});
    let report = json!({"format":"mapper-retained-rail-road-paint-verification-5","bundle":bundle,"input_manifest_sha256":hash(&fs::read(bundle.join("manifest.json")).map_err(|e|e.to_string())?),"retained":retained.receipt(),"changed_height_samples":changed,"maximum_delta_units":maximum_delta,"same_topology_and_splat_as_without_rails":true,"rail_geometry_reconstructed_exactly":true,"zero_noise_valid_because_source_rail_scale_is_one":true,"road_height_report":result.report.height,"retained_height_support":support_report,"deferred_surface_check":deferred_surface_check,"late_topology":late_topology,"late_texture":late_texture,"scope":"Retained rail height contribution, deferred bed channel comparison, fit dependencies and isolated late rail topology/texture passes","normal_combined_generation_enabled":false,"runtime_validated":false,"playable":false});
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, &report).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"tracks":paths.len(),"changed_height_samples":changed,"maximum_delta_units":maximum_delta,"retained_height_samples":support.samples().len(),"late_texture_changed_cells":texture.report.texture.changed_cells,"report":output,"normal_combined_generation_enabled":false})
    );
    Ok(())
}
