use super::*;
use crate::{powerline_costmap::Counts, road_costmap::CellLattice};

fn grid(n: usize) -> powerline_costmap::Grid {
    powerline_costmap::Grid {
        engine: powerline_costmap::VERSION,
        lattice: CellLattice::new(n as f32, 1.).unwrap(),
        costs: vec![1; n * n],
        counts: Counts::default(),
        work: 0,
    }
}
fn settings() -> Settings {
    Settings {
        search: routing::Settings {
            maximum_expansions: 100000,
            maximum_work: 1_000_000,
            directional_penalty: 10000,
            directional_radius: 1,
        },
        sample_stride: 8,
        minimum_path_cells: 8,
        maximum_path_cells: 65536,
        maximum_network_samples: 4096,
        maximum_routes: 128,
        maximum_work: 10_000_000,
    }
}
fn connector(
    grid: &powerline_costmap::Grid,
    monument: u64,
    object_id: i64,
    cell: [usize; 2],
) -> Connector {
    let position = grid.lattice.point(cell).unwrap();
    Connector {
        monument,
        object_id,
        position: [position[0], 0., position[1]],
    }
}

#[test]
fn primary_road_sampling_starts_at_the_eighth_node_and_retains_long_links() {
    let grid = grid(17);
    let road: Vec<_> = (0..17).map(|x| [x, 0]).collect();
    let connector = connector(&grid, 4, 100, [7, 16]);
    let report = generate(&grid, &[connector], &[&road], settings(), || false)
        .unwrap()
        .unwrap();
    assert!(report.roots.is_empty());
    assert_eq!(report.links.len(), 1);
    let link = &report.links[0];
    assert!(link.retained);
    assert_eq!(link.monument, 4);
    assert!(link.route.cells.contains(&[7, 16]));
    let road_end = link.route.cells.iter().find(|cell| cell[1] == 0).unwrap();
    assert!([[7, 0], [15, 0]].contains(road_end));
    assert_eq!(report.sampled_network_cells, 2 + link.route.cells.len() / 8);
    assert_eq!(grid.costs, vec![1; 17 * 17]);
}

#[test]
fn first_monument_seeds_a_network_and_all_its_sockets_promote_together() {
    let grid = grid(17);
    let connectors = [
        connector(&grid, 10, 1, [0, 0]),
        connector(&grid, 10, 2, [0, 16]),
        connector(&grid, (1 << 48) + 20, 1, [16, 16]),
    ];
    let report = generate(&grid, &connectors, &[], settings(), || false)
        .unwrap()
        .unwrap();
    assert_eq!(report.roots.len(), 1);
    assert_eq!(report.roots[0].monument, 10);
    assert!(matches!(report.roots[0].reason, RootReason::NoNetwork));
    assert_eq!(report.links.len(), 1);
    assert_eq!(report.links[0].monument, (1 << 48) + 20);
    assert!(report.links[0].route.cells.contains(&[0, 16]));
    let repeat = generate(&grid, &connectors, &[], settings(), || false)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::to_value(repeat).unwrap()
    );
}

#[test]
fn short_links_promote_without_emitting_a_path_and_duplicate_cells_terminate() {
    let grid = grid(9);
    let connectors = [
        connector(&grid, 1, 1, [4, 4]),
        connector(&grid, 2, 1, [4, 4]),
        connector(&grid, 3, 1, [5, 4]),
    ];
    let report = generate(&grid, &connectors, &[], settings(), || false)
        .unwrap()
        .unwrap();
    assert_eq!(report.roots.len(), 1);
    assert_eq!(report.links.len(), 2);
    assert!(report.links.iter().all(|link| !link.retained));
    assert_eq!(report.sampled_network_cells, 0);
    assert!(
        report
            .recoveries
            .iter()
            .all(|recovery| matches!(recovery.state, RecoveryState::Recovered { .. }))
    );
}

