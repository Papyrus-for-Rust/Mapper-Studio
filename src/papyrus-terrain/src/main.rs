use papyrus_terrain::{Settings, erosion, generate, monuments, paths, registry, water, world};
use std::io::{self, BufWriter, Read, Write};

fn world(args: &[String]) -> Result<(), String> {
    if args.len() != 16 && args.len() != 17 {
        return Err("usage: papyrus-terrain world <size> <seed> <threads> <arid> <temperate> <tundra> <arctic> <erosion=0|1> <river-count> <river-width> <river-length> <roads=0|1> <ring=0|1> <branches> <road-width> [map-timestamp]".into());
    }
    let settings = world::Settings {
        size: args[1].parse().map_err(|_| "invalid size")?,
        seed: args[2].parse().map_err(|_| "invalid seed")?,
        threads: args[3].parse().map_err(|_| "invalid threads")?,
        biomes: [
            args[4].parse().map_err(|_| "invalid arid percentage")?,
            args[5]
                .parse()
                .map_err(|_| "invalid temperate percentage")?,
            args[6].parse().map_err(|_| "invalid tundra percentage")?,
            args[7].parse().map_err(|_| "invalid arctic percentage")?,
        ],
        erode: match args[8].as_str() {
            "0" => false,
            "1" => true,
            _ => return Err("invalid erosion switch".into()),
        },
        rivers: water::Rivers {
            count: args[9].parse().map_err(|_| "invalid river count")?,
            width: args[10].parse().map_err(|_| "invalid river width")?,
            maximum_length: args[11].parse().map_err(|_| "invalid river length")?,
        },
        roads: paths::Roads {
            enabled: parse_switch(&args[12])?,
            ring: parse_switch(&args[13])?,
            branches: args[14].parse().map_err(|_| "invalid branch count")?,
            width: args[15].parse().map_err(|_| "invalid road width")?,
        },
    };
    settings.validate()?;
    let timestamp = if args.len() == 17 {
        let value: u64 = args[16].parse().map_err(|_| "invalid map timestamp")?;
        if value > i64::MAX as u64 {
            return Err("invalid map timestamp".into());
        }
        Some(value)
    } else {
        None
    };
    let stdin = io::stdin();
    let mut input = Vec::new();
    stdin
        .lock()
        .take((monuments::MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    let prepared = if input.starts_with(b"MAPR") {
        Some(registry::Prepared::read(&input, settings)?)
    } else {
        None
    };
    let legacy = if prepared.is_none() {
        Some(monuments::Request::read(&input)?)
    } else {
        None
    };
    let monument_request = prepared
        .as_ref()
        .map(|v| &v.request)
        .or(legacy.as_ref())
        .ok_or("missing registry request")?;
    if timestamp.is_some() && prepared.is_none() {
        return Err("direct export requires the local registry protocol".into());
    }
    let layers = world::generate_layers(settings, monument_request, |stage, fraction| {
        eprintln!("MAPPER_PROGRESS\t{fraction}\t{stage}")
    })?;
    let stdout = io::stdout();
    // Batch encoded values into large pipe writes rather than flushing thousands of small buffers.
    let mut output = BufWriter::with_capacity(1024 * 1024, stdout.lock());
    let write = |output: &mut BufWriter<io::StdoutLock<'_>>| -> io::Result<()> {
        output.write_all(b"MAPL")?;
        for value in [
            if timestamp.is_some() {
                6
            } else if prepared.is_some() {
                5
            } else {
                4
            },
            settings.size as u32,
            settings.seed as u32,
            settings.height_resolution() as u32,
            settings.surface_resolution() as u32,
            u32::from(settings.erode),
            10,
        ] {
            output.write_all(&value.to_le_bytes())?;
        }
        for value in layers.milliseconds {
            output.write_all(&value.to_le_bytes())?;
        }
        for value in &layers.height {
            output.write_all(&value.to_le_bytes())?;
        }
        for value in &layers.water {
            output.write_all(&value.to_le_bytes())?;
        }
        output.write_all(&layers.biome)?;
        output.write_all(&layers.splat)?;
        for value in &layers.topology {
            output.write_all(&value.to_le_bytes())?;
        }
        output.write_all(&layers.alpha)?;
        output.write_all(&(layers.paths.len() as u32).to_le_bytes())?;
        for path in &layers.paths {
            output.write_all(&path.kind.to_le_bytes())?;
            output.write_all(&path.width.to_le_bytes())?;
            output.write_all(&(path.points.len() as u32).to_le_bytes())?;
            for point in &path.points {
                output.write_all(&point.x.to_le_bytes())?;
                output.write_all(&point.z.to_le_bytes())?;
            }
        }
        output.write_all(&(layers.prefabs.len() as u32).to_le_bytes())?;
        for prefab in &layers.prefabs {
            output.write_all(&(prefab.entry as u32).to_le_bytes())?;
            for value in [prefab.x, prefab.y, prefab.z, prefab.yaw] {
                output.write_all(&value.to_le_bytes())?;
            }
            if let Some(prepared) = &prepared {
                output.write_all(&prepared.ids[prefab.entry].to_le_bytes())?;
                let path = &prepared.paths[prefab.entry];
                output.write_all(&(path.len() as u32).to_le_bytes())?;
                output.write_all(path.as_bytes())?;
            }
        }
        if let Some(timestamp) = timestamp {
            eprintln!("MAPPER_PROGRESS\t0.9\tMap export");
            output.write_all(b"MAPC")?;
            output.write_all(&1_u32.to_le_bytes())?;
            let registry = prepared
                .as_ref()
                .ok_or_else(|| io::Error::other("missing local registry"))?;
            let profile = papyrus_terrain::export::generated(
                settings,
                &layers,
                registry,
                timestamp,
                &mut *output,
            )?;
            eprintln!("MAPPER_EXPORT_TIME\t{}", profile.total_ms);
        }
        output.flush()
    };
    write(&mut output).map_err(|e| e.to_string())
}

fn parse_switch(value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err("invalid boolean switch: expected 0 or 1".into()),
    }
}

