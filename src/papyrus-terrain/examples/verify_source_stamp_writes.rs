use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
};
#[derive(Deserialize)]
struct Query {
    kind: Option<String>,
    old: Vec<u8>,
    source: Vec<f32>,
    opacity: f32,
}
#[derive(Deserialize)]
struct Input {
    queries: Vec<Query>,
}
#[derive(Deserialize)]
struct Oracle {
    format: String,
    input_sha256: String,
    oracle_sha256: String,
    results: Vec<Vec<u8>>,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("Input, oracle, new audit required".into());
    }
    let read = |path: &std::path::Path| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("Diagnostic input exceeds limit".into());
        }
        Ok(bytes)
    };
    let bytes = read(args[0].as_ref())?;
    let input: Input = serde_json::from_slice(&bytes)?;
    let oracle: Oracle = serde_json::from_slice(&read(args[1].as_ref())?)?;
    if input.queries.is_empty()
        || input.queries.len() > 100000
        || input.queries.len() != oracle.results.len()
        || oracle.format != "mapper-owned-raw-stamp-oracle-1"
        || oracle.input_sha256 != format!("{:x}", Sha256::digest(&bytes))
    {
        return Err("Invalid or stale raw-write oracle".into());
    }
    let mut mismatches = 0;
    for (q, expected) in input.queries.iter().zip(&oracle.results) {
        let rock = q.kind.as_deref() == Some("rock");
        if (if rock {
            q.old.len() != 8 || q.source.len() != 1
        } else {
            q.source.len() != q.old.len()
        }) || expected.len() != q.old.len()
            || !q.opacity.is_finite()
            || !(0. ..=1.).contains(&q.opacity)
            || q.source
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
        {
            return Err("Invalid raw-write query".into());
        }
        let actual: Vec<_> = if rock {
            papyrus_terrain::monument_composition::source_set_rock(
                q.old.as_slice().try_into()?,
                q.source[0],
            )
            .to_vec()
        } else {
            match q.old.len() {
                1 => vec![papyrus_terrain::monument_composition::source_alpha(
                    q.old[0],
                    q.source[0],
                    q.opacity,
                )],
                5 => papyrus_terrain::monument_composition::source_mixed_weights::<5>(
                    q.old.as_slice().try_into()?,
                    q.source.as_slice().try_into()?,
                    q.opacity,
                )
                .to_vec(),
                8 => papyrus_terrain::monument_composition::source_mixed_weights::<8>(
                    q.old.as_slice().try_into()?,
                    q.source.as_slice().try_into()?,
                    q.opacity,
                )
                .to_vec(),
                _ => return Err("Unsupported raw-write channel count".into()),
            }
        };
        mismatches += actual.iter().zip(expected).filter(|(a, b)| a != b).count();
    }
    let report = serde_json::json!({"format":"mapper-source-stamp-write-audit-1","input_sha256":oracle.input_sha256,"oracle_sha256":oracle.oracle_sha256,"queries":input.queries.len(),"byte_mismatches":mismatches,"full_stamp_validated":false,"full_map_validated":false});
    serde_json::to_writer_pretty(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[2])?,
        &report,
    )?;
    println!("{report}");
    if mismatches > 0 {
        return Err("Source stamp write mismatch".into());
    }
    Ok(())
}
