//! Retained generated underground prefabs and their source/layout receipts.
use crate::dungeon_base::Pose;
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-underground-rail-world-1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub instance: u64,
    pub prefab: String,
    pub prefab_id: u32,
    pub pose: Pose,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_position: Option<[f32; 3]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub engine: String,
    pub source_sha256: String,
    pub monument_sha256: String,
    pub seed: u32,
    pub layout_fingerprint: [u8; 32],
    pub instances: Vec<Instance>,
    pub report: serde_json::Value,
}
