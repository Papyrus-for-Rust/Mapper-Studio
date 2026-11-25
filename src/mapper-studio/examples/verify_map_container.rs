//! Candidate version-10 container verification, not a complete/playability claim.
use mapper_studio::{Project, path_export, prefab_export};
use papyrus_terrain::{
    map_container::{self, World},
    native_layers::Layers,
    prefab_records::Record,
};
use sha2::{Digest, Sha256};
use std::{
    io::{BufWriter, Read, Write},
    path::{Path as FilePath, PathBuf},
};
fn json(p: &FilePath) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    std::fs::File::open(p)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 64 * 1024 * 1024 {
        return Err("JSON exceeds 64 MiB".into());
    }
    Ok(serde_json::from_slice(&raw)?)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<_> = std::env::args_os().map(PathBuf::from).collect();
    let road_height = a.len() == 5 && a[4].as_os_str() == "--road-height";
    if a.len() != 4 && !road_height {
        return Err(
            "usage: verify_map_container BUNDLE PREFAB_RECORDS NEW_CANDIDATE_MAP [--road-height]"
                .into(),
        );
    }
    if a[3].exists() {
        return Err("Output already exists".into());
    }
    let m = json(&a[1].join("manifest.json"))?;
    if m["format"] != "papyrus-preview-bundle"
        || m["schema"] != 1
        || m["biome_channels"] != 5
        || m["splat_channels"] != 8
        || m["grid_rows"] != "world -Z to +Z; planar channels; little-endian words"
    {
        return Err(
            "Preview format or native layer layout is not supported by this container adapter"
                .into(),
        );
    }
    let records = json(&a[2])?;
    let bundle_sha = format!(
        "{:x}",
        Sha256::digest(std::fs::read(a[1].join("manifest.json"))?)
    );
    if records["input_manifest_sha256"] != bundle_sha {
        return Err(
            "Prefab records were prepared from a different or unbound preview bundle".into(),
        );
    }
    let size = m["size"].as_u64().ok_or("Missing size")? as u32;
    let (hr, n) = papyrus_terrain::native_layers::dimensions(size)?;
    let bytes = |name: &str| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let raw = std::fs::read(a[1].join(name))?;
        let r = &m["files"][name];
        if r["bytes"] != raw.len() as u64 || r["sha256"] != format!("{:x}", Sha256::digest(&raw)) {
            return Err(format!("Bad {name} receipt").into());
        }
        Ok(raw)
    };
    let shorts = |name: &str| -> Result<Vec<i16>, Box<dyn std::error::Error>> {
        let b = bytes(name)?;
        if b.len() % 2 != 0 {
            return Err("Odd short layer".into());
        }
        Ok(b.chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect())
    };
    let height = shorts("height.i16le")?;
    let water = shorts("local-water.i16le")?;
    let alpha = bytes("alpha.u8")?;
    let topo = bytes("topology.i32le")?;
    if topo.len() % 4 != 0 {
        return Err("Partial topology word".into());
    }
    let l = Layers {
        size,
        terrain_seed: 1,
        biome_seed: 1,
        height_resolution: hr,
        surface_resolution: n,
        height,
        biome: bytes("biome.u8")?,
        splat: bytes("splat.u8")?,
        topology: topo
            .chunks_exact(4)
            .map(|s| i32::from_le_bytes(s.try_into().unwrap()))
            .collect(),
        normalized_biome_cells: 0,
        normalized_splat_cells: 0,
    };
    if records["format"] != "papyrus-prefab-records-1"
        || records["schema"] != 1
        || records["rotation_convention"] != "Unity Euler Z-X-Y, degrees"
    {
        return Err("Unknown prefab record contract".into());
    }
    let prefabs: Vec<Record> = serde_json::from_value(records["records"].clone())?;
    let road_json = bytes("roads.json")?;
    let roads: serde_json::Value = serde_json::from_slice(&road_json)?;
    // The record manifest must cover the current snapshot, including monuments
    // introduced after initial layout. A bound hash alone cannot detect an old
    // record builder that silently omitted the roadside roots.
    bytes("project.mapper")?;
    let project = Project::load(&a[1].join("project.mapper"))?;
    let layout: serde_json::Value = serde_json::from_slice(&bytes("layout.json")?)?;
    let objects: serde_json::Value = serde_json::from_slice(&bytes("objects.json")?)?;
    let mut ids = std::collections::BTreeMap::new();
    for record in &prefabs {
        if let Some(old) = ids.insert(record.path.clone(), record.id)
            && old != record.id
        {
            return Err("Conflicting prepared prefab identities".into());
        }
    }
    let expected = prefab_export::from_snapshot(
        &project,
        &layout,
        &roads,
        &objects,
        |path| ids.get(path).copied(),
        || false,
    )?
    .ok_or("Record validation cancelled")?;
    if expected.records != prefabs {
        return Err(
            "Prepared prefab records do not cover all current snapshot placements and transforms"
                .into(),
        );
    }
    let paths =
        path_export::from_snapshot(&project, &roads, || false)?.ok_or("Path export cancelled")?;
    let names: std::collections::HashSet<_> = paths.iter().map(|p| p.name.as_str()).collect();
    if prefabs
        .iter()
        .any(|p| p.category.starts_with("Road ") && !names.contains(p.category.as_str()))
    {
        return Err("Prefab references a missing road category".into());
    }
    let mut late = None;
    if road_height {
        use papyrus_terrain::{decor_collider::Contributor, height_mesh::Mesh};
        let chunks = roads["surfaces"]["files"]
            .as_array()
            .ok_or("Missing surface chunks")?;
        if chunks.len() > 8192 || roads["surfaces"]["collision_lod"] != 0 {
            return Err("Unsupported road collision roster".into());
        }
        let mut seen = std::collections::HashSet::new();
        let mut contributors = Vec::new();
        for chunk in chunks.iter().filter(|c| c["lod"] == 0) {
            let road = chunk["road"].as_u64().ok_or("Missing surface road")? as usize;
            let index = chunk["chunk_index"]
                .as_u64()
                .ok_or("Missing surface chunk")?;
            let name = format!("road-surface-{road}-lod-0-chunk-{index}.json");
            if chunk["file"] != name
                || !seen.insert(name.clone())
                || paths.get(road).is_none_or(|p| p.hierarchy >= 2)
            {
                return Err("Invalid or duplicate road collision chunk".into());
            }
            let v: serde_json::Value = serde_json::from_slice(&bytes(&name)?)?;
            if v["road"] != chunk["road"] || v["lod"] != 0 || v["chunk_index"] != index {
                return Err("Road collision chunk identity mismatch".into());
            }
            let origin: [f32; 3] = serde_json::from_value(v["geometry"]["origin"].clone())?;
            let template: papyrus_terrain::path_mesh::Template =
                serde_json::from_value(v["geometry"]["mesh"].clone())?;
            template.validate()?;
            contributors.push(Contributor {
                mesh: Mesh {
                    kind: "raw-triangle-mesh".into(),
                    enabled: true,
                    trigger: false,
                    convex: false,
                    cooking_options: 0,
                    backfaces: false,
                    vertices: template.vertices,
                    triangles: template.triangles,
                },
                local_to_root: [
                    [1., 0., 0., origin[0]],
                    [0., 1., 0., origin[1]],
                    [0., 0., 1., origin[2]],
                    [0., 0., 0., 1.],
                ],
            });
        }
        if contributors.is_empty() {
            return Err("No road collision contributors".into());
        }
        late = Some(
            papyrus_terrain::gameplay_height::generate(
                &l.height,
                hr,
                size as f32,
                &contributors,
                256_000_000,
                || false,
            )?
            .ok_or("Gameplay height canceled")?,
        );
    }
    let saved = if !road_height && !m["gameplay_height"].is_null() {
        let declaration = &m["gameplay_height"];
        if declaration["engine"] != papyrus_terrain::gameplay_height::VERSION
            || declaration["file"] != "gameplay-height.i16le"
            || declaration["report"] != "gameplay-height.json"
            || declaration["scope"] != "road collision triangles only"
            || declaration["other_late_contributors_complete"] != false
        {
            return Err("Unknown saved gameplay-height contract".into());
        }
        let h = shorts("gameplay-height.i16le")?;
        if h.len() != l.height.len() || h.iter().zip(&l.height).any(|(a, b)| a < b || *a > 32766) {
            return Err("Invalid saved gameplay height".into());
        }
        let raw = bytes("gameplay-height.json")?;
        let report: serde_json::Value = serde_json::from_slice(&raw)?;
        if report["engine"] != papyrus_terrain::gameplay_height::VERSION
            || report["physics_cooking_validated"] != false
            || report["changed_samples"]
                != h.iter().zip(&l.height).filter(|(a, b)| a != b).count() as u64
        {
            return Err("Saved gameplay-height report mismatch".into());
        }
        Some((h, report))
    } else {
        None
    };
    let height = late.as_ref().map_or_else(
        || {
            saved
                .as_ref()
                .map_or(l.height.as_slice(), |s| s.0.as_slice())
        },
        |o| o.height.as_slice(),
    );
    let parent = a[3].parent().ok_or("Missing output parent")?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut out = BufWriter::new(stage.as_file_mut());
        map_container::write_v10(
            World {
                layers: &l,
                height,
                alpha: &alpha,
                water: Some(&water),
                prefabs: &prefabs,
                paths: &paths,
            },
            0,
            &mut out,
            || false,
        )?
        .ok_or("Container write canceled")?;
        out.flush()?;
    }
    stage.as_file().sync_all()?;
    let encoded = std::fs::read(stage.path())?;
    let decoded = papyrus_terrain::content::read_map(&encoded, || false)?;
    if decoded.size != size
        || decoded.prefabs.len() != prefabs.len()
        || decoded.paths.len() != paths.len()
        || decoded.layers.len() != 7
    {
        return Err("Container roundtrip failed".into());
    }
    for (p, expected) in decoded.paths.iter().zip(&paths) {
        if p.name != expected.name
            || p.hierarchy != expected.hierarchy
            || p.nodes.len() != expected.nodes.len()
        {
            return Err("Path hierarchy roundtrip failed".into());
        }
    }
    let digest = format!("{:x}", Sha256::digest(&encoded));
    stage.persist_noclobber(&a[3])?;
    println!(
        "{}",
        serde_json::json!({"output":a[3],"version":10,"bytes":encoded.len(),"sha256":digest,"layers":7,"prefabs":prefabs.len(),"paths":paths.len(),"dirt_paths":paths.iter().filter(|p|p.hierarchy==2).count(),"playability_validated":false,"world_generation_complete":false,"height_policy":if saved.is_some() {"receipt-checked saved road gameplay height; other late contributors pending"}else if road_height {"raw LOD0 road collider triangles; other late contributors and PhysX cooking pending"}else{"terrain duplicated into gameplay height; late collider/procedural height still pending"},"late_road_height":late.as_ref().map(|o|serde_json::to_value(&o.report)).transpose()?.or_else(||saved.as_ref().map(|s|s.1.clone())),"server_launched":false,"path_policy":path_export::VERSION})
    );
    Ok(())
}