#[test]
fn isolated_components_and_unrecoverable_sockets_have_explicit_receipts() {
    let mut grid = grid(9);
    for z in 0..9 {
        grid.costs[z * 9 + 4] = 0;
    }
    let connectors = [
        connector(&grid, 1, 1, [0, 4]),
        connector(&grid, 2, 1, [8, 4]),
    ];
    let report = generate(&grid, &connectors, &[], settings(), || false)
        .unwrap()
        .unwrap();
    assert_eq!(report.roots.len(), 2);
    assert!(matches!(report.roots[1].reason, RootReason::Unreachable));
    assert!(report.links.is_empty());
    grid.costs.fill(0);
    let report = generate(&grid, &connectors, &[], settings(), || false)
        .unwrap()
        .unwrap();
    assert!(
        report
            .recoveries
            .iter()
            .all(|recovery| matches!(recovery.state, RecoveryState::Unreachable))
    );
    assert!(report.roots.is_empty());
    let limited = Settings {
        search: routing::Settings {
            maximum_expansions: 1,
            ..settings().search
        },
        ..settings()
    };
    let report = generate(&grid, &connectors, &[], limited, || false)
        .unwrap()
        .unwrap();
    assert!(
        report
            .recoveries
            .iter()
            .all(|recovery| matches!(recovery.state, RecoveryState::DepthLimit))
    );
}

#[test]
fn malformed_inputs_and_output_caps_fail_without_changing_prices() {
    let grid = grid(17);
    let connector = connector(&grid, 1, 1, [16, 16]);
    assert!(generate(&grid, &[connector, connector], &[], settings(), || false).is_err());
    assert!(
        generate(
            &grid,
            &[Connector {
                position: [f32::NAN, 0., 0.],
                ..connector
            }],
            &[],
            settings(),
            || false
        )
        .is_err()
    );
    assert!(generate(&grid, &[], &[&[[0, 0], [17, 0]]], settings(), || false).is_err());
    let road: Vec<_> = (0..17).map(|x| [x, 0]).collect();
    assert!(
        generate(
            &grid,
            &[connector],
            &[&road],
            Settings {
                maximum_network_samples: 1,
                ..settings()
            },
            || false
        )
        .is_err()
    );
    assert!(
        generate(
            &grid,
            &[connector],
            &[&road],
            Settings {
                maximum_path_cells: 8,
                ..settings()
            },
            || false
        )
        .is_err()
    );
    assert_eq!(grid.costs, vec![1; 17 * 17]);
}

#[test]
fn shared_work_limit_and_late_cancellation_prevent_partial_publication() {
    let grid = grid(17);
    let connectors = [
        connector(&grid, 1, 1, [0, 0]),
        connector(&grid, 2, 1, [16, 16]),
    ];
    let baseline = generate(&grid, &connectors, &[], settings(), || false)
        .unwrap()
        .unwrap();
    assert!(
        generate(
            &grid,
            &connectors,
            &[],
            Settings {
                maximum_work: baseline.work,
                ..settings()
            },
            || false
        )
        .unwrap()
        .is_some()
    );
    assert!(
        generate(
            &grid,
            &connectors,
            &[],
            Settings {
                maximum_work: baseline.work - 1,
                ..settings()
            },
            || false
        )
        .is_err()
    );
    let queries = std::cell::Cell::new(0);
    assert!(
        generate(&grid, &connectors, &[], settings(), || {
            queries.set(queries.get() + 1);
            queries.get() > 100
        })
        .unwrap()
        .is_none()
    );
    assert_eq!(grid.costs, vec![1; 17 * 17]);
}

#[test]
fn large_network_allowances_preserve_the_shared_pathfinder_search_bound() {
    let grid = grid(17);
    let connectors = [connector(&grid, 1, 1, [0, 0]), connector(&grid, 2, 1, [16, 16])];
    let mut settings = settings();
    settings.maximum_work = MAXIMUM_WORK;
    settings.search.maximum_work = MAXIMUM_SEARCH_WORK;
    assert!(generate(&grid, &connectors, &[], settings, || false).unwrap().is_some());
    settings.search.maximum_work += 1;
    assert!(generate(&grid, &connectors, &[], settings, || false).is_err());
}
