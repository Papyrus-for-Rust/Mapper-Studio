use std::io::Write;
use std::process::Command;
use std::process::Stdio;

fn disabled_monuments() -> Vec<u8> {
    b"MAPM"
        .iter()
        .copied()
        .chain(
            [
                2_u32,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                0,
                u32::from_le_bytes(*b"MAPS"),
                1,
                0,
                0,
            ]
            .into_iter()
            .flat_map(u32::to_le_bytes),
        )
        .collect()
}

fn world_output(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn local_registry() -> Vec<u8> {
    let mut input = b"MAPR".to_vec();
    for word in [1_u32, 0, 0, 0, 1, 0, 0] {
        input.extend(word.to_le_bytes());
    }
    let root = std::env::temp_dir().to_string_lossy().into_owned();
    input.extend((root.len() as u32).to_le_bytes());
    input.extend(root.as_bytes());
    input.extend(0_u32.to_le_bytes());
    input
}

#[test]
fn invalid_export_headers_and_arguments_produce_no_map() {
    for input in [
        vec![],
        b"MAPX".to_vec(),
        [b"MAPX".as_slice(), &2_u32.to_le_bytes()].concat(),
    ] {
        let result = world_output(&["encode"], &input);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.is_empty());
    }
    let result = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
        .args(["encode", "extra"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

#[test]
fn direct_world_response_appends_a_map_with_the_requested_timestamp() {
    let output = world_output(
        &[
            "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0", "1",
            "4", "12", "123456",
        ],
        &local_registry(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(&output.stdout[4..8], &6_u32.to_le_bytes());
    let end = 32 + 80 + 1025 * 1025 * 4 + 512 * 512 * 18 + 8;
    assert_eq!(&output.stdout[end..end + 4], b"MAPC");
    assert_eq!(&output.stdout[end + 4..end + 8], &1_u32.to_le_bytes());
    assert_eq!(&output.stdout[end + 8..end + 12], &10_u32.to_le_bytes());
    assert_eq!(
        &output.stdout[end + 12..end + 20],
        &123456_u64.to_le_bytes()
    );
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostics.contains("Map export"));
    assert!(diagnostics.contains("MAPPER_EXPORT_TIME"));
}

#[test]
fn direct_export_rejects_invalid_timestamp_and_legacy_registry_before_generation() {
    for (stamp, input) in [
        ("bad", local_registry()),
        ("18446744073709551615", local_registry()),
        ("123456", disabled_monuments()),
    ] {
        let output = world_output(
            &[
                "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0",
                "1", "4", "12", stamp,
            ],
            &input,
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("MAPPER_PROGRESS"));
    }
}

#[test]
fn local_registry_uses_the_new_response_and_preserves_layer_bytes() {
    let args = [
        "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0", "1", "4",
        "12",
    ];
    let actual = world_output(&args, &local_registry());
    let reference = world_output(&args, &disabled_monuments());
    assert!(actual.status.success());
    assert_eq!(&actual.stdout[..4], b"MAPL");
    assert_eq!(&actual.stdout[4..8], &5_u32.to_le_bytes());
    assert_eq!(&actual.stdout[8..32], &reference.stdout[8..32]);
    assert_eq!(&actual.stdout[112..], &reference.stdout[112..]);
}

#[test]
fn malformed_local_registries_fail_without_progress_or_partial_output() {
    let valid = local_registry();
    let mut version = valid.clone();
    version[4..8].copy_from_slice(&2_u32.to_le_bytes());
    let mut budget = valid.clone();
    budget[24..28].copy_from_slice(&1_u32.to_le_bytes());
    let mut target = valid.clone();
    target[16..20].copy_from_slice(&101_u32.to_le_bytes());
    let mut trailing = valid.clone();
    trailing.push(0);
    for input in [
        vec![],
        valid[..valid.len() - 1].to_vec(),
        version,
        budget,
        target,
        trailing,
    ] {
        let output = world_output(
            &[
                "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0",
                "1", "4", "12",
            ],
            &input,
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("MAPPER_PROGRESS"));
    }
}

fn erode(input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
        .args(["erode", "33", "1000", "2", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn erosion_round_trips_a_flat_encoded_grid() {
    let input: Vec<u8> = (0..33 * 33).flat_map(|_| 20000_i16.to_le_bytes()).collect();
    let output = erode(&input);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout.len(), 16 + input.len());
    assert_eq!(&output.stdout[0..4], b"MAPE");
    assert_eq!(
        u32::from_le_bytes(output.stdout[4..8].try_into().unwrap()),
        1
    );
    assert_eq!(
        u32::from_le_bytes(output.stdout[8..12].try_into().unwrap()),
        33
    );
    assert_eq!(
        u32::from_le_bytes(output.stdout[12..16].try_into().unwrap()),
        1
    );
    assert_eq!(output.stdout[16..], input);
}

#[test]
fn erosion_rejects_truncated_trailing_and_invalid_encoded_heights() {
    for input in [
        vec![0; 12],
        vec![0; 33 * 33 * 2 + 1],
        vec![255; 33 * 33 * 2],
    ] {
        let output = erode(&input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
        .args(["erode", "65537", "1000", "2"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn protocol_is_versioned_and_little_endian() {
    let output = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
        .args(["12345", "33", "2", "40", "15", "15", "30"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let bytes = output.stdout;
    assert_eq!(bytes.len(), 16 + 33 * 33 * 5);
    assert_eq!(&bytes[0..4], b"MAPT");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
    assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 12345);
    assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 33);
    assert_eq!(f32::from_le_bytes(bytes[16..20].try_into().unwrap()), 0.0);
    assert_eq!(bytes[16 + 33 * 33 * 4], 0);
}

#[test]
fn invalid_requests_do_not_write_partial_responses() {
    for args in [
        vec!["0", "33", "1", "40", "15", "15", "30"],
        vec!["1", "65537", "1", "40", "15", "15", "30"],
        vec!["1", "33", "0", "40", "15", "15", "30"],
        vec!["1", "33", "1", "40", "15", "15", "31"],
        vec![],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn world_protocol_streams_layers_with_progress_and_timings() {
    let output = world_output(
        &[
            "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0", "1",
            "4", "12",
        ],
        &disabled_monuments(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_header: Vec<u8> = b"MAPL"
        .iter()
        .copied()
        .chain(
            [4_u32, 1000, 12345, 1025, 512, 0, 10]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        )
        .collect();
    assert_eq!(&output.stdout[..32], expected_header);
    assert_eq!(
        output.stdout.len(),
        32 + 80 + 1025 * 1025 * 4 + 512 * 512 * 18 + 8
    );
    for bytes in output.stdout[32..112].chunks_exact(8) {
        let value = f64::from_le_bytes(bytes.try_into().unwrap());
        assert!(value.is_finite() && value >= 0.0);
    }
    assert!(
        output.stdout[output.stdout.len() - 8 - 512 * 512..output.stdout.len() - 8]
            .iter()
            .all(|&a| a == 255)
    );
    let progress = String::from_utf8(output.stderr).unwrap();
    assert!(progress.contains("MAPPER_PROGRESS\t0.45\tBiomes"));
    assert!(!progress.contains("Terrain erosion"));
    assert!(progress.contains("MAPPER_PROGRESS\t0.81\tPaths"));
    assert!(progress.contains("MAPPER_PROGRESS\t0.82\tMonument placement"));
    assert_eq!(
        &output.stdout[output.stdout.len() - 4..],
        &0_u32.to_le_bytes()
    );
}

#[test]
fn invalid_world_requests_have_no_partial_output() {
    for (size, erode, width) in [
        ("6001", "1", "8"),
        ("1000", "2", "8"),
        ("1000", "0", "NaN"),
        ("1000", "0", "1001"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
            .args([
                "world", size, "12345", "2", "40", "15", "15", "30", erode, "0", width, "0", "0",
                "1", "4", "12",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("MAPPER_PROGRESS"));
    }
}

#[test]
fn world_protocol_includes_ring_and_branch_records() {
    let output = world_output(
        &[
            "world",
            "1000",
            "2147483647",
            "2",
            "40",
            "15",
            "15",
            "30",
            "0",
            "0",
            "8",
            "0",
            "1",
            "1",
            "2",
            "12.125",
        ],
        &disabled_monuments(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = output.stdout;
    let mut cursor = 32 + 80 + 1025 * 1025 * 4 + 512 * 512 * 18;
    assert_eq!(
        u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()),
        3
    );
    cursor += 4;
    for (kind, count) in [(2_u32, 129_u32), (1, 33), (1, 33)] {
        assert_eq!(
            u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()),
            kind
        );
        assert_eq!(
            f32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()),
            12.125
        );
        assert_eq!(
            u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into().unwrap()),
            count
        );
        cursor += 12;
        for node in bytes[cursor..cursor + count as usize * 8].chunks_exact(4) {
            let coordinate = f32::from_le_bytes(node.try_into().unwrap());
            assert!(coordinate.is_finite() && coordinate.abs() <= 500.0);
        }
        cursor += count as usize * 8;
    }
    assert_eq!(&bytes[cursor..cursor + 4], &0_u32.to_le_bytes());
    assert_eq!(cursor + 4, bytes.len());
}

#[test]
fn world_protocol_places_from_the_supplied_registry() {
    let mut input = b"MAPM".to_vec();
    for value in [2_u32, 1, 0, 1, 1, 1, 0, 0, 1, 1000] {
        input.extend(value.to_le_bytes());
    }
    input.extend(20_f32.to_le_bytes());
    input.extend(20_f32.to_le_bytes());
    input.extend(1_i32.to_le_bytes());
    input.extend((-180_f32).to_le_bytes());
    input.extend(180_f32.to_le_bytes());
    for value in [1_u32, 0, 0, 0] {
        input.extend(value.to_le_bytes());
    }
    for value in [u32::from_le_bytes(*b"MAPS"), 1, 0, 0] {
        input.extend(value.to_le_bytes());
    }
    let output = world_output(
        &[
            "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0", "1",
            "4", "12",
        ],
        &input,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let offset = 32 + 80 + 1025 * 1025 * 4 + 512 * 512 * 18 + 4;
    assert_eq!(output.stdout.len(), offset + 24);
    assert_eq!(&output.stdout[offset..offset + 4], &1_u32.to_le_bytes());
    assert_eq!(&output.stdout[offset + 4..offset + 8], &0_u32.to_le_bytes());
    let values: Vec<f32> = output.stdout[offset + 8..]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    assert!(values.iter().all(|v| v.is_finite()));
    assert!(
        values[0].abs() <= 500.0
            && values[2].abs() <= 500.0
            && values[1] > 4.0
            && values[3].abs() <= 180.0
    );
}

#[test]
fn malformed_monument_inputs_fail_before_generation() {
    let mut trailing = disabled_monuments();
    trailing.push(0);
    for input in [
        vec![],
        b"wrong".to_vec(),
        disabled_monuments()[..20].to_vec(),
        trailing,
        vec![0; papyrus_terrain::monuments::MAX_INPUT_BYTES + 1],
    ] {
        let output = world_output(
            &[
                "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", "0",
                "1", "4", "12",
            ],
            &input,
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("MAPPER_PROGRESS"));
    }
}

#[test]
fn invalid_road_requests_fail_before_progress_and_output() {
    for (enabled, branches, width) in [
        ("2", "3", "12"),
        ("1", "4096", "12"),
        ("1", "3", "NaN"),
        ("1", "3", "0"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_papyrus-terrain"))
            .args([
                "world", "1000", "12345", "2", "40", "15", "15", "30", "0", "0", "8", "0", enabled,
                "1", branches, width,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("MAPPER_PROGRESS"));
    }
}
