//! Derive cable inspection from the exact prefab roots used by map export.
use crate::{map_export, powerline_source::PlacedWireNode, prefab_identities, scene_wire_source};
use papyrus_terrain::{environment::Transform, powerline_wires, prefab_records::Record};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const VERSION: &str = "mapper-scene-wires-1";
pub type Preview = Result<Option<std::sync::Arc<Scene>>, String>;

#[derive(Serialize)]
pub struct Scene {
    pub engine: &'static str,
    pub source_sha256: String,
    pub identities_sha256: String,
    pub roots_sha256: String,
    pub prefab_roots: usize,
    pub registration_order: &'static str,
    pub nodes: Vec<PlacedWireNode>,
    pub report: powerline_wires::Report,
}

/// Prefab record order followed by captured hierarchy/component order defines
/// this static registration model. Game-runtime registration remains unverified.
pub fn from_records(
    records: &[Record],
    source: &scene_wire_source::Source,
    identities: &prefab_identities::Source,
    settings: powerline_wires::Settings,
    cancel: impl Fn() -> bool,
) -> Result<Option<Scene>, String> {
    if cancel() {
        return Ok(None);
    }
    if records.len() > papyrus_terrain::content::MAX_PREFABS {
        return Err("Cable scene prefab allowance exceeded".into());
    }
    let mut roots = Sha256::new();
    roots.update(b"mapper-scene-wire-roots-1");
    roots.update((records.len() as u64).to_le_bytes());
    let mut nodes = Vec::new();
    for (index, record) in records.iter().enumerate() {
        if cancel() {
            return Ok(None);
        }
        if identities.resolve(&record.path) != Some(record.id) || record.id == 0 {
            return Err(format!(
                "Cable scene prefab identity differs: {}",
                record.path
            ));
        }
        let templates = source.templates().get(&record.path).ok_or_else(|| {
            format!(
                "Cable metadata is missing for {}. Reload the selected Rust installation.",
                record.path
            )
        })?;
        let pose = Transform {
            position: record.position,
            rotation: papyrus_terrain::prefab_records::quaternion(record.rotation)?,
            scale: record.scale,
        };
        if record.category.is_empty()
            || record.category.len() > 128
            || record.category.chars().any(char::is_control)
            || record
                .position
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 10000.)
            || record
                .scale
                .iter()
                .any(|value| !value.is_finite() || *value == 0. || value.abs() > 100.)
        {
            return Err("Invalid cable scene prefab transform or category".into());
        }
        for text in [&record.category, &record.path] {
            roots.update((text.len() as u64).to_le_bytes());
            roots.update(text.as_bytes());
        }
        roots.update(record.id.to_le_bytes());
        for value in record
            .position
            .iter()
            .chain(&record.rotation)
            .chain(&record.scale)
        {
            // The map codec omits zero-valued float fields, including -0.0.
            // Match the coordinates that readers reconstruct from the export.
            let bits = if *value == 0. { 0 } else { value.to_bits() };
            roots.update(bits.to_le_bytes());
        }
        if nodes.len().saturating_add(templates.len()) > settings.maximum_nodes.min(200_000) {
            return Err("Cable scene node allowance exceeded".into());
        }
        for template in templates {
            if cancel() {
                return Ok(None);
            }
            nodes.push(template.place(index as u64, &record.path, &pose)?);
        }
    }
    let inputs: Vec<_> = nodes.iter().map(PlacedWireNode::input).collect();
    let Some(report) = powerline_wires::assemble(&inputs, settings, &cancel)? else {
        return Ok(None);
    };
    if cancel() {
        return Ok(None);
    }
    Ok(Some(Scene {
        engine: VERSION,
        source_sha256: source.sha256().to_owned(),
        identities_sha256: identities.sha256.clone(),
        roots_sha256: format!("{:x}", roots.finalize()),
        prefab_roots: records.len(),
        registration_order: "map-prefab-order/authored-component-order",
        nodes,
        report,
    }))
}

/// Build from an accepted composition through the shared export adapter.
/// All asset metadata access is cache-only.
pub fn from_world(
    input: &map_export::Input<'_>,
    settings: powerline_wires::Settings,
    cancel: impl Fn() -> bool,
) -> Result<Option<Scene>, String> {
    if cancel() {
        return Ok(None);
    }
    let _verification = crate::installation::VerificationScope::default();
    map_export::validate(input)?;
    let source = scene_wire_source::available(input.root, input.catalog, &cancel);
    if cancel() {
        return Ok(None);
    }
    let source = source?;
    let identities = prefab_identities::Source::available(input.root, input.catalog, &cancel);
    if cancel() {
        return Ok(None);
    }
    let identities = identities?;
    let Some(prefabs) = map_export::prepare_prefabs(input, &identities, &cancel)? else {
        return Ok(None);
    };
    from_records(&prefabs.records, &source, &identities, settings, cancel)
}
