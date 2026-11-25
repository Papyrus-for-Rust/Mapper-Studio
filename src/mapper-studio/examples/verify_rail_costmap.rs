//! Local final-terrain routing check. Produces a reproducible CLI query, not rails.
use mapper_studio::{
    Project,
    rail_preview::{Query, Terrain},
};
use papyrus_terrain::rail_costmap::{Grid, Policy, Route, RouteSettings, route};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::Instant,
};
fn write(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 {
        return Err("verify_rail_costmap PROJECT BUNDLE PROFILE NEW_REPORT NEW_QUERY".into());
    }
    if Path::new(&args[3]).exists() || Path::new(&args[4]).exists() {
        return Err("Choose new output paths".into());
    }
    let project = Project::load(Path::new(&args[0]))?;
    let mut raw = Vec::new();
    File::open(&args[2])
        .map_err(|e| e.to_string())?
        .take(65537)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 65536 {
        return Err("Profile exceeds bound".into());
    }
    let profile: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let policy: Policy =
        serde_json::from_value(profile["costmap"].clone()).map_err(|e| e.to_string())?;
    let terrain = Terrain::load(Path::new(&args[1]), &project, || false)?;
    let timer = Instant::now();
    let grid = terrain.grid(&policy, || false)?.ok_or("Cancelled")?;
    let build_ms = timer.elapsed().as_millis();
    let n = grid.resolution();
    let settings = RouteSettings {
        seed: 7,
        maximum_grade: 0.15,
        maximum_expansions: 250000,
    };
    let (start, end, component_cells) = test_endpoints(&grid, settings.maximum_grade)?;
    if !matches!(
        route(&grid, start, end, settings, || false)?,
        Route::Found { .. }
    ) {
        return Err(
            "Connected test endpoints did not produce a candidate within the route budget".into(),
        );
    }
    let a = grid.point(start).unwrap();
    let b = grid.point(end).unwrap();
    let query = Query {
        start: [a[0], a[2]],
        end: [b[0], b[2]],
        settings,
    };
    let mut report = terrain.audit(&grid, Some(&query), || false)?;
    report["costmap_build_ms"] = serde_json::json!(build_ms);
    report["test_component_cells"] = serde_json::json!(component_cells);
    write(
        Path::new(&args[4]),
        &serde_json::json!({"start":query.start,"end":query.end,"settings":query.settings}),
    )?;
    write(Path::new(&args[3]), &report)?;
    println!(
        "Verified candidate route; cost grid {n}x{n} built in {build_ms} ms. Terrain and project unchanged."
    );
    Ok(())
}

// Select reproducible endpoints in the largest discrete passable component.
// This only chooses the test fixture: it never relaxes costs/grade constraints.
fn test_endpoints(grid: &Grid, grade: f32) -> Result<([usize; 2], [usize; 2], usize), String> {
    let n = grid.resolution();
    let spacing = grid.audit()["cell_spacing"]
        .as_f64()
        .ok_or("Missing spacing")? as f32;
    let mut seen = vec![false; n * n];
    let mut largest = Vec::new();
    for start in 0..n * n {
        if seen[start] || grid.costs()[start] == 0 {
            continue;
        }
        seen[start] = true;
        let mut queue = VecDeque::from([start]);
        let mut component = Vec::new();
        while let Some(i) = queue.pop_front() {
            component.push(i);
            for dz in -1isize..=1 {
                for dx in -1isize..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let (Some(x), Some(z)) = (
                        (i % n).checked_add_signed(dx).filter(|&v| v < n),
                        (i / n).checked_add_signed(dz).filter(|&v| v < n),
                    ) else {
                        continue;
                    };
                    let j = z * n + x;
                    if seen[j] || grid.costs()[j] == 0 {
                        continue;
                    }
                    let diagonal = dx != 0 && dz != 0;
                    if diagonal
                        && (grid.costs()[(i / n) * n + x] == 0 || grid.costs()[z * n + i % n] == 0)
                    {
                        continue;
                    }
                    let distance = spacing
                        * if diagonal {
                            std::f32::consts::SQRT_2
                        } else {
                            1.
                        };
                    if (grid.heights()[j] - grid.heights()[i]).abs() / distance > grade {
                        continue;
                    }
                    seen[j] = true;
                    queue.push_back(j);
                }
            }
        }
        if component.len() > largest.len() {
            largest = component;
        }
    }
    let start = *largest.first().ok_or("No passable rail component")?;
    let end = *largest
        .iter()
        .max_by_key(|&&i| (i % n).abs_diff(start % n).pow(2) + (i / n).abs_diff(start / n).pow(2))
        .unwrap();
    if start == end {
        return Err("No component contains distinct test endpoints".into());
    }
    Ok(([start % n, start / n], [end % n, end / n], largest.len()))
}
