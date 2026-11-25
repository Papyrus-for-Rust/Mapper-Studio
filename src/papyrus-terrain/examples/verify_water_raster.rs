//! Headless, synthetic full-resolution water geometry verification. No game boot.
use papyrus_terrain::{erosion::decode, water_mesh::*};
use std::{fs::OpenOptions, time::Instant};

fn main() -> Result<(), String> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or("Expected NEW_REPORT path")?;
    // Reserve the requested output before doing work; never overwrite evidence.
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    let n = 4097;
    let size = 4000.;
    let existing = vec![0; n * n];
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let (s, c) = 37_f64.to_radians().sin_cos();
    for i in 0..=512 {
        let x = -1600. + i as f64 * 6.25;
        let z = (x * 0.004).sin() * 160.;
        for offset in [-7., 7.] {
            vertices.push([
                x * c - (z + offset) * s,
                45. - i as f64 * 0.05,
                x * s + (z + offset) * c,
            ]);
        }
        // Deliberate gaps must stay dry; this is not a complete river fixture.
        if i > 0 && i % 29 != 0 {
            let a = (i * 2 - 2) as u32;
            triangles.extend([[a, a + 1, a + 2], [a + 2, a + 1, a + 3]]);
        }
    }
    let surface = Surface::new(&vertices, &triangles)?;
    let surfaces = [RasterSurface {
        surface: &surface,
        two_sided: false,
    }];
    let run = || {
        rasterize(
            RasterGrid {
                size,
                resolution: n,
                existing: &existing,
            },
            &surfaces,
            128_000_000,
            || false,
        )?
        .ok_or_else(|| "Unexpected cancellation".to_string())
    };
    let start = Instant::now();
    let first = run()?;
    let first_ms = start.elapsed().as_millis();
    let replay = run()?;
    if first.water != replay.water || first.report.changed_samples == 0 {
        return Err("Water raster replay differs or generated no water".into());
    }
    let mut query_work = 0;
    let mut queries = 0;
    let mut wet_queries = 0;
    // Compare the optimized raster with independent point-to-all-triangle rays.
    // Half the samples follow the ribbon; half cover the complete map.
    for i in 0..8192 {
        let [x, z] = if i % 2 == 0 {
            let along = -1600. + (i / 2) as f64 / 4095. * 3200.;
            let across = (along * 0.004).sin() * 160.;
            [along * c - across * s, along * s + across * c]
        } else {
            [
                ((i * 1951 % n) as f64 / (n - 1) as f64 - 0.5) * size,
                ((i * 1151 % n) as f64 / (n - 1) as f64 - 0.5) * size,
            ]
        };
        let cell = |v: f64| (((v / size + 0.5) * n as f64).floor() as usize).min(n - 1);
        let (x, z) = (cell(x), cell(z));
        let world = |v: usize| ((v as f64 + 0.5) / n as f64 - 0.5) * size;
        let expected = surface.highest(
            world(x),
            world(z),
            false,
            &mut query_work,
            16_000_000,
            || false,
        )?;
        let actual = first.water[z * n + x];
        match expected {
            None if actual != 0 => return Err("Raster filled a dry query".into()),
            Some(y) if actual == 0 || (f64::from(decode(actual)) - y).abs() > 0.016 => {
                return Err("Raster height differs from independent ray".into());
            }
            _ => {}
        }
        wet_queries += usize::from(expected.is_some());
        queries += 1;
    }
    if wet_queries < 3000 || existing.iter().any(|&v| v != 0) {
        return Err("Verification coverage insufficient or input mutated".into());
    }
    serde_json::to_writer_pretty(
        file,
        &serde_json::json!({
            "format":"papyrus-water-raster-verification", "schema":1,
            "fixture":"synthetic sloping ribbon rotated 37 degrees with regular gaps",
            "resolution":n, "world_size":size, "first_run_ms":first_ms,
            "report":first.report, "replay_identical":true,
            "independent_queries":queries, "wet_queries":wet_queries,
            "input_preserved":true, "terrain_clearance_applied":false,
            "topology_applied":false, "game_validated":false,
            "connected_to_normal_generation":false
        }),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{} water samples; {queries} independent queries; exact replay; {first_ms} ms",
        first.report.changed_samples
    );
    Ok(())
}
