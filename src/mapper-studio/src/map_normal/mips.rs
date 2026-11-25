//! Independent linear box mips are built before BC3 compression, matching the
//! inspected Apply-then-Compress ordering. Unity byte parity remains unverified.
use super::*;

pub(super) const VERSION: &str = "papyrus-map-normal-box-bc3-rangefit-1";

pub(super) fn generate(
    height: impl Fn(usize, usize) -> Option<i16> + Sync,
    settings: Settings,
    pool: Option<&rayon::ThreadPool>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    if cancelled() {
        return Ok(None);
    }
    let n = settings.height_resolution;
    if !(17..=4097).contains(&n) || !(n - 1).is_power_of_two() {
        return Err("Unsupported normal mip dimensions".into());
    }
    let resolution = (n - 1) / 2;
    let mut levels = Vec::new();
    let mut pixels = 0;
    let mut blocks = 0;
    let mut filter_work = 0;
    let mut size = resolution;
    loop {
        levels.push(Level {
            resolution: size,
            offset: pixels,
        });
        pixels += size * size;
        blocks += size.div_ceil(4).pow(2);
        if size == 1 {
            break;
        }
        size /= 2;
        filter_work += size * size * 5;
    }
    let pixel_bytes = pixels * 4;
    // The complete output, current raw level and next raw level coexist.
    let working_bytes = working_bytes(resolution, true);
    let work = n * n + resolution * resolution * 128 + blocks * 4096 + filter_work;
    if work > settings.maximum_work || working_bytes > settings.maximum_bytes {
        return Err("Normal mips exceed their work or image memory allowance".into());
    }
    let Some(raw) = generate_base_in(height, settings, pool, false, &cancelled)? else {
        return Ok(None);
    };
    let mut raw = Arc::try_unwrap(raw.pixels).map_err(|_| "Unexpected shared raw normal input")?;
    let mut output = vec![[0; 4]; pixels];
    for level in &levels {
        if cancelled() {
            return Ok(None);
        }
        let size = level.resolution;
        let next = if size > 1 {
            let half = size / 2;
            let mut next = vec![[0; 4]; half * half];
            for row in 0..half {
                if cancelled() {
                    return Ok(None);
                }
                for col in 0..half {
                    next[row * half + col] = std::array::from_fn(|channel| {
                        let at = |x: usize, z: usize| u16::from(raw[z * size + x][channel]);
                        let sum = at(col * 2, row * 2)
                            + at(col * 2 + 1, row * 2)
                            + at(col * 2, row * 2 + 1)
                            + at(col * 2 + 1, row * 2 + 1);
                        (f32::from(sum) * 0.25).round_ties_even() as u8
                    });
                }
            }
            next
        } else {
            Vec::new()
        };
        let target = &mut output[level.offset..level.offset + size * size];
        for (batch, pixels) in target.chunks_mut(size * 32).enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let mut run = || {
                pixels
                    .par_chunks_mut(size * 4)
                    .enumerate()
                    .for_each(|(block_row, rows)| {
                        for block_col in 0..size.div_ceil(4) {
                            let mut block = [[0; 4]; 16];
                            let mut mask = 0;
                            for (i, pixel) in block.iter_mut().enumerate() {
                                let x = block_col * 4 + i % 4;
                                let z = batch * 32 + block_row * 4 + i / 4;
                                if x < size && z < size {
                                    *pixel = raw[z * size + x];
                                    mask |= 1 << i;
                                }
                            }
                            let decoded = compress(block, mask);
                            for (i, pixel) in decoded.into_iter().enumerate() {
                                let x = block_col * 4 + i % 4;
                                let row = i / 4;
                                if x < size && row * size + x < rows.len() {
                                    rows[row * size + x] = pixel;
                                }
                            }
                        }
                    });
            };
            if let Some(pool) = pool {
                pool.install(run);
            } else {
                run();
            }
        }
        raw = next;
    }
    Ok((!cancelled()).then(|| Grid {
        resolution,
        pixels: Arc::new(output),
        levels,
        report: Report {
            engine: VERSION,
            resolution,
            blocks,
            work_reserved: work,
            pixel_bytes,
            working_bytes,
        },
    }))
}
