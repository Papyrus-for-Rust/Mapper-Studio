//! Ordered surface-portal replacement of buried dungeon cells.
use crate::{
    tunnel_grid::Cell,
    tunnel_layout::Placement,
    tunnel_network::{Failure, Meter},
};
use std::collections::{BTreeMap, BTreeSet};

pub struct Site<'a> {
    pub placement: &'a Placement,
    pub module: &'a Cell,
    pub replaceable: bool,
}

pub struct Portal<'a> {
    pub module: &'a Cell,
    /// Captured MonumentInfo world offset in the identity prefab frame.
    pub surface_offset: [f32; 3],
}

#[derive(Clone, Copy)]
pub struct Policy {
    pub minimum_height: f32,
    pub minimum_rail_distance: f32,
    pub minimum_spacing: f32,
    pub maximum_portals: usize,
}

pub struct Request<'a> {
    pub sites: &'a [Site<'a>],
    pub portals: &'a [Portal<'a>],
    /// Sampled rail vertices, matching the inspected source's point-distance query.
    pub rails: &'a [&'a [[f32; 3]]],
    pub vertical_offset: f32,
    pub policy: Policy,
    pub seed: u64,
    pub maximum_work: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub cell: [usize; 2],
    pub original_prefab_id: u32,
    pub prefab_id: u32,
    pub surface_position: [f32; 3],
    pub rail_distance: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub selections: Vec<Selection>,
    pub candidates: usize,
    pub work: usize,
}

fn finite(position: &[f32; 3]) -> bool {
    position
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 1_000_000.)
}

fn separated(a: &Selection, b: &Selection, minimum: f32) -> bool {
    a.cell != b.cell
        && a.surface_position
            .iter()
            .zip(b.surface_position)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt()
            >= minimum
}

/// The owning installation adapter checks environment, terrain checks and terrain
/// filters, in that order, at the supplied buried root pose. It charges their work
/// against the remaining allowance. A failure publishes no replacement plan.
pub fn select(
    request: Request<'_>,
    valid: impl FnMut(u32, [f32; 3], usize) -> Result<(bool, usize), Failure>,
    cancelled: impl Fn() -> bool,
) -> Result<Plan, Failure> {
    select_with_constraints(request, valid, |_, _, _| Ok((true, 0)), cancelled)
}

