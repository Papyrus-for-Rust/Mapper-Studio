//! Source-authored TerrainCheck and TerrainFilter evaluation at a unit-scale pose.
use crate::{
    dungeon_base::Pose,
    environment::Transform,
    monument_eligibility::Filter,
    native_layers::{View, dimensions},
    placement_map::PlacementMap,
    spawn,
    tunnel_network::{Failure, Meter},
};

#[derive(Clone, Debug)]
pub struct HeightCheck {
    pub position: [f32; 3],
    pub rotate: bool,
    pub extents: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub heights: Vec<HeightCheck>,
    pub filters: Vec<Filter>,
}
impl Rules {
    pub fn validate(&self) -> Result<(), String> {
        let position = |p: &[f32; 3]| p.iter().all(|v| v.is_finite() && v.abs() <= 10000.);
        if self.heights.len() > 4096 || self.filters.len() > 4096 {
            return Err("Too many prefab terrain attributes".into());
        }
        for check in &self.heights {
            if !position(&check.position)
                || !check.extents.is_finite()
                || !(0. ..=10000.).contains(&check.extents)
            {
                return Err("Invalid prefab terrain height check".into());
            }
        }
        for filter in &self.filters {
            filter.filter.validate()?;
            if !position(&filter.position)
                || !filter.radius.is_finite()
                || !(0. ..=256.).contains(&filter.radius)
            {
                return Err("Invalid prefab terrain filter".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default)]
pub struct Prepared(Rules);
impl Prepared {
    pub fn new(rules: Rules) -> Result<Self, String> {
        rules.validate()?;
        Ok(Self(rules))
    }
    pub fn rules(&self) -> &Rules {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Height,
    Bounds,
    Placement,
    Filter,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment {
    pub rejection: Option<Rejection>,
    pub work: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    Work(Failure),
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Work(value)
    }
}

pub struct Terrain<'a> {
    view: View<'a>,
    placement: Option<PlacementMap<'a>>,
}
impl<'a> Terrain<'a> {
    /// Shape validation is constant-time. Height values and boolean placement
    /// bytes are validated where sampled, preserving bounded per-pose work.
    pub fn new(view: View<'a>) -> Result<Self, String> {
        let (hr, n) = dimensions(view.size)?;
        if view.height_resolution != hr
            || view.surface_resolution != n
            || view.height.len() != hr * hr
            || view.topology.len() != n * n
            || view.biome.len() != n * n * 5
            || view.splat.len() != n * n * 8
        {
            return Err("Prefab terrain channels have mismatched dimensions".into());
        }
        let placement = view
            .placement_blockers
            .map(|cells| PlacementMap::new(view.size as f32, n, cells))
            .transpose()?;
        Ok(Self { view, placement })
    }
    pub fn evaluate(
        &self,
        rules: &Prepared,
        pose: Pose,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Assessment, Error> {
        let rules = rules.rules();
        Transform {
            position: pose.position,
            rotation: pose.rotation,
            scale: [1.; 3],
        }
        .validate()
        .map_err(Error::Invalid)?;
        if !(1..=128_000_000).contains(&maximum_work) {
            return Err(Error::Invalid(
                "Invalid prefab terrain work allowance".into(),
            ));
        }
        let mut meter = Meter {
            used: 0,
            limit: maximum_work,
            cancelled: &cancelled,
        };
        let size = self.view.size as f32;
        let normalize =
            |point: [f32; 3]| [point[0], point[2]].map(|v| (v + size * 0.5) * (1. / size));
        let rejected = |reason, work| {
            Ok(Assessment {
                rejection: Some(reason),
                work,
            })
        };
        for check in &rules.heights {
            meter.spend(1)?;
            let point = if check.rotate {
                pose.point(check.position)
            } else {
                std::array::from_fn(|i| pose.position[i] + check.position[i])
            };
            let [u, v] = normalize(point).map(|v| v.clamp(0., 1.));
            let height = spawn::sample_height_view(self.view, u, v).map_err(Error::Invalid)?;
            if point[1] - check.extents > height || point[1] + check.extents < height {
                return rejected(Rejection::Height, meter.used);
            }
        }
        let n = self.view.surface_resolution;
        let index = |v: f32| (v * n as f32).clamp(0., (n - 1) as f32) as usize;
        for filter in &rules.filters {
            meter.spend(1)?;
            let point = pose.point(filter.position);
            let [u, v] = normalize(point);
            if filter.check_bounds
                && (point[0] < -size * 0.5
                    || point[0] > size * 0.5
                    || point[2] < -size * 0.5
                    || point[2] > size * 0.5)
            {
                return rejected(Rejection::Bounds, meter.used);
            }
            if filter.check_placement
                && let Some(placement) = self.placement
            {
                meter.spend(1)?;
                if placement
                    .is_blocked([point[0], point[2]])
                    .map_err(Error::Invalid)?
                {
                    return rejected(Rejection::Placement, meter.used);
                }
            }
            let f = &filter.filter;
            let restricted = f.topology_any != -1 || f.topology_all != 0 || f.topology_not != 0;
            if restricted && filter.radius > 0. {
                let r = filter.radius / size;
                let [cx, cz] = [index(u), index(v)];
                let mut mask = 0;
                for z in index(v - r)..=index(v + r) {
                    for x in index(u - r)..=index(u + r) {
                        meter.spend(1)?;
                        let dx = x as f32 - cx as f32;
                        let dz = z as f32 - cz as f32;
                        if dx * dx + dz * dz <= filter.radius * filter.radius {
                            mask |= self.view.topology[z * n + x] as u32;
                            if mask & f.topology_not as u32 != 0 {
                                return rejected(Rejection::Filter, meter.used);
                            }
                        }
                    }
                }
                if !f.topology_matches(mask) {
                    return rejected(Rejection::Filter, meter.used);
                }
                let material = spawn::Filter {
                    topology_any: -1,
                    topology_all: 0,
                    topology_not: 0,
                    ..f.clone()
                };
                if spawn::factor_view(self.view, &material, u, v) <= 0. {
                    return rejected(Rejection::Filter, meter.used);
                }
            } else if spawn::factor_view(self.view, f, u, v) <= 0. {
                return rejected(Rejection::Filter, meter.used);
            }
        }
        meter.spend(1)?;
        Ok(Assessment {
            rejection: None,
            work: meter.used,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_layers::Layers;
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let mut biome = vec![0; n * n * 5];
        biome[..n * n].fill(255);
        let mut splat = vec![0; n * n * 8];
        splat[..n * n].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16383; hr * hr],
            biome,
            splat,
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn pose() -> Pose {
        Pose {
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
        }
    }
    fn filter() -> Filter {
        Filter {
            position: [0.; 3],
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: -1,
                topology_all: 0,
                topology_not: 0,
            },
            radius: 0.,
            check_bounds: false,
            check_placement: true,
        }
    }
    fn run(view: View<'_>, rules: Rules, pose: Pose, maximum: usize) -> Result<Assessment, Error> {
        Terrain::new(view)
            .unwrap()
            .evaluate(&Prepared::new(rules).unwrap(), pose, maximum, || false)
    }
    #[test]
    fn height_checks_use_rotate_flag_inclusive_extents_and_clamped_samples() {
        let layers = layers();
        let mut rules = Rules {
            heights: vec![HeightCheck {
                position: [0., 0., 10.],
                rotate: true,
                extents: 0.001,
            }],
            ..Default::default()
        };
        let half = std::f32::consts::FRAC_PI_4;
        let pose = Pose {
            position: [0., 10., 0.],
            rotation: [half.sin(), 0., 0., half.cos()],
        };
        assert_eq!(
            run(layers.view(), rules.clone(), pose, 10)
                .unwrap()
                .rejection,
            None
        );
        rules.heights[0].rotate = false;
        assert_eq!(
            run(layers.view(), rules.clone(), pose, 10)
                .unwrap()
                .rejection,
            Some(Rejection::Height)
        );
        rules.heights[0].extents = 10.;
        assert_eq!(
            run(layers.view(), rules.clone(), pose, 10)
                .unwrap()
                .rejection,
            None
        );
        rules.heights[0].position = [900., 0., 0.];
        assert_eq!(run(layers.view(), rules, pose, 10).unwrap().rejection, None);
    }
    #[test]
    fn bounds_flag_uses_world_comparisons_and_unchecked_filters_clamp_edges() {
        let layers = layers();
        let mut f = filter();
        f.position[0] = f32::from_bits(500_f32.to_bits() + 1);
        let mut rules = Rules {
            filters: vec![f],
            ..Default::default()
        };
        assert_eq!(
            run(layers.view(), rules.clone(), pose(), 10)
                .unwrap()
                .rejection,
            None
        );
        rules.filters[0].check_bounds = true;
        assert_eq!(
            run(layers.view(), rules.clone(), pose(), 10)
                .unwrap()
                .rejection,
            Some(Rejection::Bounds)
        );
        rules.filters[0].position[0] = 500.;
        assert_eq!(
            run(layers.view(), rules, pose(), 10).unwrap().rejection,
            None
        );
    }
    #[test]
    fn placement_flag_uses_source_normalization_and_validates_only_queried_bytes() {
        let layers = layers();
        let n = layers.surface_resolution;
        let mut blocked = vec![0; n * n];
        blocked[n / 2 * n + n / 4] = 1;
        let mut view = layers.view();
        view.placement_blockers = Some(&blocked);
        let mut f = filter();
        f.position[0] = f32::from_bits((-250_f32).to_bits() + 1);
        let mut rules = Rules {
            filters: vec![f],
            ..Default::default()
        };
        assert_eq!(
            run(view, rules.clone(), pose(), 10).unwrap().rejection,
            Some(Rejection::Placement)
        );
        rules.filters[0].check_placement = false;
        assert_eq!(
            run(view, rules.clone(), pose(), 10).unwrap().rejection,
            None
        );
        blocked[n / 2 * n + n / 4] = 2;
        let mut view = layers.view();
        view.placement_blockers = Some(&blocked);
        rules.filters[0].check_placement = true;
        assert!(matches!(
            run(view, rules, pose(), 10),
            Err(Error::Invalid(_))
        ));
    }
    #[test]
    fn topology_radius_and_material_filters_follow_independent_source_predicates() {
        let mut layers = layers();
        let n = layers.surface_resolution;
        layers.topology[n / 2 * n + n / 2 + 1] = 2;
        let mut f = filter();
        f.filter.topology_any = 2;
        let mut rules = Rules {
            filters: vec![f],
            ..Default::default()
        };
        assert_eq!(
            run(layers.view(), rules.clone(), pose(), 100)
                .unwrap()
                .rejection,
            Some(Rejection::Filter)
        );
        rules.filters[0].radius = 3.5;
        assert_eq!(
            run(layers.view(), rules.clone(), pose(), 100)
                .unwrap()
                .rejection,
            None
        );
        rules.filters[0].filter.topology_not = 2;
        assert_eq!(
            run(layers.view(), rules.clone(), pose(), 100)
                .unwrap()
                .rejection,
            Some(Rejection::Filter)
        );
        rules.filters[0].filter.topology_not = 0;
        rules.filters[0].filter.splat_type = 2;
        assert_eq!(
            run(layers.view(), rules.clone(), pose(), 100)
                .unwrap()
                .rejection,
            Some(Rejection::Filter)
        );
        rules.filters[0].filter.splat_type = 1;
        rules.filters[0].filter.biome_type = 2;
        assert_eq!(
            run(layers.view(), rules, pose(), 100).unwrap().rejection,
            Some(Rejection::Filter)
        );
    }
    #[test]
    fn unrestricted_radius_filters_skip_topology_work_and_failures_publish_no_verdict() {
        let layers = layers();
        let mut f = filter();
        f.radius = 256.;
        let rules = Prepared::new(Rules {
            filters: vec![f],
            ..Default::default()
        })
        .unwrap();
        let terrain = Terrain::new(layers.view()).unwrap();
        assert_eq!(
            terrain.evaluate(&rules, pose(), 2, || false).unwrap().work,
            2
        );
        assert!(matches!(
            terrain.evaluate(&rules, pose(), 1, || false),
            Err(Error::Work(Failure::WorkLimit))
        ));
        assert!(matches!(
            terrain.evaluate(&rules, pose(), 10, || true),
            Err(Error::Work(Failure::Cancelled))
        ));
        assert!(
            Prepared::new(Rules {
                heights: vec![HeightCheck {
                    position: [f32::NAN, 0., 0.],
                    rotate: false,
                    extents: 1.
                }],
                ..Default::default()
            })
            .is_err()
        );
        let mut bad = layers.view();
        bad.topology = &[];
        assert!(Terrain::new(bad).is_err());
    }
}
