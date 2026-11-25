use super::*;

pub fn settings() -> Settings {
    Settings {
        minimum_chain_nodes: 3,
        maximum_nodes: 100,
        maximum_groups: 10,
        maximum_chains: 100,
        maximum_spans: 1000,
        maximum_work: 10000,
    }
}
fn connection(x: f32) -> Connection {
    Connection {
        incoming: [x, 2., -0.5],
        outgoing: [x, 2., 0.5],
        radius: 0.04,
        hidden: false,
    }
}
fn node<'a>(
    instance: u64,
    root: &'a str,
    wire: &'a str,
    position: [f32; 3],
    connections: Option<&'a [Connection]>,
) -> Node<'a> {
    Node {
        instance,
        component: 1,
        root,
        wire,
        position,
        maximum_distance: 100.,
        connections,
    }
}
fn run(nodes: &[Node<'_>]) -> Report {
    assemble(nodes, settings(), || false).unwrap().unwrap()
}
#[test]
fn root_groups_and_registration_order_survive_interleaving_and_missing_helpers() {
    let connections: Vec<_> = (0..7)
        .map(|index| vec![connection(index as f32 * 10.)])
        .collect();
    let roots = ["z", "a", "z", "a", "z", "a", "z"];
    let nodes: Vec<_> = roots
        .iter()
        .enumerate()
        .map(|(index, root)| {
            node(
                index as u64,
                root,
                "wire",
                [index as f32 * 10., 0., 0.],
                (index != 2).then_some(connections[index].as_slice()),
            )
        })
        .collect();
    let report = run(&nodes);
    assert_eq!(
        report
            .groups
            .iter()
            .map(|group| group.root.as_str())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(report.groups[0].nodes, [0, 2, 4, 6]);
    assert_eq!(report.chains[0].nodes, [0, 4, 6]);
    assert_eq!(report.chains[1].nodes, [1, 3, 5]);
    assert_eq!(report.counts.missing_helpers, 1);
    assert_eq!(report.spans.len(), 4);
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::to_value(run(&nodes)).unwrap()
    );
}
#[test]
fn incoming_node_distance_uses_three_dimensions_squared_and_a_strict_boundary() {
    let connections = [
        vec![connection(0.)],
        vec![connection(6.)],
        vec![connection(12.)],
    ];
    let mut nodes: Vec<_> = (0..3)
        .map(|index| {
            node(
                index as u64,
                "root",
                "wire",
                [index as f32 * 6., index as f32 * 8., 0.],
                Some(&connections[index]),
            )
        })
        .collect();
    nodes[0].maximum_distance = 0.;
    nodes[1].maximum_distance = -10.;
    nodes[2].maximum_distance = 10.;
    assert_eq!(run(&nodes).spans.len(), 2);
    nodes[2].maximum_distance = 9.99;
    let report = run(&nodes);
    assert_eq!(report.counts.distance_splits, 1);
    assert_eq!(report.counts.short_chains, 2);
    assert!(report.spans.is_empty());
}
#[test]
fn split_retains_the_group_signature_and_new_chains_copy_their_first_helper() {
    let mut connections: Vec<_> = (0..4)
        .map(|index| vec![connection(index as f32 * 10.)])
        .collect();
    connections[1][0].radius = 0.07;
    let nodes: Vec<_> = ["a", "b", "a", "a"]
        .iter()
        .enumerate()
        .map(|(index, wire)| {
            node(
                index as u64,
                "root",
                wire,
                [index as f32 * 10., 0., 0.],
                Some(&connections[index]),
            )
        })
        .collect();
    let report = run(&nodes);
    assert_eq!(report.counts.wire_splits, 1);
    assert_eq!(report.chains[1].nodes, [1, 2, 3]);
    assert_eq!(report.chains[1].wire, "a");
    assert_eq!(report.chains[1].signature_node, 0);
    assert!(report.spans.iter().all(|span| span.radius == 0.07));
    let nodes: Vec<_> = nodes
        .iter()
        .enumerate()
        .map(|(index, entry)| Node {
            wire: if index == 0 { "a" } else { "b" },
            ..*entry
        })
        .collect();
    let report = run(&nodes);
    assert_eq!(report.counts.wire_splits, 3);
    assert!(report.spans.is_empty());
}
#[test]
fn connection_count_splits_preserve_source_helper_coverage_and_reject_unsafe_chains() {
    let connections = [
        vec![connection(0.), connection(0.)],
        vec![connection(10.)],
        vec![connection(20.), connection(20.)],
        vec![connection(30.), connection(30.)],
    ];
    let nodes: Vec<_> = connections
        .iter()
        .enumerate()
        .map(|(index, connections)| {
            node(
                index as u64,
                "root",
                "wire",
                [index as f32 * 10., 0., 0.],
                Some(connections),
            )
        })
        .collect();
    let report = run(&nodes);
    assert_eq!(report.counts.connection_splits, 1);
    assert_eq!(report.spans.len(), 2);
    let inverted = [
        vec![connection(0.)],
        vec![connection(10.), connection(10.)],
        vec![connection(20.)],
        vec![connection(30.)],
    ];
    let nodes: Vec<_> = inverted
        .iter()
        .enumerate()
        .map(|(index, connections)| {
            node(
                index as u64,
                "root",
                "wire",
                [index as f32 * 10., 0., 0.],
                Some(connections),
            )
        })
        .collect();
    assert!(assemble(&nodes, settings(), || false).is_err());
}
#[test]
fn hidden_endpoints_and_degenerate_spans_do_not_create_visible_geometry() {
    let mut connections: Vec<_> = (0..3)
        .map(|index| {
            vec![
                connection(index as f32 * 10.),
                connection(index as f32 * 10.),
                Connection {
                    incoming: [0.; 3],
                    outgoing: [0.; 3],
                    radius: 1.,
                    hidden: false,
                },
            ]
        })
        .collect();
    connections[1][1].hidden = true;
    let nodes: Vec<_> = connections
        .iter()
        .enumerate()
        .map(|(index, connections)| {
            node(
                index as u64,
                "root",
                "wire",
                [index as f32 * 10., 0., 0.],
                Some(connections),
            )
        })
        .collect();
    let report = run(&nodes);
    assert_eq!(report.spans.len(), 2);
    assert_eq!(report.counts.hidden_connections, 2);
    assert_eq!(report.counts.degenerate_connections, 2);
    assert!(report.spans.iter().all(|span| span.connection == 0));
}
#[test]
fn span_frames_keep_full_pitch_yaw_unit_cross_section_and_endpoint_midpoints() {
    for end in [
        [10., 4., -20.],
        [0., 25., 0.],
        [0., -25., 0.],
        [0., 0., -20.],
        [20., 0., 0.],
    ] {
        let middle = end.map(|value| value * 0.5);
        let connections = [
            [Connection {
                incoming: [0.; 3],
                outgoing: [0.; 3],
                radius: 0.04,
                hidden: false,
            }],
            [Connection {
                incoming: middle,
                outgoing: middle,
                radius: 1.,
                hidden: false,
            }],
            [Connection {
                incoming: end,
                outgoing: end,
                radius: 2.,
                hidden: false,
            }],
        ];
        let nodes: Vec<_> = [[0.; 3], middle, end]
            .iter()
            .enumerate()
            .map(|(index, &position)| {
                node(
                    index as u64,
                    "root",
                    "wire",
                    position,
                    Some(&connections[index]),
                )
            })
            .collect();
        let report = run(&nodes);
        assert_eq!(report.spans.len(), 2);
        for span in report.spans {
            assert_eq!(&span.scale[..2], &[1., 1.]);
            assert_eq!(span.radius, 0.04);
            for (sign, expected) in [(-1., span.start), (1., span.end)] {
                let offset =
                    crate::decor_fit::rotate(span.rotation, [0., 0., sign * span.scale[2] * 0.5]);
                for axis in 0..3 {
                    assert!((span.position[axis] + offset[axis] - expected[axis]).abs() < 0.0001);
                }
            }
        }
    }
}
#[test]
fn exact_work_output_limits_and_one_shot_cancellation_prevent_partial_reports() {
    let connections = [
        vec![connection(0.)],
        vec![connection(10.)],
        vec![connection(20.)],
    ];
    let nodes: Vec<_> = connections
        .iter()
        .enumerate()
        .map(|(index, connections)| {
            node(
                index as u64,
                "root",
                "wire",
                [index as f32 * 10., 0., 0.],
                Some(connections),
            )
        })
        .collect();
    let baseline = run(&nodes);
    assert!(
        assemble(
            &nodes,
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
        assemble(
            &nodes,
            Settings {
                maximum_work: baseline.work - 1,
                ..settings()
            },
            || false
        )
        .is_err()
    );
    for limited in [
        Settings {
            maximum_nodes: 2,
            ..settings()
        },
        Settings {
            maximum_spans: 1,
            ..settings()
        },
    ] {
        assert!(assemble(&nodes, limited, || false).is_err());
    }
    for threshold in [1, 3, baseline.work + 1] {
        let polls = std::cell::Cell::new(0usize);
        let result = assemble(&nodes, settings(), || {
            polls.set(polls.get() + 1);
            polls.get() == threshold
        });
        if polls.get() >= threshold {
            assert!(result.unwrap().is_none());
        }
    }
    assert_eq!(connections[0][0].outgoing, [0., 2., 0.5]);
}
#[test]
fn invalid_and_duplicate_inputs_fail_before_a_report_can_be_returned() {
    let connections = [connection(0.)];
    let a = node(0, "root", "wire", [0.; 3], Some(&connections));
    assert!(assemble(&[Node { component: 0, ..a }], settings(), || false).is_err());
    let a = node(0, "root", "wire", [0.; 3], Some(&connections));
    assert!(
        assemble(
            &[Node {
                position: [f32::NAN, 0., 0.],
                ..a
            }],
            settings(),
            || false
        )
        .is_err()
    );
    let a = node(0, "root", "wire", [0.; 3], Some(&connections));
    let b = node(0, "root", "wire", [1.; 3], Some(&connections));
    assert!(assemble(&[a, b], settings(), || false).is_err());
}