/// Check downstream constraints after seeded ranking, before accepting a portal.
pub fn select_with_constraints(
    request: Request<'_>,
    mut valid: impl FnMut(u32, [f32; 3], usize) -> Result<(bool, usize), Failure>,
    mut feasible: impl FnMut(u32, [f32; 3], usize) -> Result<(bool, usize), Failure>,
    cancelled: impl Fn() -> bool,
) -> Result<Plan, Failure> {
    let policy = request.policy;
    if request.sites.len() > 65536
        || request.portals.len() > 4096
        || request.rails.len() > 65536
        || !request.vertical_offset.is_finite()
        || !(-10000. ..=0.).contains(&request.vertical_offset)
        || !policy.minimum_height.is_finite()
        || policy.minimum_height.abs() > 10000.
        || ![policy.minimum_rail_distance, policy.minimum_spacing]
            .iter()
            .all(|v| v.is_finite() && (0. ..=100000.).contains(v))
        || policy.maximum_portals > 65536
        || !(1..=128_000_000).contains(&request.maximum_work)
    {
        return Err(Failure::Invalid("Invalid tunnel portal inputs"));
    }
    let mut meter = Meter {
        used: 0,
        limit: request.maximum_work,
        cancelled: &cancelled,
    };
    let mut cells = BTreeMap::new();
    for site in request.sites {
        meter.spend(1)?;
        if site.module.id == 0
            || site.module.id != site.placement.prefab_id
            || site
                .placement
                .cell
                .iter()
                .any(|&coordinate| coordinate >= 256)
            || matches!(
                site.placement.role,
                crate::tunnel_layout::Role::Transition(_)
            )
            || !finite(&site.placement.position)
            || cells
                .insert(site.placement.cell, site.placement.position)
                .is_some()
        {
            return Err(Failure::Invalid(
                "Invalid or duplicate portal replacement site",
            ));
        }
    }
    let mut identities = BTreeSet::new();
    for portal in request.portals {
        meter.spend(1)?;
        if portal.module.id == 0
            || !identities.insert(portal.module.id)
            || !finite(&portal.surface_offset)
        {
            return Err(Failure::Invalid("Invalid tunnel portal definition"));
        }
    }
    let mut points = 0_usize;
    for rail in request.rails {
        points = points.checked_add(rail.len()).ok_or(Failure::WorkLimit)?;
        if rail.is_empty() || points > 8_000_000 {
            return Err(Failure::Invalid(
                "Surface rail sampling is empty or too large",
            ));
        }
        for point in *rail {
            meter.spend(1)?;
            if !finite(point) {
                return Err(Failure::Invalid("Invalid surface rail sample"));
            }
        }
    }
    let mut candidates = Vec::new();
    let mut distances = BTreeMap::new();
    // The source visits X first, then Z, before shuffling equal-ranked candidates.
    let mut sites: Vec<_> = request.sites.iter().collect();
    meter.spend(
        sites
            .len()
            .saturating_mul(sites.len().max(1).ilog2() as usize + 1),
    )?;
    sites.sort_by_key(|site| site.placement.cell);
    if !request.rails.is_empty() && policy.maximum_portals > 0 {
        for site in sites.iter().filter(|site| site.replaceable) {
            let mut root = site.placement.position;
            root[1] += request.vertical_offset;
            if !finite(&root) {
                return Err(Failure::Invalid(
                    "Buried portal root exceeds its coordinate bounds",
                ));
            }
            for portal in request.portals {
                meter.spend(1)?;
                if site.module.connections != portal.module.connections {
                    continue;
                }
                let remaining = meter
                    .limit
                    .checked_sub(meter.used)
                    .filter(|&remaining| remaining > 0)
                    .ok_or(Failure::WorkLimit)?;
                let (accepted, work) = valid(portal.module.id, root, remaining)?;
                meter.spend(work)?;
                if !accepted {
                    continue;
                }
                let surface = std::array::from_fn(|i| root[i] + portal.surface_offset[i]);
                if !finite(&surface) {
                    return Err(Failure::Invalid(
                        "Portal surface exceeds its coordinate bounds",
                    ));
                }
                if surface[1] < policy.minimum_height {
                    continue;
                }
                let key = (surface[0].to_bits(), surface[2].to_bits());
                let distance = if let Some(&distance) = distances.get(&key) {
                    distance
                } else {
                    let mut distance = f32::MAX;
                    for rail in request.rails {
                        for point in *rail {
                            meter.spend(1)?;
                            distance =
                                distance.min((point[0] - surface[0]).hypot(point[2] - surface[2]));
                        }
                    }
                    // Keep rejected-site queries bounded too. Once full, the
                    // remaining queries use the same metered direct calculation.
                    if distances.len() < 65536 {
                        distances.insert(key, distance);
                    }
                    distance
                };
                if distance < policy.minimum_rail_distance {
                    continue;
                }
                if candidates.len() >= 65536 {
                    return Err(Failure::WorkLimit);
                }
                candidates.push(Selection {
                    cell: site.placement.cell,
                    original_prefab_id: site.module.id,
                    prefab_id: portal.module.id,
                    surface_position: surface,
                    rail_distance: distance,
                });
            }
        }
    }
    let count = candidates.len();
    let mut random = request.seed;
    for i in (1..candidates.len()).rev() {
        meter.spend(1)?;
        let j = (crate::decor::random(&mut random) % (i as u64 + 1)) as usize;
        candidates.swap(i, j);
    }
    // Quantization and stable tie ordering match the source's integer-distance rank.
    meter.spend(
        candidates
            .len()
            .saturating_mul(candidates.len().max(1).ilog2() as usize + 1),
    )?;
    candidates.sort_by_key(|candidate| candidate.rail_distance.round_ties_even() as i32);
    let mut check = |candidate: &Selection, meter: &mut Meter<'_>| -> Result<bool, Failure> {
        let mut root = cells[&candidate.cell];
        root[1] += request.vertical_offset;
        let remaining = meter
            .limit
            .checked_sub(meter.used)
            .filter(|&work| work > 0)
            .ok_or(Failure::WorkLimit)?;
        let (accepted, work) = feasible(candidate.prefab_id, root, remaining)?;
        meter.spend(work)?;
        Ok(accepted)
    };
    let mut selections = Vec::new();
    let mut deferred = Vec::new();
    for candidate in candidates {
        meter.spend(selections.len() + 1)?;
        if selections.len() == policy.maximum_portals {
            break;
        }
        if selections
            .iter()
            .any(|selected| !separated(selected, &candidate, policy.minimum_spacing))
        {
            if policy.maximum_portals == 2 {
                deferred.push(candidate);
            }
            continue;
        }
        if check(&candidate, &mut meter)? {
            selections.push(candidate);
        }
    }
    if selections.len() == 1 && deferred.len() >= 2 {
        // All candidates compatible with the first choice already failed.
        // Look for a complete pair among the candidates it ruled out.
        let mut alternatives = Vec::new();
        for candidate in deferred {
            meter.spend(1)?;
            if check(&candidate, &mut meter)? {
                alternatives.push(candidate);
            }
        }
        'pair: for (i, first) in alternatives.iter().enumerate() {
            for second in &alternatives[i + 1..] {
                meter.spend(1)?;
                if separated(first, second, policy.minimum_spacing) {
                    selections = vec![first.clone(), second.clone()];
                    break 'pair;
                }
            }
        }
    }
    meter.spend(1)?;
    Ok(Plan {
        selections,
        candidates: count,
        work: meter.used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel_layout::Role;

    fn module(id: u32) -> Cell {
        Cell {
            id,
            connections: [1, 1, 0, 0],
            variants: [0; 4],
            avoid_neighbours: vec![],
        }
    }
    fn placement(id: u32, x: f32, cell: usize) -> Placement {
        Placement {
            prefab_id: id,
            cell: [cell, 0],
            position: [x, 0., 0.],
            role: Role::Tunnel,
        }
    }
    fn policy() -> Policy {
        Policy {
            minimum_height: 1.,
            minimum_rail_distance: 200.,
            minimum_spacing: 1500.,
            maximum_portals: 2,
        }
    }
    fn request<'a>(
        sites: &'a [Site<'a>],
        portals: &'a [Portal<'a>],
        rails: &'a [&'a [[f32; 3]]],
    ) -> Request<'a> {
        Request {
            sites,
            portals,
            rails,
            vertical_offset: -30.,
            policy: policy(),
            seed: 42,
            maximum_work: 100000,
        }
    }
    #[test]
    fn downstream_rejection_keeps_seeded_order_and_only_reserves_accepted_sites() {
        let original = module(1);
        let replacement = module(2);
        let roots = [
            placement(1, 0., 0),
            placement(1, 1600., 1),
            placement(1, 3200., 2),
        ];
        let sites: Vec<_> = roots
            .iter()
            .map(|root| Site {
                placement: root,
                module: &original,
                replaceable: true,
            })
            .collect();
        let portals = [Portal {
            module: &replacement,
            surface_offset: [0., 31., 0.],
        }];
        let points = [[-2000., 0., 0.], [5000., 0., 0.]];
        let rails = [&points[..]];
        let baseline = select(
            request(&sites, &portals, &rails),
            |_, _, _| Ok((true, 0)),
            || false,
        )
        .unwrap();
        let same = select_with_constraints(
            request(&sites, &portals, &rails),
            |_, _, _| Ok((true, 0)),
            |_, root, _| {
                assert_eq!(root[1], -30.);
                Ok((true, 0))
            },
            || false,
        )
        .unwrap();
        assert_eq!(same.selections, baseline.selections);
        assert_eq!(same.work, baseline.work);
        assert_eq!(baseline.selections[0].cell, [2, 0]);
        let next = select_with_constraints(
            request(&sites, &portals, &rails),
            |_, _, _| Ok((true, 0)),
            |_, root, _| Ok((root[0] != 3200., 3)),
            || false,
        )
        .unwrap();
        assert_eq!(next.candidates, baseline.candidates);
        assert_eq!(
            next.selections.iter().map(|s| s.cell).collect::<Vec<_>>(),
            [[0, 0], [1, 0]]
        );
        assert!(matches!(
            select_with_constraints(
                request(&sites, &portals, &rails),
                |_, _, _| Ok((true, 0)),
                |_, _, remaining| Ok((true, remaining + 1)),
                || false,
            ),
            Err(Failure::WorkLimit)
        ));
        assert!(matches!(
            select_with_constraints(
                request(&sites, &portals, &rails),
                |_, _, _| Ok((true, 0)),
                |_, _, _| Err(Failure::Cancelled),
                || false,
            ),
            Err(Failure::Cancelled)
        ));
    }
    #[test]
    fn later_spacing_compatible_pair_replaces_an_isolated_first_choice() {
        let original = module(1);
        let replacement = module(2);
        let roots = [
            placement(1, 800., 0),
            placement(1, 0., 1),
            placement(1, 1600., 2),
        ];
        let sites = roots.each_ref().map(|placement| Site {
            placement,
            module: &original,
            replaceable: true,
        });
        let portals = [Portal {
            module: &replacement,
            surface_offset: [0., 31., 0.],
        }];
        let points = [[800., 0., 201.]];
        let rails = [&points[..]];
        let plan = select(
            request(&sites, &portals, &rails),
            |_, _, _| Ok((true, 0)),
            || false,
        )
        .unwrap();
        let mut cells = plan
            .selections
            .iter()
            .map(|selection| selection.cell)
            .collect::<Vec<_>>();
        cells.sort_unstable();
        assert_eq!(cells, [[1, 0], [2, 0]]);
    }

    #[test]
    fn rejected_portal_roots_choose_distinct_seeded_alternatives() {
        let original = module(1);
        let replacement = module(2);
        let roots = (0..5)
            .map(|i| placement(1, i as f32 * 1600., i))
            .collect::<Vec<_>>();
        let sites = roots
            .iter()
            .map(|root| Site {
                placement: root,
                module: &original,
                replaceable: true,
            })
            .collect::<Vec<_>>();
        let portals = [Portal {
            module: &replacement,
            surface_offset: [0., 31., 0.],
        }];
        let points = [[-2000., 0., 0.], [10000., 0., 0.]];
        let rails = [&points[..]];
        let baseline = select(
            request(&sites, &portals, &rails),
            |_, _, _| Ok((true, 0)),
            || false,
        )
        .unwrap();
        let rejected = baseline
            .selections
            .iter()
            .map(|site| roots[site.cell[0]].position[0].to_bits())
            .collect::<BTreeSet<_>>();
        let retry = || {
            select_with_constraints(
                request(&sites, &portals, &rails),
                |_, _, _| Ok((true, 0)),
                |_, root, _| Ok((!rejected.contains(&root[0].to_bits()), 1)),
                || false,
            )
            .unwrap()
        };
        let next = retry();
        assert_eq!(next.selections.len(), baseline.selections.len());
        assert_eq!(next.selections.len(), 2);
        assert!(
            next.selections
                .iter()
                .all(|site| !baseline.selections.iter().any(|old| old.cell == site.cell))
        );
        assert_eq!(retry(), next);
        assert_eq!(next.candidates, baseline.candidates);
        assert!(next.work > baseline.work);
    }

    #[test]
    fn sampled_vertex_distance_height_threshold_and_no_rails_follow_source() {
        let original = module(1);
        let replacement = module(2);
        let root = placement(1, 0., 0);
        let sites = [Site {
            placement: &root,
            module: &original,
            replaceable: true,
        }];
        let portals = [Portal {
            module: &replacement,
            surface_offset: [0., 31., 0.],
        }];
        // The connecting line passes through the portal. The inspected query measures vertices.
        let points = [[-1000., 9000., 0.], [1000., -9000., 0.]];
        let rails = [&points[..]];
        let plan = select(
            request(&sites, &portals, &rails),
            |_, root, remaining| {
                assert_eq!(root, [0., -30., 0.]);
                assert!(remaining > 0);
                Ok((true, 2))
            },
            || false,
        )
        .unwrap();
        assert_eq!(plan.selections.len(), 1);
        assert_eq!(plan.selections[0].rail_distance, 1000.);
        assert_eq!(plan.selections[0].surface_position[1], 1.);
        let mut below = request(&sites, &portals, &rails);
        below.vertical_offset = -31.;
        assert!(
            select(below, |_, _, _| Ok((true, 0)), || false)
                .unwrap()
                .selections
                .is_empty()
        );
        let no_rails = select(
            request(&sites, &portals, &[]),
            |_, _, _| panic!("no portal checks without rails"),
            || false,
        )
        .unwrap();
        assert!(no_rails.selections.is_empty());
    }
    #[test]
    fn closest_ranked_candidates_obey_spacing_and_count_with_stable_replay() {
        let original = module(1);
        let replacement = module(2);
        let roots = [
            placement(1, 0., 0),
            placement(1, 1000., 1),
            placement(1, 1600., 2),
        ];
        let sites: Vec<_> = roots
            .iter()
            .map(|root| Site {
                placement: root,
                module: &original,
                replaceable: true,
            })
            .collect();
        let portals = [Portal {
            module: &replacement,
            surface_offset: [0., 40., 0.],
        }];
        let points = [[0., 0., -500.]];
        let rails = [&points[..]];
        let run = || {
            select(
                request(&sites, &portals, &rails),
                |_, _, _| Ok((true, 0)),
                || false,
            )
            .unwrap()
        };
        let plan = run();
        assert_eq!(plan, run());
        assert_eq!(plan.candidates, 3);
        assert_eq!(
            plan.selections.iter().map(|s| s.cell).collect::<Vec<_>>(),
            [[0, 0], [2, 0]]
        );
    }
    #[test]
    fn failed_checks_and_incompatible_or_fixed_cells_cannot_be_selected() {
        let original = module(1);
        let mut replacement = module(2);
        replacement.connections = [0, 0, 1, 1];
        let root = placement(1, 0., 0);
        let mut sites = [Site {
            placement: &root,
            module: &original,
            replaceable: true,
        }];
        let points = [[0., 0., 500.]];
        let rails = [&points[..]];
        let portals = [Portal {
            module: &replacement,
            surface_offset: [0., 40., 0.],
        }];
        assert!(
            select(
                request(&sites, &portals, &rails),
                |_, _, _| panic!("incompatible sides"),
                || false
            )
            .unwrap()
            .selections
            .is_empty()
        );
        let portals = [Portal {
            module: &original,
            surface_offset: [0., 40., 0.],
        }];
        assert!(
            select(
                request(&sites, &portals, &rails),
                |_, _, _| Ok((false, 1)),
                || false
            )
            .unwrap()
            .selections
            .is_empty()
        );
        sites[0].replaceable = false;
        assert!(
            select(
                request(&sites, &portals, &rails),
                |_, _, _| panic!("fixed cell"),
                || false
            )
            .unwrap()
            .selections
            .is_empty()
        );
    }
    #[test]
    fn repeated_marker_offsets_reuse_distance_work_but_run_each_authored_check() {
        let original = module(1);
        let replacements = [module(2), module(3)];
        let root = placement(1, 0., 0);
        let sites = [Site {
            placement: &root,
            module: &original,
            replaceable: true,
        }];
        let portals: Vec<_> = replacements
            .iter()
            .map(|module| Portal {
                module,
                surface_offset: [0., 40., 0.],
            })
            .collect();
        let points = vec![[0., 0., 500.]; 1000];
        let rails = [&points[..]];
        let mut req = request(&sites, &portals, &rails);
        req.maximum_work = 2200;
        let mut checks = 0;
        let plan = select(
            req,
            |_, _, _| {
                checks += 1;
                Ok((true, 1))
            },
            || false,
        )
        .unwrap();
        assert_eq!(checks, 2);
        assert_eq!(plan.candidates, 2);
        assert_eq!(plan.selections.len(), 1);
        assert!(plan.work <= 2200);
    }
    #[test]
    fn integer_distance_ties_shuffle_and_spacing_uses_three_dimensions() {
        let original = module(1);
        let replacements = [module(2), module(3)];
        let root = placement(1, 0., 0);
        let sites = [Site {
            placement: &root,
            module: &original,
            replaceable: true,
        }];
        let portals = [
            Portal {
                module: &replacements[0],
                surface_offset: [200.49, 40., 0.],
            },
            Portal {
                module: &replacements[1],
                surface_offset: [200.4, 40., 0.],
            },
        ];
        let points = [[0.; 3]];
        let rails = [&points[..]];
        let mut winners = BTreeSet::new();
        for seed in 0..32 {
            let mut req = request(&sites, &portals, &rails);
            req.seed = seed;
            winners.insert(
                select(req, |_, _, _| Ok((true, 0)), || false)
                    .unwrap()
                    .selections[0]
                    .prefab_id,
            );
        }
        assert_eq!(winners, BTreeSet::from([2, 3]));
        let roots = [placement(1, 0., 0), placement(1, 1000., 1)];
        let sites: Vec<_> = roots
            .iter()
            .map(|placement| Site {
                placement,
                module: &original,
                replaceable: true,
            })
            .collect();
        let portals = [
            Portal {
                module: &replacements[0],
                surface_offset: [0., 40., 0.],
            },
            Portal {
                module: &replacements[1],
                surface_offset: [0., 2040., 0.],
            },
        ];
        let points = [[0., 0., -500.]];
        let rails = [&points[..]];
        let plan = select(
            request(&sites, &portals, &rails),
            |id, root, _| {
                Ok((
                    (root[0] == 0. && id == 2) || (root[0] == 1000. && id == 3),
                    0,
                ))
            },
            || false,
        )
        .unwrap();
        assert_eq!(plan.selections.len(), 2);
    }
    #[test]
    fn cancellation_budget_failure_and_invalid_sampling_publish_no_plan() {
        let original = module(1);
        let root = placement(1, 0., 0);
        let sites = [Site {
            placement: &root,
            module: &original,
            replaceable: true,
        }];
        let portals = [Portal {
            module: &original,
            surface_offset: [0., 40., 0.],
        }];
        let points = [[0., 0., 500.]];
        let rails = [&points[..]];
        assert_eq!(
            select(
                request(&sites, &portals, &rails),
                |_, _, _| Ok((true, 0)),
                || true
            ),
            Err(Failure::Cancelled)
        );
        let mut req = request(&sites, &portals, &rails);
        req.maximum_work = 2;
        assert_eq!(
            select(req, |_, _, _| panic!("exhausted"), || false),
            Err(Failure::WorkLimit)
        );
        assert_eq!(
            select(
                request(&sites, &portals, &rails),
                |_, _, _| Err(Failure::WorkLimit),
                || false
            ),
            Err(Failure::WorkLimit)
        );
        assert_eq!(
            select(
                request(&sites, &portals, &rails),
                |_, _, remaining| Ok((true, remaining + 1)),
                || false
            ),
            Err(Failure::WorkLimit)
        );
        let bad = [[f32::NAN, 0., 0.]];
        assert!(matches!(
            select(
                request(&sites, &portals, &[&bad]),
                |_, _, _| Ok((true, 0)),
                || false
            ),
            Err(Failure::Invalid(_))
        ));
    }
}
