//! Dry-land region preference for a connected road network.
//! Region membership does not certify a route through slopes, stamps or objects.
use crate::{native_layers::Layers, rail_costmap, road_costmap::CellLattice};

pub struct Access {
    lattice: CellLattice,
    labels: Vec<u32>,
    primary: u32,
    pub work: usize,
}
impl Access {
    pub fn prepare(
        layers: &Layers,
        lattice: CellLattice,
        minimum_height: f32,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let n = lattice.resolution();
        let hr = layers.height_resolution;
        if lattice.size() != layers.size as f32
            || !(2..=4097).contains(&hr)
            || layers.height.len() != hr * hr
            || !minimum_height.is_finite()
        {
            return Err("Invalid road land-access inputs".into());
        }
        // Normal sampling has a fixed bounded stencil. Reserve its reads and
        // all eight flood edges before allocating or traversing the raster.
        let work = hr * hr + n * n * 40;
        if work > maximum_work {
            return Err("Road land-access preparation exceeds its allowance".into());
        }
        for chunk in layers.height.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|h| !(0..=32766).contains(h)) {
                return Err("Invalid road land-access height encoding".into());
            }
        }
        let mut labels = vec![u32::MAX; n * n];
        for (i, label) in labels.iter_mut().enumerate() {
            if i % 1024 == 0 && cancelled() {
                return Ok(None);
            }
            let uv = lattice
                .normalized([i % n, i / n])
                .ok_or("Invalid land-access cell")?;
            let (height, _) =
                rail_costmap::sample_height_normal(lattice.size(), hr, &layers.height, uv);
            if height > minimum_height {
                *label = 0;
            }
        }
        let mut frontier = Vec::new();
        let mut next_label = 0;
        let mut primary = u32::MAX;
        let mut largest = 0;
        for start in 0..labels.len() {
            if cancelled() {
                return Ok(None);
            }
            if labels[start] != 0 {
                continue;
            }
            next_label += 1;
            labels[start] = next_label;
            frontier.push(start);
            let mut count = 0;
            while let Some(at) = frontier.pop() {
                if count % 1024 == 0 && cancelled() {
                    return Ok(None);
                }
                count += 1;
                let [x, z] = [at % n, at / n];
                for (dx, dz) in [
                    (-1, -1),
                    (0, -1),
                    (1, -1),
                    (-1, 0),
                    (1, 0),
                    (-1, 1),
                    (0, 1),
                    (1, 1),
                ] {
                    let a = x.checked_add_signed(dx).filter(|&v| v < n);
                    let b = z.checked_add_signed(dz).filter(|&v| v < n);
                    if let (Some(a), Some(b)) = (a, b) {
                        let index = b * n + a;
                        if labels[index] == 0 {
                            labels[index] = next_label;
                            frontier.push(index);
                        }
                    }
                }
            }
            if count > largest {
                primary = next_label;
                largest = count;
            }
        }
        Ok(Some(Self {
            lattice,
            labels,
            primary,
            work,
        }))
    }

    pub fn admits(&self, point: [f32; 2]) -> bool {
        if self.primary == u32::MAX
            || point
                .iter()
                .any(|v| !v.is_finite() || v.abs() > self.lattice.size() * 0.5)
        {
            return false;
        }
        self.lattice.cell(point).is_ok_and(|cell| {
            self.labels[cell[1] * self.lattice.resolution() + cell[0]] == self.primary
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_dry_islands_do_not_join_the_primary_network_and_failures_publish_nothing() {
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 17,
            surface_resolution: 16,
            height: vec![encode(-10.); 17 * 17],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in 1..16 {
            for x in 1..8 {
                layers.height[z * 17 + x] = encode(20.);
            }
        }
        for z in 6..10 {
            for x in 12..15 {
                layers.height[z * 17 + x] = encode(20.);
            }
        }
        let lattice = CellLattice::new(1000., 50.).unwrap();
        let before = layers.height.clone();
        let access = Access::prepare(&layers, lattice, 1., 100000, || false)
            .unwrap()
            .unwrap();
        assert!(access.admits([-250., 0.]));
        assert!(!access.admits([300., 0.]));
        assert!(!access.admits([f32::NAN, 0.]));
        assert!(!access.admits([1000., 0.]));
        assert_eq!(before, layers.height);
        assert!(Access::prepare(&layers, lattice, 1., 1, || false).is_err());
        assert!(
            Access::prepare(&layers, lattice, 1., 100000, || true)
                .unwrap()
                .is_none()
        );
        layers.height.fill(encode(-10.));
        assert!(
            !Access::prepare(&layers, lattice, 1., 100000, || false)
                .unwrap()
                .unwrap()
                .admits([0., 0.])
        );
    }
}
