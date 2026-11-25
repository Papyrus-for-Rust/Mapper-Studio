//! Bounded depth/reachability raster for independent coastal generation.
//! Callers supply water heights; no terrain holes, water colliders or volumes are inferred.
use crate::{
    native_layers::{Layers, dimensions},
    spawn,
};
use std::collections::VecDeque;
pub const VERSION: &str = "papyrus-boat-depth-navigation-1";
pub const SOURCE_VERSION: &str = "papyrus-boat-depth-navigation-source-2";
#[derive(Debug)]
pub struct Navigation {
    size: f32,
    resolution: usize,
    source_positions: bool,
    reachable: Vec<bool>,
    pub deep_cells: usize,
    pub reachable_cells: usize,
}
pub struct RouteSupport {
    /// Normalized sampling positions, including diagonal clearance cells.
    pub samples: Vec<[f32; 2]>,
    pub work: usize,
}

fn flood_reachable(
    deep: &[bool],
    n: usize,
    cancelled: impl Fn() -> bool,
) -> Option<(Vec<bool>, usize)> {
    let mut reachable = vec![false; deep.len()];
    let mut queue = VecDeque::new();
    let enqueue = |index: usize, reachable: &mut [bool], queue: &mut VecDeque<usize>| {
        if deep[index] && !reachable[index] {
            reachable[index] = true;
            queue.push_back(index);
        }
    };
    let last = n - 1;
    let mid = n / 2;
    for seed in [
        0,
        mid * n,
        last * n,
        mid,
        last * n + mid,
        last,
        mid * n + last,
        n * n - 1,
    ] {
        enqueue(seed, &mut reachable, &mut queue);
    }
    let mut visits = 0_usize;
    while let Some(index) = queue.pop_front() {
        visits += 1;
        if visits.is_multiple_of(256) && cancelled() {
            return None;
        }
        // Permitted diagonals have two deep orthogonal support cells, so
        // dropping their edges leaves reachability unchanged.
        let x = index % n;
        if x != 0 {
            enqueue(index - 1, &mut reachable, &mut queue);
        }
        if x != last {
            enqueue(index + 1, &mut reachable, &mut queue);
        }
        if index >= n {
            enqueue(index - n, &mut reachable, &mut queue);
        }
        if index < last * n {
            enqueue(index + n, &mut reachable, &mut queue);
        }
    }
    (!cancelled()).then_some((reachable, visits))
}

