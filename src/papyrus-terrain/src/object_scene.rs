//! Committed objects from the live composition pass; this is not prefab assembly.
use crate::{decor_fit, decor_live, footprints};
use serde::Serialize;
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Road,
    Decoration,
    Utility,
}
#[derive(Serialize)]
pub struct Instance {
    pub id: usize,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub road: Option<usize>,
    pub prefab: String,
    pub pose: decor_fit::Pose,
}
#[derive(Serialize)]
pub struct Socket {
    pub instance: usize,
    pub prefab: String,
    pub attribute: usize,
    pub infrastructure_type: i32,
    pub position: [f32; 3],
}
#[derive(Serialize)]
pub struct PendingPhase {
    pub scene_order: usize,
    pub class: String,
    pub description: String,
}
#[derive(Serialize)]
pub struct Scene {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utility: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_rail_reservations: Option<serde_json::Value>,
    pub engine: &'static str,
    pub road_seed: u32,
    pub decoration_seed: u32,
    pub road_source_sha256: String,
    pub decoration_source_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uniform_cliffs_source_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coastal_cliffs_source_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jungle_vines_source_sha256: Option<String>,
    pub monument_source_sha256: String,
    pub initial_blockers_sha256: Option<String>,
    pub initial_blockers: crate::placement_blockers::Report,
    pub instances: Vec<Instance>,
    pub sockets: Vec<Socket>,
    pub decoration: Vec<decor_live::Report>,
    pub terrain: footprints::Report,
    pub external_phases: Vec<PendingPhase>,
    pub assembled_instances: usize,
    pub physics_validated: bool,
}
