//! Borrowed enabled placement raster with source-order world normalization.
//! Structural checks are constant-time; queried bytes must encode bool 0/1.
#[derive(Clone, Copy)]
pub struct PlacementMap<'a> {
    size: f32,
    resolution: usize,
    blocked: &'a [u8],
}
impl<'a> PlacementMap<'a> {
    pub fn new(size: f32, resolution: usize, blocked: &'a [u8]) -> Result<Self, String> {
        if !size.is_finite()
            || !(1. ..=6000.).contains(&size)
            || !(2..=2048).contains(&resolution)
            || blocked.len() != resolution * resolution
        {
            return Err("Invalid placement raster layout".into());
        }
        Ok(Self {
            size,
            resolution,
            blocked,
        })
    }
    pub fn matches(&self, size: f32, resolution: usize) -> bool {
        self.size.to_bits() == size.to_bits() && self.resolution == resolution
    }
    pub fn is_blocked(&self, point: [f32; 2]) -> Result<bool, String> {
        if point.iter().any(|v| !v.is_finite()) {
            return Err("Non-finite placement query".into());
        }
        // TerrainMeta.Normalize -> TerrainMap.Index, not world/size + 0.5.
        let [x, z] = point
            .map(|v| (((v + self.size * 0.5) * (1. / self.size)) * self.resolution as f32) as i32);
        let index = z.clamp(0, self.resolution as i32 - 1) as usize * self.resolution
            + x.clamp(0, self.resolution as i32 - 1) as usize;
        match self.blocked[index] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("Invalid boolean placement cell".into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placement_normalization_keeps_source_rounding_at_cell_boundary() {
        let mut cells = vec![0; 128 * 128];
        cells[64 * 128 + 32] = 1;
        let point = f32::from_bits((-250_f32).to_bits() + 1);
        assert_eq!(((point / 1000. + 0.5) * 128.) as usize, 31);
        assert!(
            PlacementMap::new(1000., 128, &cells)
                .unwrap()
                .is_blocked([point, 0.])
                .unwrap()
        );
    }
    #[test]
    fn placement_queries_clamp_source_indices_and_refuse_malformed_cells() {
        let mut cells = vec![0; 16];
        cells[0] = 1;
        cells[15] = 1;
        cells[6] = 2;
        let map = PlacementMap::new(1000., 4, &cells).unwrap();
        assert!(map.is_blocked([-500., -500.]).unwrap());
        assert!(map.is_blocked([-900., -900.]).unwrap());
        assert!(map.is_blocked([500., 500.]).unwrap());
        assert!(!map.is_blocked([-125., -125.]).unwrap());
        assert!(map.is_blocked([125., -125.]).is_err());
        assert!(map.is_blocked([f32::NAN, 0.]).is_err());
        assert!(PlacementMap::new(1000., 4, &cells[..15]).is_err());
    }
}