impl Navigation {
    /// TerrainPath.CreateBoatCostmap denormalizes cell centers, then the
    /// TerrainHeightMap normalizes and interpolates encoded heights before its
    /// world-height conversion. Retain that operation order at depth thresholds.
    pub fn build_source(
        layers: &Layers,
        cell_size: f32,
        minimum_depth: f32,
        water: impl Fn(f32, f32) -> Option<f32>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let (hr, _) = dimensions(layers.size)?;
        if layers.height_resolution != hr
            || layers.height.len() != hr * hr
            || layers.height.iter().any(|h| !(0..=32766).contains(h))
        {
            return Err("Invalid source boat-navigation terrain".into());
        }
        let grid = crate::path_terrain::Grid {
            size: layers.size as f32,
            resolution: hr,
            origin_y: -500.,
            vertical_span: 1000.,
            height: &layers.height,
        };
        Self::build_source_sampled(
            layers.size,
            cell_size,
            minimum_depth,
            |p| crate::path_terrain::sample_height(grid, p).ok(),
            water,
            cancelled,
        )
    }
    /// Lazy candidate stamps use exactly the same world-position sampling as
    /// committed terrain, without allocating a full candidate height raster.
    pub fn build_source_sampled(
        size: u32,
        cell_size: f32,
        minimum_depth: f32,
        height: impl Fn([f32; 3]) -> Option<f32>,
        water: impl Fn(f32, f32) -> Option<f32>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::build_with_sampling(
            size,
            cell_size,
            minimum_depth,
            true,
            |_, _, p| height(p),
            water,
            cancelled,
        )
    }
    pub fn build(
        layers: &Layers,
        cell_size: f32,
        minimum_depth: f32,
        water: impl Fn(f32, f32) -> Option<f32>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let (hr, _) = dimensions(layers.size)?;
        if layers.height_resolution != hr
            || layers.height.len() != hr * hr
            || !cell_size.is_finite()
            || !(2. ..=64.).contains(&cell_size)
            || !minimum_depth.is_finite()
            || !(0.1..=100.).contains(&minimum_depth)
            || layers.height.iter().any(|h| !(0..=32766).contains(h))
        {
            return Err("Invalid boat-navigation terrain or policy".into());
        }
        Self::build_sampled(
            layers.size,
            cell_size,
            minimum_depth,
            |u, v| Some(spawn::height(layers, u, v)),
            water,
            cancelled,
        )
    }
    pub fn build_sampled(
        size: u32,
        cell_size: f32,
        minimum_depth: f32,
        height: impl Fn(f32, f32) -> Option<f32>,
        water: impl Fn(f32, f32) -> Option<f32>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        Self::build_with_sampling(
            size,
            cell_size,
            minimum_depth,
            false,
            |u, v, _| height(u, v),
            water,
            cancelled,
        )
    }
    fn build_with_sampling(
        size: u32,
        cell_size: f32,
        minimum_depth: f32,
        source_positions: bool,
        height: impl Fn(f32, f32, [f32; 3]) -> Option<f32>,
        water: impl Fn(f32, f32) -> Option<f32>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        dimensions(size)?;
        if !cell_size.is_finite()
            || !(2. ..=64.).contains(&cell_size)
            || !minimum_depth.is_finite()
            || !(0.1..=100.).contains(&minimum_depth)
        {
            return Err("Invalid sampled boat-navigation policy".into());
        }
        let n = (size as f32 / cell_size) as usize;

        if !(2..=800).contains(&n) {
            return Err("Boat-navigation resolution exceeds supported bounds".into());
        }
        let mut deep = vec![false; n * n];
        let mut deep_cells = 0;
        let size = size as f32;
        for z in 0..n {
            if cancelled() {
                return Ok(None);
            }
            for x in 0..n {
                let u = (x as f32 + 0.5) / n as f32;
                let v = (z as f32 + 0.5) / n as f32;
                let position = if source_positions {
                    [-size * 0.5 + u * size, 0., -size * 0.5 + v * size]
                } else {
                    [(u - 0.5) * size, 0., (v - 0.5) * size]
                };
                if let Some(h) = water(position[0], position[2]) {
                    if !h.is_finite() || !(-500. ..=500.).contains(&h) {
                        return Err("Invalid boat water surface".into());
                    }
                    let Some(ground) = height(u, v, position) else {
                        continue;
                    };
                    if !ground.is_finite() || !(-500. ..=500.).contains(&ground) {
                        return Err("Invalid boat terrain sample".into());
                    }
                    if h - ground >= minimum_depth {
                        deep[z * n + x] = true;
                        deep_cells += 1;
                    }
                }
            }
        }
        let Some((reachable, reachable_cells)) = flood_reachable(&deep, n, cancelled) else {
            return Ok(None);
        };
        Ok(Some(Self {
            size,
            resolution: n,
            source_positions,
            reachable,
            deep_cells,
            reachable_cells,
        }))
    }
    pub fn resolution(&self) -> usize {
        self.resolution
    }
    pub fn size(&self) -> u32 {
        self.size as u32
    }
    pub fn reaches_edge(&self, point: [f32; 3]) -> bool {
        self.cell_index(point)
            .is_some_and(|index| self.reachable[index])
    }
    fn cell_index(&self, point: [f32; 3]) -> Option<usize> {
        if point.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let [u, v] = [point[0], point[2]].map(|p| {
            if self.source_positions {
                (p + self.size * 0.5) * (1. / self.size)
            } else {
                p / self.size + 0.5
            }
        });
        if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
            return None;
        }
        let x = (u * self.resolution as f32)
            .floor()
            .min((self.resolution - 1) as f32) as usize;
        let z = (v * self.resolution as f32)
            .floor()
            .min((self.resolution - 1) as f32) as usize;
        Some(z * self.resolution + x)
    }
    /// Find one retained path to the same eight edge seeds used by navigation.
    /// Protecting its samples preserves this raster's accepted depth/connectivity.
    pub fn route_support(
        &self,
        point: [f32; 3],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<RouteSupport>, String> {
        let start = self
            .cell_index(point)
            .filter(|&index| self.reachable[index])
            .ok_or("Boat connector has no accepted navigation route")?;
        let n = self.resolution;
        let mut work = n * n;
        if work > maximum_work {
            return Err("Boat route support work exhausted".into());
        }
        let mut parents = vec![usize::MAX; n * n];
        parents[start] = start;
        let mut queue = VecDeque::from([start]);
        while let Some(at) = queue.pop_front() {
            if cancelled() {
                return Ok(None);
            }
            if 9 > maximum_work.saturating_sub(work) {
                return Err("Boat route support work exhausted".into());
            }
            work += 9;
            let [x, z] = [at % n, at / n];
            let edge = |v| v == 0 || v == n - 1;
            if (edge(x) && (edge(z) || z == n / 2)) || (x == n / 2 && edge(z)) {
                let mut samples = Vec::new();
                let mut next = at;
                loop {
                    if cancelled() {
                        return Ok(None);
                    }
                    if 12 > maximum_work.saturating_sub(work) {
                        return Err("Boat route support work exhausted".into());
                    }
                    work += 12;
                    let parent = parents[next];
                    let a = [next % n, next / n];
                    let b = [parent % n, parent / n];
                    for cell in [a, [a[0], b[1]], [b[0], a[1]]] {
                        samples.push(cell.map(|v| (v as f32 + 0.5) / n as f32));
                    }
                    if next == start {
                        return Ok(Some(RouteSupport { samples, work }));
                    }
                    next = parent;
                }
            }
            for dz in -1_isize..=1 {
                for dx in -1_isize..=1 {
                    let a = x.checked_add_signed(dx).filter(|&v| v < n);
                    let b = z.checked_add_signed(dz).filter(|&v| v < n);
                    let (Some(a), Some(b)) = (a, b) else { continue };
                    let index = b * n + a;
                    if self.reachable[index]
                        && parents[index] == usize::MAX
                        && (dx == 0
                            || dz == 0
                            || (self.reachable[z * n + a] && self.reachable[b * n + x]))
                    {
                        parents[index] = at;
                        queue.push_back(index);
                    }
                }
            }
        }
        Err("Accepted boat navigation lost its edge route".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn four_neighbor_flood_matches_supported_diagonals() {
        let n = 4;
        for pattern in 0..(1 << (n * n)) {
            let deep: Vec<_> = (0..n * n).map(|i| pattern & (1 << i) != 0).collect();
            let mut expected = vec![false; deep.len()];
            for (x, z) in [
                (0, 0), (0, 2), (0, 3), (2, 0), (2, 3), (3, 0), (3, 2), (3, 3),
            ] {
                expected[z * n + x] = deep[z * n + x];
            }
            loop {
                let before = expected.clone();
                for i in 0..deep.len() {
                    if before[i] {
                        let (x, z) = (i % n, i / n);
                        for a in x.saturating_sub(1)..=(x + 1).min(n - 1) {
                            for b in z.saturating_sub(1)..=(z + 1).min(n - 1) {
                                if deep[z * n + a] && deep[b * n + x] && deep[b * n + a] {
                                    expected[b * n + a] = true;
                                }
                            }
                        }
                    }
                }
                if expected == before {
                    break;
                }
            }
            let (reachable, count) = flood_reachable(&deep, n, || false).unwrap();
            assert_eq!(reachable, expected);
            assert_eq!(count, expected.iter().filter(|&&cell| cell).count());
        }
    }
    #[test]
    fn protected_route_keeps_depth_and_diagonal_clearance_after_unrelated_height_edits() {
        let (hr, sr) = dimensions(1000).unwrap();
        let encode = |height: f32| ((height + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![encode(-10.); hr * hr],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let nav = Navigation::build_source(&layers, 25., 4., |_, _| Some(0.), || false)
            .unwrap()
            .unwrap();
        let point = [75., 0., -75.];
        let route = nav
            .route_support(point, 1_000_000, || false)
            .unwrap()
            .unwrap();
        let mut mask = crate::height_reservations::HeightReservations::new(
            crate::path_terrain::Grid {
                size: 1000.,
                resolution: hr,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &layers.height,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        for uv in &route.samples {
            mask.reserve_query(uv.map(|v| -500. + v * 1000.)).unwrap();
        }
        assert!(mask.samples() < hr * hr / 100);
        for (index, height) in layers.height.iter_mut().enumerate() {
            if !mask.contains(index) {
                *height = encode(20.);
            }
        }
        let retained = Navigation::build_source(&layers, 25., 4., |_, _| Some(0.), || false)
            .unwrap()
            .unwrap();
        assert!(retained.reaches_edge(point));
        assert!(retained.reachable_cells < nav.reachable_cells);
        assert!(nav.route_support(point, 1, || false).is_err());
        assert!(
            nav.route_support([501., 0., 0.], 1_000_000, || false)
                .is_err()
        );
        assert!(
            nav.route_support(point, 1_000_000, || true)
                .unwrap()
                .is_none()
        );
        assert!(route.work <= 1_000_000);
    }
    #[test]
    fn source_query_normalization_selects_the_server_cell_at_a_float_boundary() {
        let nav = |source_positions| {
            let mut reachable = vec![false; 133 * 133];
            reachable[66 * 133 + 12] = true;
            Navigation {
                size: 1000.,
                resolution: 133,
                source_positions,
                reachable,
                deep_cells: 1,
                reachable_cells: 1,
            }
        };
        let point = [-409.77444_f32, 0., 0.];
        assert!(!nav(false).reaches_edge(point));
        assert!(nav(true).reaches_edge(point));
        assert!(!nav(true).reaches_edge([501., 0., 0.]));
        assert!(!nav(true).reaches_edge([f32::NAN, 0., 0.]));
    }
    #[test]
    fn source_height_operation_order_decides_the_exact_four_metre_depth_boundary() {
        let (hr, sr) = dimensions(1000).unwrap();
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![15000; hr * hr],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let u = (108_f32 + 0.5) / 133.;
        let v = (127_f32 + 0.5) / 133.;
        let point = [-500. + u * 1000., 0., -500. + v * 1000.];
        let layout = crate::path_terrain::Layout {
            size: 1000.,
            resolution: hr,
            origin_y: -500.,
            vertical_span: 1000.,
        };
        let support = crate::path_terrain::height_support(layout, point).unwrap();
        for (index, value) in support
            .indices
            .into_iter()
            .zip([16266, 16271, 16245, 16241])
        {
            layers.height[index] = value;
        }
        assert!(spawn::height(&layers, u, v) > -4.);
        let source = crate::path_terrain::sample_height(
            crate::path_terrain::Grid {
                size: 1000.,
                resolution: hr,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &layers.height,
            },
            point,
        )
        .unwrap();
        assert_eq!(source.to_bits(), (-4_f32).to_bits());
        let old = Navigation::build(&layers, 7.5, 4., |_, _| Some(0.), || false)
            .unwrap()
            .unwrap();
        let new = Navigation::build_source(&layers, 7.5, 4., |_, _| Some(0.), || false)
            .unwrap()
            .unwrap();
        assert!(!old.reaches_edge(point));
        assert!(new.reaches_edge(point));
        assert_eq!(new.deep_cells, old.deep_cells + 1);
        assert!(
            Navigation::build_source(&layers, 7.5, 4., |_, _| Some(0.), || true)
                .unwrap()
                .is_none()
        );
        assert!(
            Navigation::build_source(&layers, 7.5, 4., |_, _| Some(f32::NAN), || false).is_err()
        );
    }
    #[test]
    fn deep_ocean_connects_but_inland_basins_and_shallow_channels_do_not() {
        let (hr, sr) = dimensions(1000).unwrap();
        let mut l = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![crate::erosion::encode(-10.); hr * hr],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let nav = Navigation::build(&l, 7.5, 4., |_, _| Some(0.), || false)
            .unwrap()
            .unwrap();
        assert!(nav.reaches_edge([0., 0., 0.]));
        assert!(!nav.reaches_edge([501., 0., 0.]));
        for z in 0..hr {
            for x in 0..hr {
                let wx = (x as f32 / (hr - 1) as f32 - 0.5) * 1000.;
                let wz = (z as f32 / (hr - 1) as f32 - 0.5) * 1000.;
                if (100. ..=140.).contains(&wx.abs().max(wz.abs())) {
                    l.height[z * hr + x] = crate::erosion::encode(-2.);
                }
            }
        }
        let nav = Navigation::build(&l, 7.5, 4., |_, _| Some(0.), || false)
            .unwrap()
            .unwrap();
        assert!(!nav.reaches_edge([0., 0., 0.]));
        assert!(nav.reaches_edge([300., 0., 0.]));
        assert!(
            Navigation::build(&l, 7.5, 4., |_, _| Some(0.), || true)
                .unwrap()
                .is_none()
        );
        assert!(Navigation::build(&l, 0., 4., |_, _| Some(0.), || false).is_err());
        assert!(Navigation::build(&l, 7.5, 4., |_, _| Some(f32::NAN), || false).is_err());
    }
}
