//! Explicit local geometric check. Does not establish Unity/PhysX raycast parity.
use mapper_studio::{Project, prefab_assets, water_collision::Cache, water_geometry::Geometry};
use papyrus_terrain::water_mesh::Surface;
use std::{collections::BTreeSet, path::Path};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "verify_water_collision <Rust root> <project> <catalog> <collision cache> <report>"
                .into(),
        );
    }
    let root = Path::new(&args[0]);
    let project = Project::load(Path::new(&args[1]))?;
    let catalog = prefab_assets::load(
        root,
        Path::new(&args[2]),
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?;
    let cache = Cache::load(root, Path::new(&args[3]), &catalog, || false)?;
    let mut seen = BTreeSet::new();
    let mut checked = 0;
    let mut work = 0;
    for (path, asset) in &catalog.entries {
        let Some(report) = &asset.water_geometry else {
            continue;
        };
        for collider in report
            .water_bodies
            .iter()
            .flat_map(|b| &b.triggers)
            .chain(report.contributors.iter().map(|c| &c.collider))
        {
            if !matches!(collider.geometry, Geometry::Mesh)
                || !seen.insert((path, collider.object_id))
            {
                continue;
            }
            let mesh = cache
                .mesh(path, collider.object_id)
                .ok_or("Unresolved source mesh")?;
            let original = Surface::new(&mesh.vertices, &mesh.triangles)?;
            let x = (original.minimum[0] + original.maximum[0]) / 2.;
            let z = (original.minimum[2] + original.maximum[2]) / 2.;
            let height = original
                .highest(x, z, true, &mut work, 100000, || false)?
                .ok_or("No source-center surface")?;
            for degrees in [13_f64, 37., 47.5, 123., 271.5] {
                let (s, c) = degrees.to_radians().sin_cos();
                let vertices: Vec<_> = mesh
                    .vertices
                    .iter()
                    .map(|v| {
                        [
                            100. + v[0] * c - v[2] * s,
                            25. + v[1],
                            -200. + v[0] * s + v[2] * c,
                        ]
                    })
                    .collect();
                let rotated = Surface::new(&vertices, &mesh.triangles)?;
                let result = rotated
                    .highest(
                        100. + x * c - z * s,
                        -200. + x * s + z * c,
                        true,
                        &mut work,
                        100000,
                        || false,
                    )?
                    .ok_or("No rotated source surface")?;
                if (result - height - 25.).abs() > 1e-7 {
                    return Err("Raw source surface rotation/translation invariance failed".into());
                }
                checked += 1;
            }
        }
    }
    if seen.is_empty() {
        return Err("No source collision meshes checked".into());
    }
    let report = serde_json::json!({"cache_sha256":cache.sha256,"source_meshes":seen.len(),"non_cardinal_rotation_checks":checked,"triangle_tests":work,"translated_height_invariance":true,"physics_cooked":false,"rasterized":false,"game_validated":false});
    std::fs::write(
        &args[4],
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
