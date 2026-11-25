//! Immutable terrain dependencies reserved for an accepted generation stage.
use crate::{
    native_layers::Layers,
    path_terrain::{self, Grid, Layout},
};
use md5::{Digest, Md5};
pub struct HeightReservations {
    layout: Layout,
    baseline: [u8; 16],
    words: Vec<u64>,
    samples: usize,
}
fn digest(height: &[i16], cancelled: &impl Fn() -> bool) -> Option<[u8; 16]> {
    let mut hasher = Md5::new();
    for chunk in height.chunks(4096) {
        if cancelled() {
            return None;
        }
        #[cfg(target_endian = "little")]
        hasher.update(bytemuck::cast_slice::<i16, u8>(chunk));
        #[cfg(target_endian = "big")]
        {
            let mut buffer = [0_u8; 8192];
            for (v, out) in chunk.iter().zip(buffer.chunks_exact_mut(2)) {
                out.copy_from_slice(&v.to_le_bytes());
            }
            hasher.update(&buffer[..chunk.len() * 2]);
        }
    }
    Some(hasher.finalize().into())
}
impl HeightReservations {
    pub fn new(grid: Grid<'_>, cancelled: impl Fn() -> bool) -> Result<Option<Self>, String> {
        let n = grid.resolution;
        if !(2..=4097).contains(&n) || grid.height.len() != n * n {
            return Err("Invalid height reservation grid".into());
        }
        let layout = Layout {
            size: grid.size,
            resolution: n,
            origin_y: grid.origin_y,
            vertical_span: grid.vertical_span,
        };
        path_terrain::encoded_height_quantum(layout)?;
        let Some(baseline) = digest(grid.height, &cancelled) else {
            return Ok(None);
        };
        Ok(Some(Self {
            layout,
            baseline,
            words: vec![0; (n * n).div_ceil(64)],
            samples: 0,
        }))
    }
    pub fn layout_matches(&self, grid: Grid<'_>) -> bool {
        self.layout.resolution == grid.resolution
            && self.layout.size.to_bits() == grid.size.to_bits()
            && self.layout.origin_y.to_bits() == grid.origin_y.to_bits()
            && self.layout.vertical_span.to_bits() == grid.vertical_span.to_bits()
    }
    /// Binding catches stale/foreign in-memory masks; this is not an asset
    /// receipt or a security boundary. The bitset itself is never deserialized.
    pub fn validate_binding(
        &self,
        layers: &Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if layers.height_resolution != self.layout.resolution
            || (layers.size as f32).to_bits() != self.layout.size.to_bits()
            || layers.height.len() != self.layout.resolution * self.layout.resolution
        {
            return Err("Foreign height reservation layout".into());
        }
        let Some(value) = digest(&layers.height, &cancelled) else {
            return Ok(false);
        };
        if value != self.baseline {
            return Err("Stale height reservation terrain".into());
        }
        Ok(true)
    }
    /// Check an immutable numerical fitting input against the same revision
    /// and coordinate layout used to capture these height dependencies.
    pub fn validate_grid_binding(
        &self,
        grid: Grid<'_>,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if !self.layout_matches(grid)
            || grid.height.len() != self.layout.resolution * self.layout.resolution
        {
            return Err("Foreign height reservation layout".into());
        }
        let Some(value) = digest(grid.height, &cancelled) else {
            return Ok(false);
        };
        if value != self.baseline {
            return Err("Stale height reservation terrain".into());
        }
        Ok(true)
    }
    pub fn reserve(&mut self, index: usize) -> Result<(), String> {
        if index >= self.layout.resolution * self.layout.resolution {
            return Err("Height reservation index outside grid".into());
        }
        let flag = 1_u64 << (index % 64);
        let word = &mut self.words[index / 64];
        if *word & flag == 0 {
            *word |= flag;
            self.samples += 1;
        }
        Ok(())
    }
    pub fn reserve_query(&mut self, position: [f32; 2]) -> Result<(), String> {
        for i in path_terrain::height_support(self.layout, [position[0], 0., position[1]])?.indices
        {
            self.reserve(i)?;
        }
        Ok(())
    }

