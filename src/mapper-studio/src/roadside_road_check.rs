//! Reject individual roadside stamps that make protected road spans infeasible.
use crate::{road_paint_source, road_planning::Plan};
use papyrus_terrain::{
    monument_composition::projection::Projected,
    native_layers::Layers,
    path_terrain,
    road_network::Destination,
    road_terrain::Policy,
    roadside_monuments::Road,
    roadside_selection::{Check, Failure, Selected, Work},
};
use std::collections::BTreeSet;

use papyrus_terrain::road_strip::check as strip_clear;
use std::cell::RefCell;

struct BaselineSample {
    height: f32,
    terrain_fixed: bool,
}
/// Per-selection cache. Its borrowed inputs cannot change while it is alive;
/// samples are populated only for roads touched by an assessed projection.
pub(crate) struct Prepared<'a> {
    plan: &'a Plan,
    layers: &'a Layers,
    paint: &'a road_paint_source::Profile,
    terrain: &'a Policy,
    pins: BTreeSet<(usize, usize)>,
    roads: Vec<Road<'a>>,
    bounds: Vec<Option<[f32; 4]>>,
    samples: Vec<RefCell<Option<Vec<BaselineSample>>>>,
}
impl<'a> Prepared<'a> {
    pub(crate) fn new(
        plan: &'a Plan,
        layers: &'a Layers,
        paint: &'a road_paint_source::Profile,
        terrain: &'a Policy,
        work: &mut Work,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Self>, Failure> {
        if cancelled() {
            return Ok(None);
        }
        let (hr, n) = (layers.height_resolution, layers.surface_resolution);
        if !(2..=4097).contains(&hr)
            || !(2..=4097).contains(&n)
            || layers.size == 0
            || layers.height.len() != hr * hr
            || layers.topology.len() != n * n
        {
            return Err(Failure::Evaluation(
                "Invalid roadside baseline grids".into(),
            ));
        }
        let mut pins = BTreeSet::new();
        for road in &plan.planning_report().roads {
            if cancelled() {
                return Ok(None);
            }
            for dest in [road.from, road.to] {
                work.spend(4)?;
                if let Destination::Network { road, point } = dest {
                    pins.insert((road, point));
                }
            }
        }
        let roads = plan.roads();
        let half = layers.size as f32 * 0.5;
        let mut bounds = Vec::with_capacity(roads.len());
        for road in &roads {
            if cancelled() {
                return Ok(None);
            }
            let mut road_bounds = [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ];
            let mut inside = true;
            for &[x, _, z] in road.path.points() {
                if x.abs() > half || z.abs() > half {
                    // Out-of-world controls use clamped sampling.
                    inside = false;
                    break;
                }
                road_bounds = [
                    road_bounds[0].min(x),
                    road_bounds[1].min(z),
                    road_bounds[2].max(x),
                    road_bounds[3].max(z),
                ];
            }
            bounds.push(inside.then_some(road_bounds));
        }
        Ok(Some(Self {
            plan,
            layers,
            paint,
            terrain,
            pins,
            roads,
            bounds,
            samples: (0..plan.planning_report().roads.len())
                .map(|_| RefCell::new(None))
                .collect(),
        }))
    }
    fn sample_road(
        &self,
        road: usize,
        work: &mut Work,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Vec<BaselineSample>>, Failure> {
        let grid = path_terrain::Grid {
            size: self.layers.size as f32,
            resolution: self.layers.height_resolution,
            origin_y: self.paint.height_origin,
            vertical_span: self.paint.vertical_span,
            height: &self.layers.height,
        };
        let n = self.layers.surface_resolution;
        let points = self.roads[road].path.points();
        let mut samples = Vec::with_capacity(points.len());
        for &p in points {
            if cancelled() {
                return Ok(None);
            }
            work.spend(32)?;
            let [x, z] = [p[0], p[2]].map(|v| {
                (((v + grid.size * 0.5) * (1. / grid.size)) * n as f32).clamp(0., (n - 1) as f32)
                    as usize
            });
            let bits = self.layers.topology[z * n + x];
            samples.push(BaselineSample {
                height: path_terrain::sample_height(grid, p).map_err(Failure::Evaluation)?,
                terrain_fixed: self.terrain.weight(bits) == 0.
                    || bits & self.terrain.water_topology != 0,
            });
        }
        Ok(Some(samples))
    }
    pub(crate) fn assess(
        &self,
        selected: Selected,
        projection: &Projected<'_, '_, '_>,
        maximum_grade: f32,
        check_edge_grades: bool,
        work: &mut Work,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Check, Failure> {
        if !projection.uses_layers(self.layers) {
            return Err(Failure::Evaluation(
                "Projected roadside baseline revision differs".into(),
            ));
        }
        assess_inner::<true>(
            self.plan,
            selected,
            projection,
            self.layers,
            self.paint,
            self.terrain,
            maximum_grade,
            check_edge_grades,
            Some(self),
            work,
            cancelled,
        )
    }
}

fn fixed_spans(
    points: &[[f32; 3]],
    heights: &[f32],
    fixed: &[bool],
    affected: &[bool],
    closed: bool,
    grade: f32,
) -> bool {
    let unique = points.len() - usize::from(closed);
    let mut distance = 0.;
    let mut first = None;
    let mut previous = None;
    let mut touched = false;
    let exceeds_grade = |a: usize, b: usize, run: f64| {
        (f64::from(heights[b]) - f64::from(heights[a])).abs()
            > run * f64::from(grade) * 0.999
    };
    for i in 0..points.len() {
        if i > 0 {
            distance += (f64::from(points[i][0]) - f64::from(points[i - 1][0]))
                .hypot(f64::from(points[i][2]) - f64::from(points[i - 1][2]));
        }
        touched |= affected[i];
        if i < unique && fixed[i] {
            if let Some((a, start)) = previous {
                if touched && exceeds_grade(a, i, distance - start) {
                    return false;
                }
            } else {
                first = Some((i, distance, touched));
            }
            previous = Some((i, distance));
            // A fixed control belongs to both neighboring spans.
            touched = affected[i];
        }
    }
    if closed
        && let (Some((b, end, prefix_touched)), Some((a, start))) = (first, previous)
        && a != b
        && (touched || prefix_touched)
    {
        return !exceeds_grade(a, b, distance - start + end);
    }
    true
}
#[allow(clippy::too_many_arguments)]
fn assess_inner<const BROAD_PHASE: bool>(
    plan: &Plan,
    selected: Selected,
    projection: &Projected<'_, '_, '_>,
    layers: &Layers,
    paint: &road_paint_source::Profile,
    terrain: &Policy,
    maximum_grade: f32,
    check_edge_grades: bool,
    cached: Option<&Prepared<'_>>,
    work: &mut Work,
    cancelled: &impl Fn() -> bool,
) -> Result<Check, Failure> {
    let uncached_roads;
    let original = if let Some(prepared) = cached {
        &prepared.roads
    } else {
        uncached_roads = plan.roads();
        &uncached_roads
    };
    let source_ring_guidance = plan
        .planning_report()
        .source_layout
        .as_ref()
        .is_some_and(|r| {
            (r["engine"] == crate::source_road_handoff::RAW_VERSION
                && r["raw_endpoints_preserved"] == true)
                || (r["engine"] == crate::source_road_handoff::JUNCTION_CHECKED_VERSION
                    && r["raw_endpoints_preserved"] == false)
        });
    let budget = original
        .iter()
        .map(|r| r.path.points().len())
        .sum::<usize>()
        .saturating_mul(8)
        .saturating_add(1024)
        .min(work.remaining())
        .min(1_000_000);
    if budget == 0 {
        return Err(Failure::Budget);
    }
    let adjusted =
        match plan.straighten_selected(std::slice::from_ref(&selected), budget, cancelled) {
            Ok(Some(paths)) => {
                work.spend(paths.work)?;
                paths
            }
            Ok(None) => return Ok(Check::Cancelled),
            Err(error) => {
                work.spend(budget)?;
                if error.contains("budget") || error.contains("allowance") {
                    return Err(Failure::Budget);
                }
                return Ok(Check::Rejected(
                    "Roadside span breaks road geometry or attachments",
                ));
            }
        };
    let mut pins = BTreeSet::new();
    for road in plan
        .planning_report()
        .roads
        .iter()
        .filter(|_| cached.is_none())
    {
        for dest in [road.from, road.to] {
            if let Destination::Network { road, point } = dest {
                pins.insert((road, point));
            }
        }
    }
    let pins = cached.map_or(&pins, |cache| &cache.pins);
    let is_fixed = |bits| terrain.weight(bits) == 0. || bits & terrain.water_topology != 0;
    let n = layers.surface_resolution;
    let grid = path_terrain::Grid {
        size: layers.size as f32,
        resolution: layers.height_resolution,
        origin_y: paint.height_origin,
        vertical_span: paint.vertical_span,
        height: &layers.height,
    };
    let bounds = projection.world_bounds();
    for (r, road) in adjusted.roads().iter().enumerate() {
        let points = road.path.points();
        if BROAD_PHASE && let Some(bounds) = bounds {
            if std::ptr::eq(road.path, original[r].path)
                && let Some(road_bounds) = cached.and_then(|prepared| prepared.bounds[r])
                && (road_bounds[2] < bounds[0]
                    || road_bounds[0] > bounds[2]
                    || road_bounds[3] < bounds[1]
                    || road_bounds[1] > bounds[3])
                && work.remaining() >= points.len()
            {
                if cancelled() {
                    return Ok(Check::Cancelled);
                }
                // Preserve the same logical charge as the skipped point scan.
                work.spend(points.len())?;
                continue;
            }
            // A control outside the lazy projection patch cannot change its
            // sampled height/topology. Keep every geometrically moved road and
            // fall back for out-of-map controls whose sampling is clamped.
            let mut touches = false;
            for (point, before) in points.iter().zip(original[r].path.points()) {
                if cancelled() {
                    return Ok(Check::Cancelled);
                }
                work.spend(1)?;
                if point[0].to_bits() != before[0].to_bits()
                    || point[2].to_bits() != before[2].to_bits()
                    || point[0].abs() > grid.size * 0.5
                    || point[2].abs() > grid.size * 0.5
                    || (point[0] >= bounds[0]
                        && point[0] <= bounds[2]
                        && point[2] >= bounds[1]
                        && point[2] <= bounds[3])
                {
                    touches = true;
                    break;
                }
            }
            if !touches {
                continue;
            }
        }
        let mut heights = Vec::with_capacity(points.len());
        let mut fixed = Vec::with_capacity(points.len());
        let mut affected = Vec::with_capacity(points.len());
        let mut row = cached
            .filter(|_| bounds.is_some())
            .map(|cache| cache.samples[r].borrow_mut());
        if let (Some(cache), Some(row)) = (cached, row.as_mut())
            && row.is_none()
        {
            let Some(samples) = cache.sample_road(r, work, cancelled)? else {
                return Ok(Check::Cancelled);
            };
            **row = Some(samples);
        }
        for (i, &p) in points.iter().enumerate() {
            if cancelled() {
                return Ok(Check::Cancelled);
            }
            let origin = original[r].path.points()[i];
            if let (Some(bounds), Some(row)) = (bounds, row.as_ref())
                && p[0].to_bits() == origin[0].to_bits()
                && p[2].to_bits() == origin[2].to_bits()
                && p[0].abs() <= grid.size * 0.5
                && p[2].abs() <= grid.size * 0.5
                && (p[0] < bounds[0] || p[0] > bounds[2] || p[2] < bounds[1] || p[2] > bounds[3])
            {
                work.spend(4 + 2 * (pins.len() + 1).ilog2() as usize)?;
                let sample = &row.as_ref().unwrap()[i];
                heights.push(sample.height);
                fixed.push(
                    sample.terrain_fixed
                        || pins.contains(&(r, i))
                        || (!road.path.is_circular() && (i == 0 || i + 1 == points.len())),
                );
                affected.push(false);
                continue;
            }
            work.spend(32)?;
            let [x, z] = [p[0], p[2]].map(|v| {
                (((v + grid.size * 0.5) * (1. / grid.size)) * n as f32).clamp(0., (n - 1) as f32)
                    as usize
            });
            let before = layers.topology[z * n + x];
            let after = projection
                .topology(x, z)
                .ok_or_else(|| Failure::Evaluation("Invalid projected road topology".into()))?;
            let h = projection
                .source_height(p, paint.height_origin, paint.vertical_span)
                .map_err(Failure::Evaluation)?;
            let old = path_terrain::sample_height(grid, p).map_err(Failure::Evaluation)?;
            let pinned = pins.contains(&(r, i))
                || (!road.path.is_circular() && (i == 0 || i + 1 == points.len()));
            let f = pinned || is_fixed(after);
            fixed.push(f);
            heights.push(h);
            let origin = original[r].path.points()[i];
            affected.push(
                (f && h.to_bits() != old.to_bits())
                    || is_fixed(before) != is_fixed(after)
                    || p[0].to_bits() != origin[0].to_bits()
                    || p[2].to_bits() != origin[2].to_bits(),
            );
        }
        if affected.iter().any(|v| *v) {
            if !fixed_spans(
                points,
                &heights,
                &fixed,
                &affected,
                road.path.is_circular(),
                maximum_grade,
            ) {
                return Ok(Check::Rejected(
                    "Roadside stamp makes a protected road grade infeasible",
                ));
            }
            let allowance = work.remaining().min(32_000_000);
            if allowance == 0 {
                return Err(Failure::Budget);
            }
            let curve = match road.path.refit_heights(
                &heights,
                0,
                &vec![1.; heights.len()],
                allowance,
                cancelled,
            ) {
                Ok(Some(curve)) => {
                    work.spend(curve.work())?;
                    curve
                }
                Ok(None) => return Ok(Check::Cancelled),
                Err(papyrus_terrain::path_geometry::Failure::Budget) => return Err(Failure::Budget),
                Err(_) => {
                    return Ok(Check::Rejected(
                        "Roadside projection degenerates a road curve",
                    ));
                }
            };
            let family = paint
                .policy
                .families
                .iter()
                .find(|f| f.hierarchy == road.hierarchy)
                .ok_or_else(|| Failure::Evaluation("Missing roadside road-width policy".into()))?;
            if road.hierarchy == 0
                && !source_ring_guidance
                && let Some((launch, _)) = plan.checked_ring_costs()
            {
                let allowance = work.remaining().min(2_000_000);
                if allowance == 0 {
                    return Err(Failure::Budget);
                }
                match plan.check_ring_curve(
                    &curve,
                    layers.size,
                    launch.sample_spacing,
                    allowance,
                    cancelled,
                ) {
                    Ok(Some(audit)) => {
                        work.spend(audit.work)?;
                        if !audit.clear {
                            return Ok(Check::Rejected(
                                "Roadside straightening leaves the source routing corridor",
                            ));
                        }
                    }
                    Ok(None) => return Ok(Check::Cancelled),
                    Err(papyrus_terrain::path_geometry::Failure::Budget) => {
                        return Err(Failure::Budget);
                    }
                    Err(_) => {
                        return Err(Failure::Evaluation(
                            "Invalid projected source routing curve".into(),
                        ));
                    }
                }
            }
            let radius = family.width * 0.5 * family.random_scale.max(1.);
            match strip_clear(&curve, &affected, radius, |n| work.spend(n), cancelled)? {
                None => return Ok(Check::Cancelled),
                Some(false) => return Ok(Check::Rejected("Roadside span folds the road surface")),
                Some(true) => {}
            }
            if check_edge_grades {
                let hr = layers.height_resolution;
                let minimum = family.width * 0.5 * family.random_scale.min(1.);
                let radii = [minimum, (minimum + radius) * 0.5, radius];
                let geometry_changed =
                    points.iter().zip(original[r].path.points()).any(|(a, b)| {
                        a[0].to_bits() != b[0].to_bits() || a[2].to_bits() != b[2].to_bits()
                    });
                let query = |p| -> Result<papyrus_terrain::road_strip::FixedHeight, Failure> {
                    let (height, support) = projection
                        .source_height_with_support(p, paint.height_origin, paint.vertical_span)
                        .map_err(Failure::Evaluation)?;
                    let old = path_terrain::sample_height(grid, p).map_err(Failure::Evaluation)?;
                    let mut fixed = true;
                    let mut changed = geometry_changed || height.to_bits() != old.to_bits();
                    for i in support {
                        let coords = [i % hr, i / hr]
                            .map(|v| v as f32 / (hr - 1) as f32 * grid.size - grid.size * 0.5);
                        let [x, z] = coords.map(|v| {
                            (((v + grid.size * 0.5) * (1. / grid.size)) * n as f32)
                                .clamp(0., (n - 1) as f32) as usize
                        });
                        let before = layers.topology[z * n + x];
                        let after = projection
                            .cell(x, z)
                            .ok_or_else(|| {
                                Failure::Evaluation("Invalid projected edge topology".into())
                            })?
                            .topology;
                        fixed &= is_fixed(after);
                        changed |= is_fixed(before) != is_fixed(after);
                    }
                    Ok(papyrus_terrain::road_strip::FixedHeight {
                        height,
                        fixed,
                        changed,
                    })
                };
                match papyrus_terrain::road_strip::check_fixed_grades(
                    &curve,
                    &affected,
                    &radii,
                    maximum_grade,
                    query,
                    |n| work.spend(n.saturating_mul(129)),
                    cancelled,
                )? {
                    None => return Ok(Check::Cancelled),
                    Some(false) => {
                        return Ok(Check::Rejected(
                            "Roadside stamp makes a protected road edge too steep",
                        ));
                    }
                    Some(true) => {}
                }
            }
        }
    }
    Ok(Check::Accepted {
        height: selected.position[1],
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broad_phase_matches_full_checks_for_rotated_stamps_crossings_and_moved_roads() {
        use papyrus_terrain::{
            monument_composition::{self, Image, Stamp},
            monument_spacing::BoxShape,
            road_network,
            roadside_monuments::Candidate,
            roadside_selection as selection,
        };
        for dense in [false, true] {
            let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
            let layers = Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 2,
                height_resolution: hr,
                surface_resolution: n,
                height: if dense {
                    (0..hr * hr)
                        .map(|i| 17000 + (((i % hr) / 7 + (i / hr) / 9) % 24) as i16)
                        .collect()
                } else {
                    vec![17000; hr * hr]
                },
                topology: vec![1; n * n],
                splat: vec![0; n * n * 8],
                biome: vec![0; n * n * 5],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            };
            let alpha = vec![255; n * n];
            let projection =
                monument_composition::projection::Terrain::new(&layers, &alpha, || false)
                    .unwrap()
                    .unwrap();
            let mut report = road_network::Report::default();
            for (i, x) in [0., 25., 550.]
                .into_iter()
                .chain((0..28).map(|i| 200. + i as f32 * 10.))
                .enumerate()
            {
                report.roads.push(road_network::Road {
                    from: road_network::Destination::Monument {
                        monument: i as u64 + 1,
                        connector: 1,
                    },
                    to: road_network::Destination::Monument {
                        monument: i as u64 + 1,
                        connector: 2,
                    },
                    points: (0..if i == 0 || !dense { 5 } else { 65 })
                        .map(|j| {
                            [
                                x + if i == 0 && j == 2 { 12. } else { 0. },
                                20.,
                                if i == 0 || !dense {
                                    -80. + j as f32 * 40.
                                } else {
                                    -320. + j as f32 * 10.
                                },
                            ]
                        })
                        .collect(),
                });
            }
            report.families.push(road_network::Family {
                infrastructure_type: 0,
                hierarchy: 1,
                seed: 1,
                first_road: 0,
                road_count: report.roads.len(),
                socket_count: 0,
                unresolved: vec![],
                width: 10.,
                splat_channel: 7,
            });
            let plan = Plan::from_report(report, || false).unwrap().unwrap();
            let paint: road_paint_source::Profile = serde_json::from_str(include_str!(
                "../../../compatibility/infrastructure/client-0f703d43-road-paint.json"
            ))
            .unwrap();
            let terrain = crate::road_terrain_source::Profile::for_paint(&paint)
                .unwrap()
                .unwrap();
            let image = Image {
                width: 1,
                height: 1,
                rgba: &[0, 0, 0, 0],
            };
            let mut stamp = Stamp {
                source_cliff_generation: false,
                source_quad_raster: false,
                source_height_blending: false,
                source_texture_sampling: false,
                position: [0., 30., 0.],
                yaw_degrees: 0.,
                size: [100., 100., 30.],
                offset: [7., 0., 3.],
                radius: 25.,
                fade: 0.,
                height: Some(image),
                blend: Some(Image {
                    rgba: &[0, 0, 0, 255],
                    ..image
                }),
                splat: None,
                splat_mask: 0,
                alpha: None,
                biome: None,
                biome_mask: 0,
                topology: Some(Image {
                    rgba: &[0, 4, 0, 0],
                    ..image
                }),
                topology_mask: 1024,
                replace_topology: false,
                auto_cliff_splat: false,
                auto_cliff_topology: false,
            };
            let candidates = [Candidate {
                road: 0,
                start_index: 1,
                end_index: 3,
                sample_distance: 40.,
                direction: 1,
                position: [0., 20., 0.],
                yaw: 0.,
            }];
            let requests = [selection::Request {
                id: 1,
                required: true,
                excluded: false,
                preference: None,
            }];
            let groups = [selection::Group {
                key: 1,
                priority: 0,
                shape: BoxShape {
                    center: [0.; 3],
                    extent: [1.; 3],
                },
                requests: &requests,
                candidates: &candidates,
            }];
            let policy = selection::Policy {
                seed: 1,
                attempts: 1,
                successful_candidates: 1,
                target_count: 1,
                minimum_same: 0.,
                minimum_other: 0.,
                same_mode: selection::DistanceMode::Any,
                other_mode: selection::DistanceMode::Any,
                maximum_work: 32_000_000,
            };
            let prepared = RefCell::new(None);
            let mut cached_work = 0;
            let mut broad_work = 0;
            for position in [
                [0., 30., 0.],
                [20., 100., 0.],
                [300., 30., 300.],
                [499., 100., 0.],
            ] {
                stamp.position = position;
                for yaw in [0., 17., 37., 90., 180., 270.] {
                    stamp.yaw_degrees = yaw;
                    let projected = projection.projected(&stamp).unwrap();
                    let run = |mode: u8| {
                        selection::solve(
                            &groups,
                            &[],
                            policy,
                            |group, request, index, c, w| {
                                let selected = Selected {
                                    group,
                                    request,
                                    candidate: index,
                                    road: c.road,
                                    start_index: c.start_index,
                                    end_index: c.end_index,
                                    position: c.position,
                                    yaw: c.yaw,
                                    score: 0.,
                                };
                                if mode == 2 {
                                    let mut cache = prepared.borrow_mut();
                                    if cache.is_none() {
                                        let Some(value) = Prepared::new(
                                            &plan,
                                            &layers,
                                            &paint,
                                            &terrain.policy,
                                            w,
                                            &|| false,
                                        )?
                                        else {
                                            return Ok(Check::Cancelled);
                                        };
                                        *cache = Some(value);
                                    }
                                    cache.as_ref().unwrap().assess(
                                        selected,
                                        &projected,
                                        0.2,
                                        true,
                                        w,
                                        &|| false,
                                    )
                                } else if mode == 1 {
                                    assess_inner::<true>(
                                        &plan,
                                        selected,
                                        &projected,
                                        &layers,
                                        &paint,
                                        &terrain.policy,
                                        0.2,
                                        true,
                                        None,
                                        w,
                                        &|| false,
                                    )
                                } else {
                                    assess_inner::<false>(
                                        &plan,
                                        selected,
                                        &projected,
                                        &layers,
                                        &paint,
                                        &terrain.policy,
                                        0.2,
                                        true,
                                        None,
                                        w,
                                        &|| false,
                                    )
                                }
                            },
                            || false,
                        )
                        .unwrap()
                        .unwrap()
                    };
                    let fast = run(1);
                    let full = run(0);
                    let cached = run(2);
                    assert_eq!(cached.selected, full.selected, "cached {position:?}/{yaw}");
                    assert_eq!(
                        cached.rejections, full.rejections,
                        "cached {position:?}/{yaw}"
                    );
                    assert_eq!(cached.required_missing, full.required_missing);
                    cached_work += cached.work;
                    broad_work += fast.work;
                    assert_eq!(fast.selected, full.selected, "{position:?}/{yaw}");
                    assert_eq!(fast.rejections, full.rejections, "{position:?}/{yaw}");
                    assert_eq!(fast.required_missing, full.required_missing);
                    if position[0] == 300. && !dense {
                        assert!(
                            !fast.selected.is_empty(),
                            "Moved road outside the stamp still passes the full check"
                        );
                        assert!(fast.work < full.work);
                    }
                }
            }
            assert!(
                cached_work < broad_work,
                "cached {cached_work}, previous broad phase {broad_work}"
            );
            let other = layers.clone();
            let other_terrain =
                monument_composition::projection::Terrain::new(&other, &alpha, || false)
                    .unwrap()
                    .unwrap();
            let other_projection = other_terrain.projected(&stamp).unwrap();
            assert!(!other_projection.uses_layers(&layers));
            assert!(matches!(
                selection::solve(
                    &groups,
                    &[],
                    policy,
                    |g, r, i, c, w| {
                        prepared.borrow().as_ref().unwrap().assess(
                            Selected {
                                group: g,
                                request: r,
                                candidate: i,
                                road: c.road,
                                start_index: c.start_index,
                                end_index: c.end_index,
                                position: c.position,
                                yaw: c.yaw,
                                score: 0.,
                            },
                            &other_projection,
                            0.2,
                            true,
                            w,
                            &|| false,
                        )
                    },
                    || false
                ),
                Err(Failure::Evaluation(_))
            ));
            // Source construction uses its original grid as guidance at preflight and final
            // construction. Strict/unknown receipts still require that corridor.
            stamp.position = [300., 30., 300.];
            let remote = projection.projected(&stamp).unwrap();
            for (engine, preserved, accept) in [
                (crate::source_road_handoff::RAW_VERSION, true, true),
                (crate::source_road_handoff::RAW_VERSION, false, false),
                (
                    crate::source_road_handoff::JUNCTION_CHECKED_VERSION,
                    false,
                    true,
                ),
                (
                    crate::source_road_handoff::JUNCTION_CHECKED_VERSION,
                    true,
                    false,
                ),
                (crate::source_road_handoff::VERSION, true, false),
                ("future", true, false),
            ] {
                let mut report = road_network::Report::default();
                report.roads.push(road_network::Road {
                    from: road_network::Destination::Loop,
                    to: road_network::Destination::Loop,
                    points: plan.roads()[0].path.points().to_vec(),
                });
                report.families.push(road_network::Family {
                    infrastructure_type: 0,
                    hierarchy: 0,
                    seed: 1,
                    first_road: 0,
                    road_count: 1,
                    socket_count: 0,
                    unresolved: vec![],
                    width: 12.,
                    splat_channel: 7,
                });
                report.entrance_launch = Some(road_network::LaunchPolicy {
                    engine: road_network::LaunchEngine::GlobalEarthwork,
                    minimum_run_widths: 1.,
                    sample_spacing: 1.,
                    maximum_work: 2_000_000,
                });
                report.ring_costmap =
                    Some(serde_json::json!({"grid":{"resolution":257,"costs":vec![0;257*257]}}));
                report.source_layout =
                    Some(serde_json::json!({"engine":engine,"raw_endpoints_preserved":preserved}));
                let ring = Plan::from_report(report, || false).unwrap().unwrap();
                let result = selection::solve(
                    &groups,
                    &[],
                    policy,
                    |g, r, i, c, w| {
                        let Some(prepared) =
                            Prepared::new(&ring, &layers, &paint, &terrain.policy, w, &|| false)?
                        else {
                            return Ok(Check::Cancelled);
                        };
                        prepared.assess(
                            Selected {
                                group: g,
                                request: r,
                                candidate: i,
                                road: c.road,
                                start_index: c.start_index,
                                end_index: c.end_index,
                                position: c.position,
                                yaw: c.yaw,
                                score: 0.,
                            },
                            &remote,
                            0.2,
                            false,
                            w,
                            &|| false,
                        )
                    },
                    || false,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    !result.selected.is_empty(),
                    accept,
                    "{engine}/{preserved}/{dense}: {:?}",
                    result.rejections
                );
                if !accept {
                    assert!(
                        result
                            .rejections
                            .keys()
                            .any(|r| r.contains("source routing corridor"))
                    );
                }
            }
        }
    }
    #[test]
    fn rejects_new_infeasible_fixed_spans_but_leaves_adjustable_and_unrelated_slopes_to_later_checks()
     {
        let points = [[0., 0., 0.], [10., 0., 0.], [20., 0., 0.]];
        assert!(!fixed_spans(
            &points,
            &[0., 5., 0.],
            &[true; 3],
            &[false, true, false],
            false,
            0.2
        ));
        assert!(fixed_spans(
            &points,
            &[0., 5., 0.],
            &[true; 3],
            &[false; 3],
            false,
            0.2
        ));
        assert!(fixed_spans(
            &points,
            &[0., 5., 0.],
            &[true, false, true],
            &[false, true, false],
            false,
            0.2
        ));
        assert!(fixed_spans(
            &points,
            &[0., 1., 2.],
            &[true; 3],
            &[true; 3],
            false,
            0.2
        ));
    }
    #[test]
    fn checks_protected_spans_across_the_closed_path_seam() {
        let p = [
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 0., 10.],
            [0., 0., 10.],
            [0., 0., 0.],
        ];
        let heights = [0., 0., 0., 2.2, 0.];
        let pins = [true, false, false, true, false];
        let changed = [false, false, false, true, false];
        assert!(fixed_spans(&p, &heights, &pins, &changed, false, 0.2));
        assert!(!fixed_spans(&p, &heights, &pins, &changed, true, 0.2));

        // Both sides of the wrap can change even when the fixed controls do not.
        let heights = [0., 0., 0., 2.5, 0.];
        let pins = [false, true, false, true, false];
        for changed in [[true, false, false, false, false], [false, false, false, false, true]] {
            assert!(fixed_spans(&p, &heights, &pins, &changed, false, 0.2));
            assert!(!fixed_spans(&p, &heights, &pins, &changed, true, 0.2));
        }
        assert!(fixed_spans(&p, &heights, &[true, false, false, false, true], &[true; 5], true, 0.2));
    }
    #[test]
    fn road_strip_checks_reject_tight_folded_transitions_and_support_cancellation() {
        let straight = papyrus_terrain::path_geometry::Path::from_controls(
            &[[0., 0., 0.], [0., 0., 20.], [0., 0., 40.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let tight = papyrus_terrain::path_geometry::Path::from_controls(
            &[[0., 0., 0.], [1., 0., 0.], [1., 0., 10.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            strip_clear(&straight, &[true; 3], 5., |_| Ok::<_, Failure>(()), &|| {
                false
            })
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            strip_clear(&tight, &[true; 3], 5., |_| Ok::<_, Failure>(()), &|| false).unwrap(),
            Some(false)
        );
        assert_eq!(
            strip_clear(&straight, &[true; 3], 5., |_| Ok::<_, Failure>(()), &|| {
                true
            })
            .unwrap(),
            None
        );
    }
}