fn erode(args: &[String]) -> Result<(), String> {
    if !(4..=5).contains(&args.len()) {
        return Err(
            "usage: papyrus-terrain erode <resolution> <world-size> <threads> [iterations=512]".into(),
        );
    }
    let settings = erosion::Settings {
        resolution: args[1].parse().map_err(|_| "invalid resolution")?,
        world_size: args[2].parse().map_err(|_| "invalid world size")?,
        threads: args[3].parse().map_err(|_| "invalid threads")?,
        iterations: if args.len() == 5 {
            args[4].parse().map_err(|_| "invalid iterations")?
        } else {
            erosion::ITERATIONS
        },
    };
    settings.validate()?;
    let mut input = vec![0; settings.resolution * settings.resolution * 2];
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    reader.read_exact(&mut input).map_err(|e| e.to_string())?;
    let mut trailing = [0];
    if reader.read(&mut trailing).map_err(|e| e.to_string())? != 0 {
        return Err("trailing erosion input data".into());
    }
    let mut heights: Vec<i16> = input
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    drop(input);
    erosion::apply(&mut heights, settings)?;
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    let write = |output: &mut BufWriter<io::StdoutLock<'_>>| -> io::Result<()> {
        output.write_all(b"MAPE")?;
        output.write_all(&1_u32.to_le_bytes())?;
        output.write_all(&(settings.resolution as u32).to_le_bytes())?;
        output.write_all(&(settings.iterations as u32).to_le_bytes())?;
        for height in heights {
            output.write_all(&height.to_le_bytes())?;
        }
        output.flush()
    };
    write(&mut output).map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "encode") {
        if args.len() != 1 {
            return Err("usage: papyrus-terrain encode".into());
        }
        let profile = papyrus_terrain::export::encode(
            io::stdin().lock(),
            BufWriter::with_capacity(1024 * 1024, io::stdout().lock()),
        )
        .map_err(|e| e.to_string())?;
        eprintln!(
            "MAPPER_EXPORT_PROFILE\t{}",
            serde_json::json!({"compression":profile.compression_ms,"output":profile.output_ms,"input":profile.read_ms,"validation":profile.validation_ms,"total":profile.total_ms})
        );
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "world") {
        return world(&args);
    }
    if args.first().is_some_and(|arg| arg == "erode") {
        return erode(&args);
    }
    if args.len() != 7 {
        return Err("usage: papyrus-terrain <seed> <resolution> <threads> <arid> <temperate> <tundra> <arctic>\nBinary MAPT v1 is written to stdout; diagnostics go to stderr.".into());
    }
    let settings = Settings {
        seed: args[0].parse().map_err(|_| "invalid seed")?,
        resolution: args[1].parse().map_err(|_| "invalid resolution")?,
        threads: args[2].parse().map_err(|_| "invalid threads")?,
        biomes: [
            args[3].parse().map_err(|_| "invalid arid percentage")?,
            args[4]
                .parse()
                .map_err(|_| "invalid temperate percentage")?,
            args[5].parse().map_err(|_| "invalid tundra percentage")?,
            args[6].parse().map_err(|_| "invalid arctic percentage")?,
        ],
    };
    let field = generate(settings)?;
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    let write = |output: &mut BufWriter<io::StdoutLock<'_>>| -> io::Result<()> {
        output.write_all(b"MAPT")?;
        output.write_all(&1_u32.to_le_bytes())?;
        output.write_all(&settings.seed.to_le_bytes())?;
        output.write_all(&(settings.resolution as u32).to_le_bytes())?;
        for height in field.heights {
            output.write_all(&height.to_le_bytes())?;
        }
        output.write_all(&field.biomes)?;
        output.flush()
    };
    write(&mut output).map_err(|error| error.to_string())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
