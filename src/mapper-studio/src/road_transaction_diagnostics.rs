//! Opt-in snapshots of successful real painted-road transactions. No game runtime.
//! Compiled only with road-diagnostics; every capture has a fresh directory.
use crate::{Project, road_refinement};
use papyrus_terrain::{native_layers::Layers, road_network::Report};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

pub(crate) fn write_words<const N: usize>(
    root: &Path,
    name: &str,
    values: impl Iterator<Item = [u8; N]>,
    cancelled: &impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    let mut out = BufWriter::new(fs::File::create(root.join(name)).map_err(|e| e.to_string())?);
    let mut hash = Sha256::new();
    let mut bytes = 0usize;
    for word in values {
        if bytes.is_multiple_of(65536) && cancelled() {
            return Err("Road transaction capture cancelled".into());
        }
        out.write_all(&word).map_err(|e| e.to_string())?;
        hash.update(word);
        bytes += N;
    }
    out.flush().map_err(|e| e.to_string())?;
    out.get_ref().sync_all().map_err(|e| e.to_string())?;
    Ok(json!({"bytes":bytes,"sha256":format!("{:x}",hash.finalize())}))
}

#[allow(clippy::too_many_arguments)]
pub fn capture_if_requested(
    project: &Project,
    roads: &Report,
    stamped: &Layers,
    painted: &[i16],
    initial: &[i16],
    topology: &[i32],
    protected_topology: i32,
    allowed_topology: i32,
    height_origin: f32,
    vertical_span: f32,
    result: &road_refinement::Output,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    let Some(root) = std::env::var_os("MAPPER_ROAD_TRANSACTION_CAPTURES") else {
        return Ok(());
    };
    capture(
        Path::new(&root),
        project,
        roads,
        stamped,
        painted,
        initial,
        topology,
        protected_topology,
        allowed_topology,
        height_origin,
        vertical_span,
        result,
        cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
fn capture(
    root: &Path,
    project: &Project,
    roads: &Report,
    stamped: &Layers,
    painted: &[i16],
    initial: &[i16],
    topology: &[i32],
    protected_topology: i32,
    allowed_topology: i32,
    height_origin: f32,
    vertical_span: f32,
    result: &road_refinement::Output,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    if !root.is_dir() || cancelled() {
        return Err("Transaction capture root unavailable or cancelled".into());
    }
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(stamped.size)?;
    if stamped.height_resolution != hr
        || stamped.surface_resolution != sr
        || [
            stamped.height.len(),
            painted.len(),
            initial.len(),
            result.height.len(),
        ]
        .iter()
        .any(|n| *n != hr * hr)
        || topology.len() != sr * sr
        || roads.roads.len() > 256
        || result.controls.len() != roads.roads.len()
        || result.controls.iter().map(Vec::len).sum::<usize>() > 131072
    {
        return Err("Invalid road transaction capture layout".into());
    }
    let private = tempfile::Builder::new()
        .prefix(".road-transaction-")
        .tempdir_in(root)
        .map_err(|e| e.to_string())?;
    let mut receipts = BTreeMap::new();
    for (name, values) in [
        ("stamped.i16le", stamped.height.as_slice()),
        ("painted.i16le", painted),
        ("initial.i16le", initial),
        ("refined.i16le", result.height.as_slice()),
    ] {
        receipts.insert(
            name.to_owned(),
            write_words(
                private.path(),
                name,
                values.iter().map(|v| v.to_le_bytes()),
                &cancelled,
            )?,
        );
    }
    receipts.insert(
        "topology.i32le".to_owned(),
        write_words(
            private.path(),
            "topology.i32le",
            topology.iter().map(|v| v.to_le_bytes()),
            &cancelled,
        )?,
    );
    let metadata = json!({"format":"mapper-painted-road-transaction-1","schema":1,"project":project,
        "size":stamped.size,"height_resolution":hr,"surface_resolution":sr,
        "height_origin":height_origin,"vertical_span":vertical_span,"protected_topology":protected_topology,
        "allowed_topology":allowed_topology,"roads":roads.json(),"final_controls":result.controls,
        "bed":result.report,"files":receipts,"runtime_validated":false});
    let raw = serde_json::to_vec(&metadata).map_err(|e| e.to_string())?;
    if raw.len() > 32 * 1024 * 1024 || cancelled() {
        return Err("Road transaction metadata allowance or cancellation".into());
    }
    fs::write(private.path().join("transaction.json"), raw).map_err(|e| e.to_string())?;
    let destination = root.join(
        private
            .path()
            .file_name()
            .ok_or("Capture name unavailable")?
            .to_string_lossy()
            .trim_start_matches('.'),
    );
    if destination.exists() {
        return Err("Road transaction destination already exists".into());
    }
    fs::rename(private.path(), &destination).map_err(|e| e.to_string())?;
    eprintln!(
        "Captured painted road transaction: {}",
        destination.display()
    );
    Ok(())
}
