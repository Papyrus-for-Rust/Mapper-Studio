use super::*;

fn policy() -> Policy {
    Policy {
        spacing: 125.,
        radius: 0.,
        blocked_topology: 1,
        expensive_topology: 2,
        moderate_topology: 4,
        expensive_cost: 2500,
        moderate_cost: 1000,
        normal_base: 1,
        slope_scale: 10.,
    }
}

fn terrain<'a>(height: &'a [i16], topology: &'a [i32], placement: &'a [u8]) -> Terrain<'a> {
    Terrain {
        size: 1000.,
        height_resolution: 5,
        height,
        topology_resolution: 4,
        topology,
        placement_resolution: 4,
        placement,
    }
}

#[test]
fn branch_priority_costs_and_lattice_are_source_configured() {
    let heights = [16000; 25];
    let topology = [7; 16];
    let placement = [1; 16];
    let run = |topology: &[i32], placement: &[u8]| {
        build(
            terrain(&heights, topology, placement),
            &policy(),
            100000,
            || false,
        )
        .unwrap()
        .unwrap()
    };
    let blocked = run(&topology, &placement);
    assert_eq!(blocked.costs, [0; 64]);
    assert_eq!(blocked.counts.blocked_topology, 64);
    assert_eq!(blocked.lattice.point([0, 0]), Some([-437.5, -437.5]));
    let expensive = run(&[6; 16], &placement);
    assert_eq!(expensive.costs, [2500; 64]);
    assert_eq!(expensive.counts.expensive_topology, 64);
    let occupied = run(&[4; 16], &placement);
    assert_eq!(occupied.costs, [2500; 64]);
    assert_eq!(occupied.counts.placement, 64);
    let moderate = run(&[4; 16], &[0; 16]);
    assert_eq!(moderate.costs, [1000; 64]);
    assert_eq!(moderate.counts.moderate_topology, 64);
    let normal = run(&[0; 16], &[0; 16]);
    assert_eq!(normal.costs, [1; 64]);
    assert_eq!(normal.counts.normal, 64);
    assert!(normal.work > moderate.work);
    assert_eq!(heights, [16000; 25]);
    assert_eq!(topology, [7; 16]);
    assert_eq!(placement, [1; 16]);
}

#[test]
fn normal_slope_cost_uses_truncation_and_retains_steep_walkable_cells() {
    let p = policy();
    assert_eq!(normal_cost(&p, 0.), 1);
    assert_eq!(normal_cost(&p, 0.32), 2);
    assert_eq!(normal_cost(&p, 30.), 9001);
    assert_eq!(normal_cost(&p, 90.), 81001);
    let height: Vec<_> = (0..25).map(|i| 10000 + (i % 5) * 4000).collect();
    let grid = build(terrain(&height, &[0; 16], &[0; 16]), &p, 100000, || false)
        .unwrap()
        .unwrap();
    assert!(grid.costs.iter().all(|&v| v > 1));
    assert_eq!(grid.counts.normal, 64);
}

#[test]
fn budget_exhaustion_and_cancellation_discard_private_output() {
    let p = policy();
    let height = [16000; 25];
    let topology = [0; 16];
    let placement = [0; 16];
    let baseline = build(terrain(&height, &topology, &placement), &p, 100000, || {
        false
    })
    .unwrap()
    .unwrap();
    let exact = build(
        terrain(&height, &topology, &placement),
        &p,
        baseline.work,
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(baseline.costs, exact.costs);
    assert!(
        build(
            terrain(&height, &topology, &placement),
            &p,
            baseline.work - 1,
            || false
        )
        .is_err()
    );
    let calls = std::cell::Cell::new(0);
    assert!(
        build(terrain(&height, &topology, &placement), &p, 100000, || {
            calls.set(calls.get() + 1);
            calls.get() > 15
        })
        .unwrap()
        .is_none()
    );
    assert_eq!(height, [16000; 25]);
    assert_eq!(topology, [0; 16]);
    assert_eq!(placement, [0; 16]);
}

#[test]
fn malformed_policy_rasters_and_hidden_invalid_heights_are_rejected() {
    let p = policy();
    for height in [[16000; 25], [-1; 25]] {
        let result = build(terrain(&height, &[1; 16], &[2; 16]), &p, 100000, || false);
        assert!(result.is_err());
    }
    assert!(
        build(
            terrain(&[16000; 24], &[0; 16], &[0; 16]),
            &p,
            100000,
            || false
        )
        .is_err()
    );
    let mut p = p;
    p.slope_scale = f32::NAN;
    assert!(p.validate().is_err());
    p.slope_scale = 10.;
    p.normal_base = 0;
    assert!(p.validate().is_err());
}

#[test]
fn prefix_queries_match_independent_direct_radius_scans_on_mixed_rasters() {
    let p = Policy {
        spacing: 7.5,
        radius: 5.,
        blocked_topology: 1 << 31,
        expensive_topology: 2,
        moderate_topology: 4,
        ..policy()
    };
    let height: Vec<i16> = (0..17 * 17).map(|i| 16000 + (i % 17) * 40).collect();
    let topology: Vec<i32> = (0..32 * 32)
        .map(|i| match i % 17 {
            0 => (1u32 << 31) as i32,
            1 | 2 => 2,
            3 | 4 => 4,
            _ => 0,
        })
        .collect();
    let placement: Vec<u8> = (0..16 * 16).map(|i| u8::from(i % 31 == 0)).collect();
    let grid = build(
        Terrain {
            size: 1000.,
            height_resolution: 17,
            height: &height,
            topology_resolution: 32,
            topology: &topology,
            placement_resolution: 16,
            placement: &placement,
        },
        &p,
        1_000_000,
        || false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(grid.lattice.resolution(), 133);
    let scan = |n: usize, uv: [f32; 2], circular: bool, predicate: &dyn Fn(usize) -> bool| {
        let radius = p.radius * (1. / 1000.);
        let index = |v: f32| ((v * n as f32) as isize).clamp(0, n as isize - 1) as usize;
        let center = uv.map(index);
        let lo = uv.map(|v| index(v - radius));
        let hi = uv.map(|v| index(v + radius));
        (lo[1]..=hi[1]).any(|z| {
            (lo[0]..=hi[0]).any(|x| {
                let inside = !circular
                    || (x.abs_diff(center[0]).pow(2) + z.abs_diff(center[1]).pow(2)) as f32
                        <= p.radius * p.radius;
                inside && predicate(z * n + x)
            })
        })
    };
    for z in 0..133 {
        for x in 0..133 {
            let uv = [(x as f32 + 0.5) / 133., (z as f32 + 0.5) / 133.];
            let has = |mask| scan(32, uv, true, &|i| topology[i] as u32 & mask != 0);
            let expected = if has(p.blocked_topology) {
                0
            } else if has(p.expensive_topology) || scan(16, uv, false, &|i| placement[i] != 0) {
                p.expensive_cost
            } else if has(p.moderate_topology) {
                p.moderate_cost
            } else {
                let slope = crate::terrain_normal::slope(1000, 17, &height, uv).unwrap();
                1 + (slope * slope * 10.) as u32
            };
            assert_eq!(grid.costs[z * 133 + x], expected, "cell {x},{z}");
        }
    }
    let counts = &grid.counts;
    assert_eq!(
        counts.blocked_topology
            + counts.expensive_topology
            + counts.placement
            + counts.moderate_topology
            + counts.normal,
        133 * 133
    );
}
