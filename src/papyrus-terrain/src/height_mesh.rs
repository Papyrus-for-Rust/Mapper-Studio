//! Bounded raw contributor triangles. These are not PhysX-cooked collision data.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mesh {
    pub kind: String,
    pub enabled: bool,
    pub trigger: bool,
    pub convex: bool,
    pub cooking_options: u32,
    pub backfaces: bool,
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}
impl Mesh {
    pub fn validate(&self) -> Result<(), String> {
        if self.kind != "raw-triangle-mesh"
            || self.vertices.len() < 3
            || self.vertices.len() > 65536
            || self.triangles.is_empty()
            || self.triangles.len() > 131072
            || self
                .vertices
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || self
                .triangles
                .iter()
                .flatten()
                .any(|i| *i as usize >= self.vertices.len())
        {
            return Err("Invalid or excessive raw height contributor mesh".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_mesh_bounds_indices_and_metadata_roundtrip_without_assuming_physics_equivalence() {
        let mut mesh = Mesh {
            kind: "raw-triangle-mesh".into(),
            enabled: true,
            trigger: false,
            convex: false,
            cooking_options: 30,
            backfaces: false,
            vertices: vec![[0., 0., 0.], [1., 1., 0.], [0., 0., 1.]],
            triangles: vec![[0, 2, 1]],
        };
        mesh.validate().unwrap();
        let data = serde_json::to_vec(&mesh).unwrap();
        let read: Mesh = serde_json::from_slice(&data).unwrap();
        read.validate().unwrap();
        assert_eq!(data, serde_json::to_vec(&read).unwrap());
        mesh.triangles[0][2] = 3;
        assert!(mesh.validate().is_err());
        mesh.triangles[0][2] = 1;
        mesh.vertices[0][0] = f32::NAN;
        assert!(mesh.validate().is_err());
        mesh.vertices[0][0] = 10001.;
        assert!(mesh.validate().is_err());
    }
}