    /// The caller supplies a nonempty, in-bounds interval on a grid row.
    fn reserve_row(&mut self, first: usize, last: usize) {
        let first_word = first / 64;
        let last_word = last / 64;
        let first_mask = u64::MAX << (first % 64);
        let last_mask = u64::MAX >> (63 - last % 64);
        if first_word == last_word {
            let word = &mut self.words[first_word];
            let add = first_mask & last_mask & !*word;
            self.samples += add.count_ones() as usize;
            *word |= add;
            return;
        }
        let mut words = self.words[first_word..=last_word].iter_mut();
        let first = words.next().unwrap();
        let last = words.next_back().unwrap();
        for (word, mask) in [(first, first_mask), (last, last_mask)] {
            let add = mask & !*word;
            self.samples += add.count_ones() as usize;
            *word |= add;
        }
        for word in words {
            self.samples += word.count_zeros() as usize;
            *word = u64::MAX;
        }
    }
    /// Reserve bilinear height support throughout a source Hermite corridor.
    /// Cubic Bezier hulls bound the complete curve, including bends between
    /// samples. Subdivision bounds raster work without relying on a sampling
    /// spacing that could miss terrain cells. On error/cancellation the caller
    /// must discard this private mask; partial reservations are not publishable.
    pub fn reserve_path(
        &mut self,
        path: &crate::path_geometry::Path,
        radius: f32,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<usize>, String> {
        if !radius.is_finite()
            || !(0. ..=1024.).contains(&radius)
            || !path.length().is_finite()
            || path.length() <= 0.
            || path.range() != (0, path.points().len() - 1)
            || !(1..=64_000_000).contains(&maximum_work)
        {
            return Err("Invalid height reservation corridor".into());
        }
        let mut work = 0usize;
        let mut spend = |count| -> Result<(), String> {
            work = work
                .checked_add(count)
                .filter(|v| *v <= maximum_work)
                .ok_or("Height reservation corridor work exhausted")?;
            Ok(())
        };
        let n = self.layout.resolution;
        let cell = f64::from(self.layout.size) / (n - 1) as f64;
        let mut stack = Vec::new();
        for i in 0..path.points().len() - 1 {
            if cancelled() {
                return Ok(None);
            }
            spend(16)?;
            let p = [path.points()[i], path.points()[i + 1]];
            // Match Path::point's f32 tangent scaling before conversion.
            let t = [path.tangents()[i], path.tangents()[i + 1]]
                .map(|v| v.map(|v| f64::from(v * path.step_size())));
            let a = [f64::from(p[0][0]), f64::from(p[0][2])];
            let b = [f64::from(p[1][0]), f64::from(p[1][2])];
            let curve = [
                a,
                [a[0] + t[0][0] / 3., a[1] + t[0][2] / 3.],
                [b[0] - t[1][0] / 3., b[1] - t[1][2] / 3.],
                b,
            ];
            // Bound the f32 Hermite evaluator's rounding as well as the real
            // cubic. The raster receives an additional support-cell margin.
            let scale = p
                .iter()
                .flatten()
                .fold(1_f64, |s, v| s.max(f64::from(v.abs())))
                + f64::from(path.step_size().abs());
            let guard = 128. * f64::from(f32::EPSILON) * scale;
            let radius = f64::from(radius) + guard;
            stack.push((curve, 0u8));
            while let Some((q, depth)) = stack.pop() {
                if cancelled() {
                    return Ok(None);
                }
                spend(16)?;
                let lo: [f64; 2] = std::array::from_fn(|axis| {
                    q.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min)
                });
                let hi: [f64; 2] = std::array::from_fn(|axis| {
                    q.iter().map(|p| p[axis]).fold(f64::NEG_INFINITY, f64::max)
                });
                if (hi[0] - lo[0]).max(hi[1] - lo[1]) > cell * 8. {
                    if depth == 24 {
                        return Err("Height reservation corridor subdivision exhausted".into());
                    }
                    let midpoint =
                        |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
                    let ab = midpoint(q[0], q[1]);
                    let bc = midpoint(q[1], q[2]);
                    let cd = midpoint(q[2], q[3]);
                    let abc = midpoint(ab, bc);
                    let bcd = midpoint(bc, cd);
                    let mid = midpoint(abc, bcd);
                    stack.push(([mid, bcd, cd, q[3]], depth + 1));
                    stack.push(([q[0], ab, abc, mid], depth + 1));
                    continue;
                }
                let index = |world: f64| (world + f64::from(self.layout.size) * 0.5) / cell;
                let lower: [usize; 2] =
                    lo.map(|v| (index(v - radius).floor() - 1.).clamp(0., (n - 1) as f64) as usize);
                let upper: [usize; 2] =
                    // Below the negative map edge the source sampler still
                    // addresses node one, even when its weight is zero.
                    hi.map(|v| (index(v + radius).ceil() + 1.).clamp(1., (n - 1) as f64) as usize);
                let width = upper[0] - lower[0] + 1;
                for z in lower[1]..=upper[1] {
                    if cancelled() {
                        return Ok(None);
                    }
                    spend(width)?;
                    self.reserve_row(z * n + lower[0], z * n + upper[0]);
                }
            }
        }
        Ok(Some(work))
    }
    pub fn contains(&self, index: usize) -> bool {
        index < self.layout.resolution * self.layout.resolution
            && self.words[index / 64] & (1_u64 << (index % 64)) != 0
    }
    pub fn samples(&self) -> usize {
        self.samples
    }
    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.words
            .iter()
            .copied()
            .enumerate()
            .flat_map(|(index, mut word)| {
                std::iter::from_fn(move || {
                    if word == 0 {
                        return None;
                    }
                    let bit = word.trailing_zeros() as usize;
                    word &= word - 1;
                    Some(index * 64 + bit)
                })
            })
    }
    pub fn bytes(&self) -> usize {
        self.words.len() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_ranges_and_sparse_iteration_match_scalar_reservations() {
        let height = [0; 257 * 257];
        let grid = Grid {
            size: 1000.,
            resolution: 257,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let mut rows = HeightReservations::new(grid, || false).unwrap().unwrap();
        let mut scalar = HeightReservations::new(grid, || false).unwrap().unwrap();
        for (first, last) in [
            (0, 0),
            (9, 27),
            (63, 64),
            (7, 210),
            (128, 256),
            (66_040, 66_048),
        ] {
            rows.reserve_row(first, last);
            for index in first..=last {
                scalar.reserve(index).unwrap();
            }
        }
        assert_eq!(rows.words, scalar.words);
        assert_eq!(rows.samples(), scalar.samples());
        assert!(
            rows.indices()
                .eq((0..height.len()).filter(|&index| scalar.contains(index)))
        );
    }

    #[test]
    fn reservation_digest_hashes_signed_heights_in_little_endian_order() {
        let samples: Vec<i16> = (-4096..=4096).collect();
        let bytes: Vec<_> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(
            digest(&samples, &|| false).unwrap().as_slice(),
            Md5::digest(bytes).as_slice()
        );
    }

    #[test]
    fn curved_corridors_cover_interior_shoulders_loops_and_clamped_edges() {
        use crate::path_geometry::Path;
        let height = vec![16383; 257 * 257];
        let grid = Grid {
            size: 1000.,
            resolution: 257,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let layout = Layout {
            size: grid.size,
            resolution: grid.resolution,
            origin_y: grid.origin_y,
            vertical_span: grid.vertical_span,
        };
        for controls in [
            vec![
                [-200., 0., -100.],
                [-90., 0., 80.],
                [80., 0., -90.],
                [200., 0., 100.],
            ],
            vec![
                [-450., 0., -450.],
                [0., 0., -480.],
                [450., 0., -450.],
                [450., 0., 450.],
                [-450., 0., -450.],
            ],
            vec![[-600., 0., -100.], [-495., 0., 0.], [-400., 0., 200.]],
        ] {
            let path = Path::from_controls(&controls, 10000, || false)
                .unwrap()
                .unwrap();
            let mut controls_only = HeightReservations::new(grid, || false).unwrap().unwrap();
            for p in &controls {
                controls_only.reserve_query([p[0], p[2]]).unwrap();
            }
            let mut mask = HeightReservations::new(grid, || false).unwrap().unwrap();
            let work = mask
                .reserve_path(&path, 11., 2_000_000, || false)
                .unwrap()
                .unwrap();
            assert!(work > mask.samples() && work < 2_000_000);
            let mut missed_by_controls = 0;
            // Dense independent support queries cover the entire disk around
            // source curve points, rather than repeating the hull rasterizer.
            for sample in 0..=2000 {
                let p = path.point(path.length() * sample as f32 / 2000.).unwrap();
                for r in [0., 5.5, 11.] {
                    for angle in 0..16 {
                        let a = angle as f32 * std::f32::consts::TAU / 16.;
                        let q = [p[0] + r * a.cos(), p[1], p[2] + r * a.sin()];
                        for i in path_terrain::height_support(layout, q).unwrap().indices {
                            assert!(mask.contains(i), "missing support {i} for {q:?}");
                            missed_by_controls += usize::from(!controls_only.contains(i));
                        }
                    }
                }
            }
            assert!(missed_by_controls > 1000);
            assert!(
                mask.samples() < height.len() / 3,
                "corridor must not freeze the full map"
            );
            assert!(!mask.contains(230 * 257 + 51));
        }
    }
    #[test]
    fn corridor_reservations_bound_work_inputs_and_mid_operation_cancellation() {
        use crate::path_geometry::Path;
        use std::cell::Cell;
        let height = vec![16383; 65 * 65];
        let grid = Grid {
            size: 1000.,
            resolution: 65,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &height,
        };
        let path = Path::from_controls(
            &[[-100., 0., -100.], [0., 0., 0.], [100., 0., 100.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        for radius in [f32::NAN, -1., 1025.] {
            let mut mask = HeightReservations::new(grid, || false).unwrap().unwrap();
            assert!(mask.reserve_path(&path, radius, 10000, || false).is_err());
            assert_eq!(mask.samples(), 0);
        }
        let mut mask = HeightReservations::new(grid, || false).unwrap().unwrap();
        assert!(mask.reserve_path(&path, 5., 1, || false).is_err());
        assert_eq!(mask.samples(), 0);
        let calls = Cell::new(0);
        assert!(
            mask.reserve_path(&path, 5., 100000, || {
                calls.set(calls.get() + 1);
                calls.get() == 18
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(calls.get(), 18);
        let mut partial = path.clone();
        partial.set_range(0, 1).unwrap();
        assert!(mask.reserve_path(&partial, 5., 100000, || false).is_err());
    }
    #[test]
    fn reservations_bind_to_the_initial_grid_and_bound_queries_and_cancellation() {
        let mut layers = Layers {
            size: 1000,
            height_resolution: 5,
            surface_resolution: 2,
            height: vec![16383; 25],
            topology: vec![0; 4],
            biome: vec![],
            splat: vec![],
            terrain_seed: 1,
            biome_seed: 2,
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let grid = Grid {
            size: 1000.,
            resolution: 5,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &layers.height,
        };
        assert!(HeightReservations::new(grid, || true).unwrap().is_none());
        let mut mask = HeightReservations::new(grid, || false).unwrap().unwrap();
        mask.reserve_query([0., 0.]).unwrap();
        assert_eq!(mask.samples(), 4);
        mask.reserve(12).unwrap();
        assert_eq!(mask.samples(), 4);
        assert!(mask.contains(12) && mask.contains(18));
        assert!(!mask.contains(0) && !mask.contains(usize::MAX));
        assert!(mask.reserve(25).is_err());
        assert!(mask.reserve_query([f32::NAN, 0.]).is_err());
        assert_eq!(mask.bytes(), 8);
        assert!(mask.validate_binding(&layers, || false).unwrap());
        assert!(!mask.validate_binding(&layers, || true).unwrap());
        layers.height[0] += 1;
        assert!(mask.validate_binding(&layers, || false).is_err());
    }
}
