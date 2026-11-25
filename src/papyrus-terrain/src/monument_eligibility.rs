//! Source-data checks for unit-scale, yaw-only monument proposals.
//! Stage-wide policy, placement-map occupancy and physics remain separate checks.
use crate::{monument_bounds::Footprint, native_layers::Layers, spawn};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub position: [f32; 3],
    pub extents: f32,
    pub offset: f32,
    pub radius: f32,
    pub slope_scale: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub position: [f32; 3],
    pub filter: spawn::Filter,
    pub radius: f32,
    pub check_bounds: bool,
    pub check_placement: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub tier_mask: u32,
    pub min_corners: usize,
    pub anchors: Vec<Anchor>,
    pub filters: Vec<Filter>,
    pub unsupported: Vec<String>,
    pub pending: Vec<String>,
}
impl Rules {
    pub fn validate(&self) -> Result<(), String> {
        let valid = |v: f32| v.is_finite() && v.abs() <= 10000.;
        if !(1..=4).contains(&self.min_corners)
            || self.anchors.len() > 4096
            || self.filters.len() > 4096
            || self.unsupported.len() > 256
            || self.pending.len() > 256
        {
            return Err("Invalid monument eligibility limits".into());
        }
        for a in &self.anchors {
            if !a
                .position
                .iter()
                .copied()
                .chain([a.extents, a.offset, a.radius, a.slope_scale])
                .all(valid)
                || a.extents < 0.
                || !(0. ..=256.).contains(&a.radius)
            {
                return Err("Invalid terrain anchor".into());
            }
        }
        for f in &self.filters {
            f.filter.validate()?;
            if !f.position.iter().copied().all(valid)
                || !valid(f.radius)
                || !(0. ..=256.).contains(&f.radius)
            {
                return Err("Invalid monument terrain filter".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, PartialEq)]
pub enum Check {
    Accepted([f32; 3]),
    Rejected(&'static str),
    Budget,
}
fn uv(size: u32, p: [f32; 3]) -> Option<(f32, f32)> {
    let u = p[0] / size as f32 + 0.5;
    let v = p[2] / size as f32 + 0.5;
    (p.iter().all(|v| v.is_finite()) && (0. ..=1.).contains(&u) && (0. ..=1.).contains(&v))
        .then_some((u, v))
}
fn index(n: usize, u: f32) -> usize {
    (u * n as f32).clamp(0., (n - 1) as f32) as usize
}
fn world(p: [f32; 3], origin: [f32; 3], (s, c): (f32, f32)) -> [f32; 3] {
    [
        origin[0] + c * p[0] - s * p[2],
        origin[1] + p[1],
        origin[2] + s * p[0] + c * p[2],
    ]
}
fn visit(work: &mut usize, limit: usize) -> bool {
    if *work >= limit {
        false
    } else {
        *work += 1;
        true
    }
}
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    layers: &Layers,
    rules: &Rules,
    bounds: Footprint,
    pos: [f32; 3],
    yaw: f32,
    work: &mut usize,
    limit: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Check>, String> {
    let Some(prepared) = Evaluator::new(layers, rules, bounds, &cancelled)? else {
        return Ok(None);
    };
    prepared.evaluate(pos, yaw, work, limit, cancelled)
}
/// Borrowed, immutable validated context for dense proposal filtering. The
/// public one-shot entry point retains the same checks and arithmetic.
pub struct Evaluator<'a> {
    layers: &'a Layers,
    rules: &'a Rules,
    bounds: Footprint,
    stage_filter: Option<&'a spawn::Filter>,
    placement_map: Option<crate::placement_map::PlacementMap<'a>>,
}
impl<'a> Evaluator<'a> {
    pub fn new(
        layers: &'a Layers,
        rules: &'a Rules,
        bounds: Footprint,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        // This is also a public engine boundary: callers need not have loaded rules
        // through the installation snapshot validator first. Reject malformed data
        // before it can influence an accepted pose (or an index/radius scan).
        rules.validate()?;
        if cancelled() {
            return Ok(None);
        }
        let (hr, n) = crate::native_layers::dimensions(layers.size)?;
        if layers.height.len() != hr * hr
            || layers.height_resolution != hr
            || layers.surface_resolution != n
            || layers.topology.len() != n * n
            || layers.splat.len() != n * n * 8
            || layers.biome.len() != n * n * 5
            || !bounds.valid()
            || !(1..=4).contains(&rules.min_corners)
        {
            return Err("Monument eligibility grid/bounds mismatch".into());
        }
        Ok(Some(Self {
            layers,
            rules,
            bounds,
            stage_filter: None,
            placement_map: None,
        }))
    }
    /// PlaceMonuments passes its stage filter to every TerrainAnchor, in
    /// addition to its probabilistic check at the proposed root.
    pub fn with_stage_filter(mut self, filter: &'a spawn::Filter) -> Result<Self, String> {
        filter.validate()?;
        self.stage_filter = Some(filter);
        Ok(self)
    }
    pub fn with_placement_map(
        mut self,
        map: crate::placement_map::PlacementMap<'a>,
    ) -> Result<Self, String> {
        if !map.matches(self.layers.size as f32, self.layers.surface_resolution) {
            return Err("Monument placement raster layout differs".into());
        }
        self.placement_map = Some(map);
        Ok(self)
    }
    pub fn evaluate(
        &self,
        mut pos: [f32; 3],
        yaw: f32,
        work: &mut usize,
        limit: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Check>, String> {
        if cancelled() {
            return Ok(None);
        }
        if !yaw.is_finite() || !pos.iter().all(|v| v.is_finite()) {
            return Err("Monument eligibility grid/bounds mismatch".into());
        }
        let (layers, rules, bounds) = (self.layers, self.rules, self.bounds);
        let n = layers.surface_resolution;
        let hr = layers.height_resolution;
        if !rules.unsupported.is_empty() {
            return Ok(Some(Check::Rejected(
                "Unsupported required monument attributes",
            )));
        }
        let heading = yaw.to_radians().sin_cos();
        for f in &rules.filters {
            if cancelled() {
                return Ok(None);
            }
            let p = world(f.position, pos, heading);
            let Some((u, v)) = uv(layers.size, p) else {
                return Ok(Some(Check::Rejected("Terrain filter lies outside the map")));
            };
            if !visit(work, limit) {
                return Ok(Some(Check::Budget));
            }
            if let Some(map) = self.placement_map.filter(|_| f.check_placement) {
                if !visit(work, limit) {
                    return Ok(Some(Check::Budget));
                }
                if map.is_blocked([p[0], p[2]])? {
                    return Ok(Some(Check::Rejected(
                        "Authored terrain filter intersects reserved placement",
                    )));
                }
            }
            if f.radius == 0. {
                if spawn::factor(layers, &f.filter, u, v) <= 0. {
                    return Ok(Some(Check::Rejected("Authored terrain filter failed")));
                }
            } else {
                let r = f.radius / layers.size as f32;
                let (cx, cz) = (index(n, u), index(n, v));
                let mut mask = 0;
                for z in index(n, v - r)..=index(n, v + r) {
                    if cancelled() {
                        return Ok(None);
                    }
                    for x in index(n, u - r)..=index(n, u + r) {
                        if !visit(work, limit) {
                            return Ok(Some(Check::Budget));
                        }
                        let dx = x as f32 - cx as f32;
                        let dz = z as f32 - cz as f32;
                        if dx * dx + dz * dz <= f.radius * f.radius {
                            mask |= layers.topology[z * n + x] as u32;
                        }
                    }
                }
                let mut material = f.filter.clone();
                material.topology_any = -1;
                material.topology_all = 0;
                material.topology_not = 0;
                if !f.filter.topology_matches(mask) || spawn::factor(layers, &material, u, v) <= 0.
                {
                    return Ok(Some(Check::Rejected(
                        "Authored radius/material filter failed",
                    )));
                }
            }
        }
        if !rules.anchors.is_empty() {
            let mut lo = f32::NEG_INFINITY;
            let mut hi = f32::INFINITY;
            let mut average = 0.;
            for a in &rules.anchors {
                if cancelled() {
                    return Ok(None);
                }
                let p = world(a.position, pos, heading);
                let Some((u, v)) = uv(layers.size, p) else {
                    return Ok(Some(Check::Rejected("Terrain anchor lies outside the map")));
                };
                if !visit(work, limit) {
                    return Ok(Some(Check::Budget));
                }
                if let Some(filter) = self.stage_filter {
                    if !visit(work, limit) {
                        return Ok(Some(Check::Budget));
                    }
                    if let Some(map) = self.placement_map {
                        if !visit(work, limit) {
                            return Ok(Some(Check::Budget));
                        }
                        if map.is_blocked([p[0], p[2]])? {
                            return Ok(Some(Check::Rejected(
                                "Monument stage anchor intersects reserved placement",
                            )));
                        }
                    }
                    if spawn::factor(layers, filter, u, v) == 0. {
                        return Ok(Some(Check::Rejected(
                            "Authored monument stage filter failed at anchor",
                        )));
                    }
                }
                let h = spawn::height(layers, u, v);
                let mut amin = h - a.offset - a.extents;
                let mut amax = h - a.offset + a.extents;
                if a.radius > 0. {
                    let r = a.radius / layers.size as f32;
                    let first = index(hr, u - r);
                    let last = index(hr, u + r);
                    let mut minimum = i16::MAX;
                    let mut maximum = i16::MIN;
                    for z in index(hr, v - r)..=index(hr, v + r) {
                        if cancelled() {
                            return Ok(None);
                        }
                        let row = &layers.height[z * hr + first..=z * hr + last];
                        if row.len() > limit.saturating_sub(*work) {
                            *work = limit;
                            return Ok(Some(Check::Budget));
                        }
                        *work += row.len();
                        for &sample in row {
                            minimum = minimum.min(sample);
                            maximum = maximum.max(sample);
                        }
                    }
                    // Decoding and the authored offset/extent arithmetic are
                    // monotone. Only the extrema can tighten the interval.
                    amin = amin.max(crate::erosion::decode(maximum) - a.offset - a.extents);
                    amax = amax.min(crate::erosion::decode(minimum) - a.offset + a.extents);
                }
                average += h - a.position[1];
                lo = lo.max(amin - a.position[1]);
                hi = hi.min(amax - a.position[1]);
                if hi < lo {
                    return Ok(Some(Check::Rejected(
                        "Authored terrain anchors have no shared height",
                    )));
                }
            }
            if hi > 1. && lo < 1. {
                lo = 1.;
            }
            pos[1] = (average / rules.anchors.len() as f32).clamp(lo, hi);
        }
        let mut matching = 0;
        for sx in [-1., 1.] {
            for sz in [-1., 1.] {
                if cancelled() {
                    return Ok(None);
                }
                let p = world(
                    [
                        bounds.center[0] + sx * bounds.extent[0],
                        0.,
                        bounds.center[1] + sz * bounds.extent[1],
                    ],
                    pos,
                    heading,
                );
                let Some((u, v)) = uv(layers.size, p) else {
                    return Ok(Some(Check::Rejected(
                        "Monument tier corner lies outside the map",
                    )));
                };
                if !visit(work, limit) {
                    return Ok(Some(Check::Budget));
                }
                if layers.topology[index(n, v) * n + index(n, u)] as u32 & rules.tier_mask != 0 {
                    matching += 1;
                }
            }
        }
        Ok(Some(if matching < rules.min_corners {
            Check::Rejected("Authored monument tier corners failed")
        } else {
            Check::Accepted(pos)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_placement_queries_follow_rotated_filter_and_stage_anchor_points() {
        let (layers, mut rules) = fixture();
        let filter = spawn::Filter {
            splat_type: -1,
            biome_type: -1,
            topology_any: -1,
            topology_all: 0,
            topology_not: 0,
        };
        rules.filters.push(Filter {
            position: [100., 0., 0.],
            filter: filter.clone(),
            radius: 0.,
            check_bounds: true,
            check_placement: true,
        });
        let n = layers.surface_resolution;
        let bounds = Footprint {
            center: [0.; 2],
            extent: [20., 20.],
        };
        let mut cells = vec![0; n * n];
        let cell = |p: f32| (((p + 500.) * (1. / 1000.)) * n as f32) as usize;
        cells[cell(100.) * n + cell(0.)] = 1;
        let map = crate::placement_map::PlacementMap::new(1000., n, &cells).unwrap();
        let run = |rules: &Rules,
                   map: Option<crate::placement_map::PlacementMap<'_>>,
                   stage: bool,
                   limit| {
            let prepared = Evaluator::new(&layers, rules, bounds, || false)
                .unwrap()
                .unwrap();
            let prepared = if let Some(map) = map {
                prepared.with_placement_map(map).unwrap()
            } else {
                prepared
            };
            let prepared = if stage {
                prepared.with_stage_filter(&filter).unwrap()
            } else {
                prepared
            };
            prepared
                .evaluate([0., 100., 0.], 90., &mut 0, limit, || false)
                .unwrap()
        };
        assert!(matches!(
            run(&rules, None, false, 1000),
            Some(Check::Accepted(_))
        ));
        assert_eq!(
            run(&rules, Some(map), false, 1000),
            Some(Check::Rejected(
                "Authored terrain filter intersects reserved placement"
            ))
        );
        rules.filters[0].check_placement = false;
        assert!(matches!(
            run(&rules, Some(map), false, 1000),
            Some(Check::Accepted(_))
        ));
        // Placement occupancy is sampled at the authored point even when its
        // topology filter has a radius containing a different reserved cell.
        rules.filters[0].check_placement = true;
        rules.filters[0].position = [0.; 3];
        rules.filters[0].radius = 128.;
        assert!(matches!(
            run(&rules, Some(map), false, 100_000),
            Some(Check::Accepted(_))
        ));
        rules.filters.clear();
        rules.anchors.push(Anchor {
            position: [100., 0., 0.],
            extents: 1.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        assert!(matches!(
            run(&rules, Some(map), false, 1000),
            Some(Check::Accepted(_))
        ));
        assert_eq!(
            run(&rules, Some(map), true, 1000),
            Some(Check::Rejected(
                "Monument stage anchor intersects reserved placement"
            ))
        );
        assert_eq!(run(&rules, Some(map), true, 0), Some(Check::Budget));
        assert!(
            Evaluator::new(&layers, &rules, bounds, || false)
                .unwrap()
                .unwrap()
                .with_placement_map(map)
                .unwrap()
                .evaluate([0., 100., 0.], 90., &mut 0, 1000, || true)
                .unwrap()
                .is_none()
        );
    }
    fn fixture() -> (Layers, Rules) {
        let (h, n) = crate::native_layers::dimensions(1000).unwrap();
        (
            Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 1,
                height_resolution: h,
                surface_resolution: n,
                height: vec![crate::erosion::encode(100.); h * h],
                biome: vec![255; n * n * 5],
                splat: vec![255; n * n * 8],
                topology: vec![1; n * n],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            },
            Rules {
                tier_mask: 1,
                min_corners: 3,
                anchors: vec![],
                filters: vec![],
                unsupported: vec![],
                pending: vec![],
            },
        )
    }
    fn check(l: &Layers, r: &Rules, limit: usize) -> Option<Check> {
        evaluate(
            l,
            r,
            Footprint {
                center: [0., 0.],
                extent: [20., 30.],
            },
            [0., 50., 0.],
            17.,
            &mut 0,
            limit,
            || false,
        )
        .unwrap()
    }
    #[test]
    fn stage_filter_checks_rotated_anchors_and_accounts_for_work_without_mutation() {
        let (mut layers, mut rules) = fixture();
        let n = layers.surface_resolution;
        // Snow on the left, grass on the right. The root stays in snow.
        layers.biome.fill(0);
        for z in 0..n {
            for x in 0..n {
                layers.biome[(if x < n / 2 { 3 } else { 1 }) * n * n + z * n + x] = 255;
            }
        }
        let before = layers.biome.clone();
        rules.anchors.push(Anchor {
            position: [100., 0., 0.],
            extents: 10.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        let bounds = Footprint {
            center: [0.; 2],
            extent: [10.; 2],
        };
        let filter = spawn::Filter {
            splat_type: -1,
            biome_type: 8,
            topology_any: -1,
            topology_all: 0,
            topology_not: 0,
        };
        let plain = Evaluator::new(&layers, &rules, bounds, || false)
            .unwrap()
            .unwrap();
        let checked = Evaluator::new(&layers, &rules, bounds, || false)
            .unwrap()
            .unwrap()
            .with_stage_filter(&filter)
            .unwrap();
        for yaw in [0., 37., 90., 137., 180., 270.] {
            assert!(matches!(
                plain
                    .evaluate([-50., 100., 0.], yaw, &mut 0, 100, || false)
                    .unwrap(),
                Some(Check::Accepted(_))
            ));
            let result = checked
                .evaluate([-50., 100., 0.], yaw, &mut 0, 100, || false)
                .unwrap();
            if yaw < 90. {
                assert_eq!(
                    result,
                    Some(Check::Rejected(
                        "Authored monument stage filter failed at anchor"
                    ))
                );
            } else {
                assert!(matches!(result, Some(Check::Accepted(_))));
            }
        }
        let mut work = 0;
        assert_eq!(
            checked
                .evaluate([-50., 100., 0.], 0., &mut work, 1, || false)
                .unwrap(),
            Some(Check::Budget)
        );
        assert_eq!(work, 1);
        assert_eq!(
            checked
                .evaluate([-50., 100., 0.], 0., &mut 0, 100, || true)
                .unwrap(),
            None
        );
        assert_eq!(layers.biome, before);
        let mut invalid = filter.clone();
        invalid.biome_type = 0;
        assert!(plain.with_stage_filter(&invalid).is_err());
    }
    #[test]
    fn prepared_context_matches_single_queries_and_work_and_rejects_invalid_input() {
        let (layers, mut rules) = fixture();
        rules.anchors.push(Anchor {
            position: [5., 2., -7.],
            extents: 10.,
            offset: 1.,
            radius: 2.,
            slope_scale: 0.5,
        });
        let bounds = Footprint {
            center: [0.; 2],
            extent: [20., 30.],
        };
        let prepared = Evaluator::new(&layers, &rules, bounds, || false)
            .unwrap()
            .unwrap();
        for (position, yaw) in [
            ([0., 50., 0.], 0.),
            ([100., 80., -200.], 37.),
            ([490., 90., 0.], 271.),
        ] {
            for limit in [1, 4, 100, 10000] {
                let (mut a, mut b) = (0, 0);
                assert_eq!(
                    evaluate(
                        &layers,
                        &rules,
                        bounds,
                        position,
                        yaw,
                        &mut a,
                        limit,
                        || false
                    )
                    .unwrap(),
                    prepared
                        .evaluate(position, yaw, &mut b, limit, || false)
                        .unwrap()
                );
                assert_eq!(a, b);
            }
        }
        assert!(
            prepared
                .evaluate([f32::NAN, 0., 0.], 0., &mut 0, 100, || false)
                .is_err()
        );
        assert!(
            prepared
                .evaluate([0.; 3], 0., &mut 0, 100, || true)
                .unwrap()
                .is_none()
        );
        rules.min_corners = 0;
        assert!(Evaluator::new(&layers, &rules, bounds, || false).is_err());
    }
    #[test]
    fn anchors_adjust_height_using_offsets_and_reject_disjoint_intervals() {
        let (l, mut r) = fixture();
        r.anchors.push(Anchor {
            position: [0., 5., 0.],
            extents: 0.,
            offset: 2.,
            radius: 0.,
            slope_scale: 1.,
        });
        let Check::Accepted(p) = check(&l, &r, 100).unwrap() else {
            panic!("expected fitted anchor");
        };
        assert!((p[1] - (crate::erosion::decode(l.height[0]) - 7.)).abs() < 0.001);
        r.anchors.push(Anchor {
            position: [0., 100., 0.],
            extents: 0.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        assert_eq!(
            check(&l, &r, 100),
            Some(Check::Rejected(
                "Authored terrain anchors have no shared height"
            ))
        );
    }
    #[test]
    fn radius_anchors_check_the_entire_source_rectangle() {
        let (mut l, mut r) = fixture();
        r.anchors.push(Anchor {
            position: [0.; 3],
            extents: 1.,
            offset: 0.,
            radius: 5.,
            slope_scale: 0.,
        });
        assert!(matches!(check(&l, &r, 1000), Some(Check::Accepted(_))));
        let bounds = Footprint {
            center: [0.; 2],
            extent: [20.; 2],
        };
        let eval = Evaluator::new(&l, &r, bounds, || false).unwrap().unwrap();
        let radius = r.anchors[0].radius / l.size as f32;
        let side =
            index(l.height_resolution, 0.5 + radius) - index(l.height_resolution, 0.5 - radius) + 1;
        let first_row = 1 + side;
        for limit in [2, first_row, first_row + 1, 1 + side * side] {
            let mut work = 0;
            assert_eq!(
                eval.evaluate([0.; 3], 0., &mut work, limit, || false)
                    .unwrap(),
                Some(Check::Budget)
            );
            assert_eq!(work, limit);
        }
        let mut work = 0;
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() == 4
        };
        assert_eq!(
            eval.evaluate([0.; 3], 0., &mut work, 1000, cancelled)
                .unwrap(),
            None
        );
        assert_eq!(work, first_row);
        let h = l.height_resolution;
        l.height[h / 2 * h + h / 2 + 1] = crate::erosion::encode(150.);
        assert_eq!(
            check(&l, &r, 1000),
            Some(Check::Rejected(
                "Authored terrain anchors have no shared height"
            ))
        );
    }
    #[test]
    fn tiers_and_local_filters_reject_ineligible_cells_without_changing_layers() {
        let (mut l, mut r) = fixture();
        r.filters.push(Filter {
            position: [0.; 3],
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: -1,
                topology_all: 0,
                topology_not: 2,
            },
            radius: 5.,
            check_bounds: true,
            check_placement: true,
        });
        assert!(matches!(check(&l, &r, 1000), Some(Check::Accepted(_))));
        let n = l.surface_resolution;
        l.topology[n / 2 * n + n / 2 + 1] |= 2;
        assert_eq!(
            check(&l, &r, 1000),
            Some(Check::Rejected("Authored radius/material filter failed"))
        );
        r.filters.clear();
        r.tier_mask = 4;
        assert_eq!(
            check(&l, &r, 1000),
            Some(Check::Rejected("Authored monument tier corners failed"))
        );
        assert_eq!(l.topology[n / 2 * n + n / 2 + 1], 3);
    }
    #[test]
    fn checks_cancel_and_exhaust_before_publishing_a_pose() {
        let (l, mut r) = fixture();
        assert_eq!(check(&l, &r, 0), Some(Check::Budget));
        assert!(
            evaluate(
                &l,
                &r,
                Footprint {
                    center: [0.; 2],
                    extent: [20.; 2]
                },
                [0.; 3],
                0.,
                &mut 0,
                100,
                || true
            )
            .unwrap()
            .is_none()
        );
        r.unsupported.push("DecorScale".into());
        assert_eq!(
            check(&l, &r, 100),
            Some(Check::Rejected("Unsupported required monument attributes"))
        );
        r.anchors.push(Anchor {
            position: [0.; 3],
            extents: 1.,
            offset: 0.,
            radius: f32::NAN,
            slope_scale: 0.,
        });
        assert!(r.validate().is_err());
    }

    #[test]
    fn rotated_anchor_and_filter_use_local_offsets_not_the_monument_pivot() {
        let (mut l, mut r) = fixture();
        r.anchors.push(Anchor {
            position: [40., 7., 0.],
            extents: 0.,
            offset: 3.,
            radius: 0.,
            slope_scale: 9.,
        });
        r.filters.push(Filter {
            position: [40., 0., 0.],
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: 2,
                topology_all: 2,
                topology_not: 0,
            },
            radius: 0.,
            check_bounds: true,
            check_placement: false,
        });
        // At 90 degrees, local +X lies on world +Z. The pivot and unrotated
        // location deliberately fail the authored filter.
        let n = l.surface_resolution;
        l.topology[index(n, 0.54) * n + index(n, 0.5)] |= 2;
        let run = |yaw| {
            evaluate(
                &l,
                &r,
                Footprint {
                    center: [0.; 2],
                    extent: [20.; 2],
                },
                [0., 50., 0.],
                yaw,
                &mut 0,
                100,
                || false,
            )
            .unwrap()
        };
        let Some(Check::Accepted(p)) = run(90.) else {
            panic!("rotated attributes should pass")
        };
        assert!((p[1] - (crate::erosion::decode(l.height[0]) - 10.)).abs() < 0.001);
        assert_eq!(
            run(0.),
            Some(Check::Rejected("Authored terrain filter failed"))
        );
        assert_eq!(
            run(270.),
            Some(Check::Rejected("Authored terrain filter failed"))
        );
    }

    #[test]
    fn tier_corners_respect_authored_center_rotation_and_exact_threshold() {
        let (mut l, mut r) = fixture();
        l.topology.fill(0);
        let bounds = Footprint {
            center: [60., 0.],
            extent: [20., 30.],
        };
        let n = l.surface_resolution;
        // A 90-degree rotation puts the offset bounds center at world Z=60.
        for (x, z) in [(30., 40.), (-30., 40.), (30., 80.)] {
            l.topology[index(n, z / 1000. + 0.5) * n + index(n, x / 1000. + 0.5)] = 1;
        }
        let run = |r: &Rules, yaw| {
            evaluate(&l, r, bounds, [0., 50., 0.], yaw, &mut 0, 100, || false).unwrap()
        };
        assert_eq!(run(&r, 90.), Some(Check::Accepted([0., 50., 0.])));
        r.min_corners = 4;
        assert_eq!(
            run(&r, 90.),
            Some(Check::Rejected("Authored monument tier corners failed"))
        );
        r.min_corners = 3;
        assert_eq!(
            run(&r, 0.),
            Some(Check::Rejected("Authored monument tier corners failed"))
        );
    }

    #[test]
    fn radius_filter_combines_any_all_and_forbidden_masks_at_rotated_offset() {
        let (mut l, mut r) = fixture();
        r.filters.push(Filter {
            position: [40., 0., 0.],
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: 2,
                topology_all: 6,
                topology_not: 8,
            },
            radius: 2.,
            check_bounds: true,
            check_placement: false,
        });
        let n = l.surface_resolution;
        let center = index(n, 0.54) * n + index(n, 0.5);
        let run = |l: &Layers| {
            evaluate(
                l,
                &r,
                Footprint {
                    center: [0.; 2],
                    extent: [20.; 2],
                },
                [0., 50., 0.],
                90.,
                &mut 0,
                1000,
                || false,
            )
            .unwrap()
        };
        // Required bits may live on different samples inside the radius.
        l.topology[center] = 3;
        l.topology[center + 1] = 5;
        assert_eq!(run(&l), Some(Check::Accepted([0., 50., 0.])));
        l.topology[center + 1] = 1;
        assert_eq!(
            run(&l),
            Some(Check::Rejected("Authored radius/material filter failed"))
        );
        l.topology[center + 1] = 13;
        assert_eq!(
            run(&l),
            Some(Check::Rejected("Authored radius/material filter failed"))
        );
        // Forbidden topology outside the source radius rectangle is irrelevant.
        l.topology[center + 1] = 5;
        l.topology[center + 4] = 8;
        assert_eq!(run(&l), Some(Check::Accepted([0., 50., 0.])));
    }

    #[test]
    fn anchor_floor_does_not_override_an_authored_interval_below_sea_level() {
        let (mut l, mut r) = fixture();
        l.height.fill(crate::erosion::encode(0.));
        r.anchors.push(Anchor {
            position: [0.; 3],
            extents: 2.,
            offset: 0.,
            radius: 0.,
            slope_scale: 0.,
        });
        // The source applies the 1m floor only when the shared interval crosses it.
        let Some(Check::Accepted(p)) = check(&l, &r, 100) else {
            panic!("expected anchor fit")
        };
        assert_eq!(p[1], 1.);
        r.anchors[0].offset = 10.;
        let Some(Check::Accepted(p)) = check(&l, &r, 100) else {
            panic!("expected submerged fit")
        };
        assert!(p[1] < -7.);
    }

    #[test]
    fn invalid_pose_or_rules_never_return_an_accepted_position() {
        let (l, mut r) = fixture();
        for y in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                evaluate(
                    &l,
                    &r,
                    Footprint {
                        center: [0.; 2],
                        extent: [1.; 2]
                    },
                    [0., y, 0.],
                    0.,
                    &mut 0,
                    100,
                    || false
                )
                .is_err()
            );
        }
        r.min_corners = 0;
        assert!(
            evaluate(
                &l,
                &r,
                Footprint {
                    center: [0.; 2],
                    extent: [1.; 2]
                },
                [0., 50., 0.],
                0.,
                &mut 0,
                100,
                || false
            )
            .is_err()
        );
    }

    #[test]
    fn direct_engine_calls_validate_all_authored_rules_before_fitting() {
        let (l, mut r) = fixture();
        r.anchors.push(Anchor {
            position: [0.; 3],
            extents: 1.,
            offset: f32::NAN,
            radius: 0.,
            slope_scale: 0.,
        });
        let run = |r: &Rules| {
            evaluate(
                &l,
                r,
                Footprint {
                    center: [0.; 2],
                    extent: [20.; 2],
                },
                [0., 50., 0.],
                0.,
                &mut 0,
                1000,
                || false,
            )
        };
        assert!(run(&r).is_err());
        r.anchors[0].offset = 0.;
        r.anchors[0].radius = 257.;
        assert!(run(&r).is_err());
        r.anchors.clear();
        r.filters.push(Filter {
            position: [0.; 3],
            filter: spawn::Filter {
                splat_type: 0,
                biome_type: -1,
                topology_any: -1,
                topology_all: 0,
                topology_not: 0,
            },
            radius: 0.,
            check_bounds: false,
            check_placement: false,
        });
        assert!(run(&r).is_err());
        r.filters[0].filter.splat_type = -1;
        r.filters[0].position[2] = f32::INFINITY;
        assert!(run(&r).is_err());
    }
}
