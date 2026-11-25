//! Coordinate grid from the inspected MapHelper implementation. This is the
//! paper-map reference grid, not WorldGrid's 50-metre visibility cells.
mod appearance;
pub use appearance::Appearance;
pub(crate) use appearance::prepare as prepare_appearance;
pub const VERSION: &str = "papyrus-map-helper-grid-1";
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub size: u32,
    pub nominal_count: u32,
    pub cell_size: f32,
}
impl Grid {
    /// The PaperMap shader divides the terrain rectangle into this many equal
    /// parts. This differs from the integer cell width used by teleport2grid.
    pub fn paper(size: u32) -> Option<Self> {
        Self::with_paper_spread(size, 7.)
    }

    fn with_paper_spread(size: u32, spread: f32) -> Option<Self> {
        if !(1000..=6000).contains(&size) {
            return None;
        }
        let count = (size as f32 * spread / 1024.).floor() as u32;
        if !(1..=63).contains(&count) {
            return None;
        }
        Some(Self {
            size,
            nominal_count: count,
            cell_size: size as f32 / count as f32,
        })
    }

    pub fn new(size: u32) -> Option<Self> {
        if !(1000..=6000).contains(&size) {
            return None;
        }
        let count = (size as f32 / 146.28572_f32 + 0.001).floor() as u32;
        // World.Size is uint and num2 is int in the source expression: C#
        // promotes them to long and divides as integers before float conversion.
        Some(Self {
            size,
            nominal_count: count,
            cell_size: (size / count) as f32,
        })
    }
    pub fn visible_cells(self) -> u32 {
        (self.size as f32 / self.cell_size).ceil() as u32
    }
    pub fn cell(self, x: f32, z: f32) -> Option<[i32; 2]> {
        if !x.is_finite() || !z.is_finite() {
            return None;
        }
        let half = self.size as f32 / 2.;
        Some([
            ((x + half) / self.cell_size).floor() as i32,
            ((half - z) / self.cell_size).floor() as i32,
        ])
    }
    pub fn center(self, cell: [i32; 2]) -> [f32; 2] {
        let half = self.size as f32 / 2.;
        [
            -half + cell[0] as f32 * self.cell_size + self.cell_size / 2.,
            half - cell[1] as f32 * self.cell_size - self.cell_size / 2.,
        ]
    }
}
pub fn column(column: i32) -> String {
    let mut n = column.max(0) as u64 + 1;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII map column")
}
pub fn label(cell: [i32; 2]) -> String {
    format!("{}{}", column(cell[0]), cell[1])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cells_labels_and_centers_match_owned_map_helper() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/server-bd19c4ac-map-grid.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], "mapper-owned-map-grid-1");
        let rows = fixture["results"].as_array().unwrap();
        assert_eq!(rows.len(), 80);
        for row in rows {
            let q = &row["query"];
            let grid = Grid::new(q["size"].as_u64().unwrap() as u32).unwrap();
            let x: f32 = serde_json::from_value(q["x"].clone()).unwrap();
            let z: f32 = serde_json::from_value(q["z"].clone()).unwrap();
            let expected: [i32; 2] = serde_json::from_value(row["cell"].clone()).unwrap();
            assert_eq!(grid.cell(x, z), Some(expected), "{q}");
            assert_eq!(label(expected), row["label"].as_str().unwrap(), "{q}");
            let bits: [i32; 2] = serde_json::from_value(row["center_bits"].clone()).unwrap();
            assert_eq!(
                grid.center(expected).map(|v| v.to_bits() as i32),
                bits,
                "{q}"
            );
        }
    }
    #[test]
    fn boundaries_and_partial_edge_cells_use_source_integer_division() {
        let grid = Grid::new(4000).unwrap();
        assert_eq!(grid.nominal_count, 27);
        assert_eq!(grid.cell_size, 148.);
        assert_eq!(grid.visible_cells(), 28);
        assert_eq!(grid.cell(-2000., 2000.), Some([0, 0]));
        assert_eq!(grid.cell(-1852., 1852.), Some([1, 1]));
        assert_eq!(grid.cell(1999., -1999.), Some([27, 27]));
        assert_eq!(label([25, 0]), "Z0");
        assert_eq!(label([26, 12]), "AA12");
        assert_eq!(label([-1, -2]), "A-2");
        assert_eq!(Grid::new(4500).unwrap().cell_size, 150.);
    }

    #[test]
    fn paper_map_uses_exact_divisions_and_inverts_texture_y() {
        for (size, count) in [(2194, 14), (3500, 23), (4000, 27), (4500, 30), (6000, 41)] {
            let grid = Grid::paper(size).unwrap();
            assert_eq!(grid.nominal_count, count);
            let half = size as f32 / 2.;
            assert_eq!(grid.cell(-half, half), Some([0, 0]));
            assert_eq!(grid.cell(half - 0.1, -half + 0.1), Some([count as i32 - 1; 2]));
            assert_eq!(grid.cell(-half + grid.cell_size * 1.5, half - grid.cell_size * 2.5), Some([1, 2]));
        }
        assert_ne!(Grid::paper(4000).unwrap().cell_size, Grid::new(4000).unwrap().cell_size);
    }
}
