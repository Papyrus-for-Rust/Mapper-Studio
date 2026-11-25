//! Retained surface rail geometry and transient keep-clear data in composition.
use serde::Serialize;
#[derive(Debug, Serialize)]
pub struct Chunk {
    pub track_id: u64,
    pub lod: usize,
    pub chunk_index: usize,
    pub geometry: crate::path_mesh::Chunk,
}
impl Chunk {
    pub fn filename(&self) -> String {
        format!(
            "rail-mesh-track-{}-lod-{}-chunk-{}.json",
            self.track_id, self.lod, self.chunk_index
        )
    }
}
pub struct Scene {
    pub report: serde_json::Value,
    pub tracks: Vec<crate::rail_bed::FittedTrack>,
    pub meshes: Vec<Chunk>,
    pub reservations: crate::rail_reservations::Output,
}
