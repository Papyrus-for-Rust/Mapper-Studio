//! Explicit developer captures for replaying terrain access searches offline.
use super::{AccessDiagnostics, CellGrid, Push, Settings, TerrainRecovery};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(super) struct Input<'a> {
    pub terrain: TerrainRecovery<'a>,
    pub grid: &'a CellGrid,
    pub starts: &'a [[usize; 2]],
    pub ends: &'a [[usize; 2]],
    pub settings: Settings,
    pub push: Option<Push>,
    pub maximum_nodes: usize,
    pub trace: &'a AccessDiagnostics,
}

pub(super) fn capture(input: Input<'_>) -> Result<(), String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let Some(root) = std::env::var_os("MAPPER_TERRAIN_ACCESS_CAPTURE") else {
        return Ok(());
    };
    if input.trace.stop == "cancelled" {
        return Ok(());
    }
    let root = PathBuf::from(root);
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    if index >= 512 {
        return Err("Terrain access capture count exceeds allowance".into());
    }
    if index == 0 {
        fs::create_dir(&root).map_err(|e| format!("New terrain access capture directory: {e}"))?;
    }
    let data = serde_json::json!({
        "format":"mapper-terrain-access-capture-1",
        "grid": input.grid,
        "heights": input.terrain.heights,
        "maximum_grade": input.terrain.maximum_grade,
        "slope_penalty": input.terrain.slope_penalty,
        "extra_radius": input.terrain.extra_radius,
        "entrance_exclusions": input.terrain.entrance_exclusions,
        "entrance_penalties": input.terrain.entrance_penalties,
        "starts": input.starts,
        "ends": input.ends,
        "settings": input.settings,
        "push": input.push,
        "maximum_nodes": input.maximum_nodes,
        "trace": input.trace,
    });
    let bytes = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Terrain access capture exceeds size allowance".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(format!("query-{index:04}.json")))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
