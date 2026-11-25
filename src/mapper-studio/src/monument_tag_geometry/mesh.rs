use super::vector;
use papyrus_terrain::environment::Box3;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MeshBounds {
    kind: String,
    center: [f32; 3],
    extent: [f32; 3],
    asset_file: String,
    object_id: i64,
}
impl MeshBounds {
    pub fn read(value: &serde_json::Value) -> Result<Option<Self>, String> {
        let Some(raw) = value.get("mesh_bounds") else {
            return Ok(None);
        };
        let bounds: Self =
            serde_json::from_value(raw.clone()).map_err(|error| error.to_string())?;
        if bounds.kind != "serialized_mesh_local_aabb"
            || !vector(&bounds.center)
            || !vector(&bounds.extent)
            || bounds.extent.iter().any(|v| *v < 0.)
            || bounds.object_id == 0
            || bounds.asset_file.is_empty()
            || bounds.asset_file.len() > 512
            || bounds.asset_file.contains('\0')
        {
            return Err("Invalid authored tagged mesh bounds".into());
        }
        Ok(Some(bounds))
    }
    pub fn world_bounds(&self, matrix: [[f32; 4]; 4]) -> Result<Box3, String> {
        let result = Box3 {
            center: std::array::from_fn(|row| {
                ((matrix[row][0] * self.center[0] + matrix[row][1] * self.center[1])
                    + matrix[row][2] * self.center[2])
                    + matrix[row][3]
            }),
            extents: std::array::from_fn(|row| {
                (matrix[row][0].abs() * self.extent[0] + matrix[row][1].abs() * self.extent[1])
                    + matrix[row][2].abs() * self.extent[2]
            }),
            rotation: [0., 0., 0., 1.],
        };
        result.validate()?;
        Ok(result)
    }
}
