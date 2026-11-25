//! Read-only inspection of the retained native grids. Images sample data without modifying it.
use eframe::egui::{Color32, ColorImage};
use papyrus_terrain::native_layers::{Layers, dimensions};
pub(crate) const GROUND_PALETTE: [[f32; 3]; 8] = [
    [120., 108., 78.],
    [224., 233., 229.],
    [180., 169., 125.],
    [132., 135., 121.],
    [114., 129., 79.],
    [72., 98., 65.],
    [148., 147., 127.],
    [132., 122., 96.],
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Overview,
    Heights,
    Biomes,
    Ground,
    Topology,
    Water,
    ProcessedTopology,
    SpawnCandidates,
}
impl View {
    pub const ALL: [Self; 8] = [
        Self::Overview,
        Self::Heights,
        Self::Biomes,
        Self::Ground,
        Self::Topology,
        Self::Water,
        Self::ProcessedTopology,
        Self::SpawnCandidates,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Heights => "Heights",
            Self::Biomes => "Biomes",
            Self::Ground => "Ground materials",
            Self::Topology => "Topology bit",
            Self::Water => "Water classification",
            Self::ProcessedTopology => "Processed topology bit",
            Self::SpawnCandidates => "Potential player spawns",
        }
    }
    pub fn legend(self) -> &'static str {
        match self {
            Self::Overview => "Native surface overview",
            Self::Heights => "Water: blue · Land: black at sea level to white at 300 m",
            Self::Biomes => "Weighted native biome channels, including jungle",
            Self::Ground => "Weighted native splat channels",
            Self::Topology => "White: selected bit is set · Dark: bit is clear",
            Self::ProcessedTopology => "Processed flags · White: bit set · Dark: bit clear",
            Self::Water => "Blue: ocean · Amber: enclosed below sea level (unfilled) · Green: land",
            Self::SpawnCandidates => {
                "Green: passes terrain filters and conservative clearance · Physics and game spawning unchecked"
            }
        }
    }
}
pub fn render_spawn_candidates(
    audit: &papyrus_terrain::spawn::Audit,
    output: usize,
) -> Result<ColorImage, String> {
    let n = audit.resolution;
    if !(2..=2048).contains(&n)
        || audit.character_mask.len() != n * n
        || !(2..=4096).contains(&output)
    {
        return Err("Invalid spawn inspection dimensions".into());
    }
    let pixels = (0..output * output)
        .map(|i| {
            let z = (output - 1 - i / output) * (n - 1) / (output - 1);
            let x = (i % output) * (n - 1) / (output - 1);
            if audit.character_mask[z * n + x] != 0 {
                Color32::from_rgb(96, 200, 116)
            } else {
                Color32::from_gray(30)
            }
        })
        .collect();
    Ok(ColorImage::new([output, output], pixels))
}
pub fn render(layers: &Layers, view: View, bit: u8) -> Result<ColorImage, String> {
    render_processed(layers, view, bit, None)
}
/// Combined painter inspection: white means all selected bits, amber a partial
/// match, dark none. This never replaces or normalizes the underlying bitfield.
pub fn render_topology_selection(
    layers: &Layers,
    mask: u32,
    output: usize,
) -> Result<ColorImage, String> {
    let (_, n) = dimensions(layers.size)?;
    if layers.surface_resolution != n
        || layers.topology.len() != n * n
        || !(2..=4096).contains(&output)
    {
        return Err("Invalid topology inspection dimensions".into());
    }
    let pixels = (0..output * output)
        .map(|i| {
            let z = (output - 1 - i / output) * (n - 1) / (output - 1);
            let x = (i % output) * (n - 1) / (output - 1);
            let flags = layers.topology[z * n + x] as u32 & mask;
            if flags == 0 {
                Color32::from_gray(30)
            } else if flags == mask {
                Color32::WHITE
            } else {
                Color32::from_rgb(220, 158, 60)
            }
        })
        .collect();
    Ok(ColorImage::new([output, output], pixels))
}
pub fn render_processed(
    layers: &Layers,
    view: View,
    bit: u8,
    ocean: Option<&papyrus_terrain::ocean::Ocean>,
) -> Result<ColorImage, String> {
    render_sized(layers, view, bit, ocean, 513)
}
pub fn render_sized(
    layers: &Layers,
    view: View,
    bit: u8,
    ocean: Option<&papyrus_terrain::ocean::Ocean>,
    output: usize,
) -> Result<ColorImage, String> {
    let (hr, sr) = dimensions(layers.size)?;
    let cells = sr * sr;
    if layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || layers.biome.len() != cells * 5
        || layers.splat.len() != cells * 8
        || layers.topology.len() != cells
        || bit > 31
        || matches!(view, View::Overview | View::SpawnCandidates)
        || !(2..=4096).contains(&output)
    {
        return Err("Invalid native inspection inputs".into());
    }
    if matches!(view, View::Water | View::ProcessedTopology)
        && !ocean.is_some_and(|o| {
            o.resolution == sr && o.classification.len() == cells && o.topology.len() == cells
        })
    {
        return Err("Build ocean reconstruction before inspecting processed water".into());
    }
    let n = output;
    let biomes = [
        [185., 164., 112.],
        [110., 141., 85.],
        [147., 151., 129.],
        [224., 233., 229.],
        [49., 101., 60.],
    ];
    let ground = GROUND_PALETTE;
    let mut pixels = Vec::with_capacity(n * n);
    for row in 0..n {
        for x in 0..n {
            let z = n - 1 - row;
            let i = z * (sr - 1) / (n - 1) * sr + x * (sr - 1) / (n - 1);
            let h = layers.height[z * (hr - 1) / (n - 1) * hr + x * (hr - 1) / (n - 1)] as f32
                / 32766.
                * 1000.
                - 500.;
            let weighted = |values: &[u8], palette: &[[f32; 3]]| -> Color32 {
                let sum: f32 = (0..palette.len())
                    .map(|c| f32::from(values[c * cells + i]))
                    .sum();
                let color: [u8; 3] = std::array::from_fn(|channel| {
                    if sum == 0. {
                        0
                    } else {
                        palette
                            .iter()
                            .enumerate()
                            .map(|(c, p)| p[channel] * f32::from(values[c * cells + i]) / sum)
                            .sum::<f32>()
                            .round()
                            .clamp(0., 255.) as u8
                    }
                });
                Color32::from_rgb(color[0], color[1], color[2])
            };
            pixels.push(match view {
                View::Heights if h <= 0. => Color32::from_rgb(25, 67, 87),
                View::Heights => Color32::from_gray((h / 300. * 255.).clamp(0., 255.) as u8),
                View::Biomes => weighted(&layers.biome, &biomes),
                View::Ground => weighted(&layers.splat, &ground),
                View::Topology => {
                    if (layers.topology[i] as u32) & (1_u32 << bit) != 0 {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(30)
                    }
                }
                View::ProcessedTopology => {
                    if (ocean.unwrap().topology[i] as u32) & (1_u32 << bit) != 0 {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(30)
                    }
                }
                View::Water => match ocean.unwrap().classification[i] {
                    papyrus_terrain::ocean::OCEAN => Color32::from_rgb(36, 114, 160),
                    papyrus_terrain::ocean::ENCLOSED => Color32::from_rgb(220, 158, 60),
                    _ => Color32::from_rgb(76, 108, 73),
                },
                View::Overview | View::SpawnCandidates => unreachable!(),
            });
        }
    }
    Ok(ColorImage::new([n, n], pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspection_uses_planar_channels_north_up_and_unsigned_topology_bits() {
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 123,
            biome_seed: 456,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[4 * cells..].fill(255);
        layers.splat[5 * cells..6 * cells].fill(255);
        layers.topology[0] = i32::MIN;
        let biome = render(&layers, View::Biomes, 0).unwrap();
        assert_eq!(biome.pixels[0], Color32::from_rgb(49, 101, 60));
        let material = render(&layers, View::Ground, 0).unwrap();
        assert_eq!(material.pixels[0], Color32::from_rgb(72, 98, 65));
        let topology = render(&layers, View::Topology, 31).unwrap();
        assert_eq!(topology.pixels[512 * 513], Color32::WHITE);
        assert_ne!(topology.pixels[0], Color32::WHITE);
        assert_eq!(layers.topology[0], i32::MIN);
        assert!(render(&layers, View::Topology, 32).is_err());
        assert!(render(&layers, View::Overview, 0).is_err());
        layers.topology[0] = 3;
        layers.topology[sr - 1] = 1;
        let combined = render_topology_selection(&layers, 3, 2).unwrap();
        assert_eq!(combined.pixels[2], Color32::WHITE);
        assert_eq!(combined.pixels[3], Color32::from_rgb(220, 158, 60));
        assert_eq!(combined.pixels[0], Color32::from_gray(30));
        assert!(
            render_topology_selection(&layers, 0, 2)
                .unwrap()
                .pixels
                .iter()
                .all(|p| *p == Color32::from_gray(30))
        );
    }
}
