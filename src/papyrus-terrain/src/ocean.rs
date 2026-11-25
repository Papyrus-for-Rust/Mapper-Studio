//! Cancellable ocean reconstruction over retained terrain. Source arrays are never modified.
use std::collections::VecDeque;
pub const VERSION: &str = "papyrus-ocean-1";
pub const OCEAN_BIT: i32 = 128;
pub const LAND: u8 = 0;
pub const OCEAN: u8 = 1;
pub const ENCLOSED: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ocean {
    pub resolution: usize,
    pub classification: Vec<u8>,
    pub topology: Vec<i32>,
    /// Local water defaults to encoded -500 m. The ocean uses the global sea plane.
    pub local_water: Vec<i16>,
    pub ocean_cells: usize,
    pub enclosed_cells: usize,
    pub changed_topology_cells: usize,
}
impl Ocean {
    pub fn bytes(&self) -> usize {
        self.classification.len() + self.topology.len() * 4 + self.local_water.len() * 2
    }

    /// Reclassify the sea after terrain changes while retaining existing local water.
    pub fn refresh_topology(
        &mut self,
        height: &[i16],
        height_resolution: usize,
        source_topology: &[i32],
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if self.local_water.len() != height.len() {
            return Err("Invalid ocean local water dimensions".into());
        }
        let Some(classified) = classify_topology(
            height,
            height_resolution,
            source_topology,
            self.resolution,
            &cancelled,
        )? else {
            return Ok(false);
        };
        if cancelled() {
            return Ok(false);
        }
        self.classification = classified.classification;
        self.topology = classified.topology;
        self.ocean_cells = classified.ocean_cells;
        self.enclosed_cells = classified.enclosed_cells;
        self.changed_topology_cells = classified.changed_topology_cells;
        Ok(true)
    }
}
/// Boundary-connected sea mask, without allocating topology or local-water grids.
#[derive(Debug, PartialEq, Eq)]
pub struct Mask {
    pub resolution: usize,
    pub classification: Vec<u8>,
}
/// Classify exactly the height samples used by final ocean reconstruction,
/// without allocating topology or local-water rasters.
pub fn classify_height(
    height: &[i16],
    height_resolution: usize,
    surface_resolution: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Mask>, String> {
    if !(33..=4097).contains(&height_resolution)
        || !(height_resolution - 1).is_power_of_two()
        || !(32..=2048).contains(&surface_resolution)
        || !surface_resolution.is_power_of_two()
        || height.len() != height_resolution * height_resolution
    {
        return Err("Invalid ocean reconstruction dimensions".into());
    }
    for chunk in height.chunks(32768) {
        if cancelled() {
            return Ok(None);
        }
        if chunk.iter().any(|h| !(0..=32766).contains(h)) {
            return Err("Invalid ocean source heights".into());
        }
    }
    let n = surface_resolution;
    let sample_axis: Vec<_> = (0..n)
        .map(|cell| cell * (height_resolution - 1) / (n - 1))
        .collect();
    classify_sampled(
        n,
        |x, z| Ok(height[sample_axis[z] * height_resolution + sample_axis[x]] <= 16383),
        cancelled,
    )
}
pub fn reconstruct(
    height: &[i16],
    height_resolution: usize,
    source_topology: &[i32],
    surface_resolution: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Ocean>, String> {
    reconstruct_with_water(
        height,
        height_resolution,
        source_topology,
        surface_resolution,
        None,
        cancelled,
    )
}

/// Rebuild the ocean and copy existing local water directly into the new grid.
/// Omitting local water creates a dry grid for the global sea plane.
pub fn reconstruct_with_water(
    height: &[i16],
    height_resolution: usize,
    source_topology: &[i32],
    surface_resolution: usize,
    local_water: Option<&[i16]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Ocean>, String> {
    if local_water.is_some_and(|water| water.len() != height.len()) {
        return Err("Invalid ocean local water dimensions".into());
    }
    let Some(classified) = classify_topology(
        height,
        height_resolution,
        source_topology,
        surface_resolution,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    Ok(build_ocean(
        classified,
        surface_resolution,
        height.len(),
        local_water,
        &cancelled,
    ))
}

/// Apply a previously computed sea classification to updated topology and
/// retained water. The caller supplies the mask for the current terrain.
pub fn reconstruct_from_mask(
    mask: Mask,
    source_topology: &[i32],
    height_resolution: usize,
    local_water: &[i16],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Ocean>, String> {
    if !(33..=4097).contains(&height_resolution)
        || !(height_resolution - 1).is_power_of_two()
        || local_water.len() != height_resolution * height_resolution
    {
        return Err("Invalid ocean local water dimensions".into());
    }
    let resolution = mask.resolution;
    let Some(classified) = apply_classification(mask, source_topology, &cancelled)? else {
        return Ok(None);
    };
    Ok(build_ocean(
        classified,
        resolution,
        local_water.len(),
        Some(local_water),
        &cancelled,
    ))
}

fn build_ocean(
    classified: ClassifiedTopology,
    surface_resolution: usize,
    height_cells: usize,
    local_water: Option<&[i16]>,
    cancelled: &impl Fn() -> bool,
) -> Option<Ocean> {
    let mut water_grid = Vec::with_capacity(height_cells);
    for samples in (0..height_cells).step_by(32768) {
        if cancelled() {
            return None;
        }
        let end = (samples + 32768).min(height_cells);
        if let Some(water) = local_water {
            water_grid.extend_from_slice(&water[samples..end]);
        } else {
            water_grid.resize(end, 0);
        }
    }
    Some(Ocean {
        resolution: surface_resolution,
        classification: classified.classification,
        topology: classified.topology,
        local_water: water_grid,
        ocean_cells: classified.ocean_cells,
        enclosed_cells: classified.enclosed_cells,
        changed_topology_cells: classified.changed_topology_cells,
    })
}

/// Update only the ocean bit for callers that route over classified terrain.
pub fn reconstruct_topology(
    height: &[i16],
    height_resolution: usize,
    source_topology: &[i32],
    surface_resolution: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<i32>>, String> {
    Ok(classify_topology(
        height,
        height_resolution,
        source_topology,
        surface_resolution,
        &cancelled,
    )?
    .filter(|_| !cancelled())
    .map(|classified| classified.topology))
}

struct ClassifiedTopology {
    classification: Vec<u8>,
    topology: Vec<i32>,
    ocean_cells: usize,
    enclosed_cells: usize,
    changed_topology_cells: usize,
}

fn classify_topology(
    height: &[i16],
    height_resolution: usize,
    source_topology: &[i32],
    surface_resolution: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ClassifiedTopology>, String> {
    if !(33..=4097).contains(&height_resolution)
        || !(height_resolution - 1).is_power_of_two()
        || !(32..=2048).contains(&surface_resolution)
        || !surface_resolution.is_power_of_two()
        || height.len() != height_resolution * height_resolution
        || source_topology.len() != surface_resolution * surface_resolution
    {
        return Err("Invalid ocean reconstruction dimensions".into());
    }
    let n = surface_resolution;
    let Some(mask) = classify_height(height, height_resolution, n, cancelled)? else {
        return Ok(None);
    };
    apply_classification(mask, source_topology, cancelled)
}

fn apply_classification(
    mask: Mask,
    source_topology: &[i32],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ClassifiedTopology>, String> {
    let n = mask.resolution;
    if !(32..=2048).contains(&n)
        || !n.is_power_of_two()
        || mask.classification.len() != n * n
        || source_topology.len() != n * n
    {
        return Err("Invalid ocean reconstruction dimensions".into());
    }
    let classification = mask.classification;
    let cells = n * n;
    let mut topology = Vec::with_capacity(cells);
    let mut ocean_cells = 0;
    let mut enclosed_cells = 0;
    let mut changed_topology_cells = 0;
    for (i, (&original, &class)) in source_topology.iter().zip(&classification).enumerate() {
        if i % 16384 == 0 && cancelled() {
            return Ok(None);
        }
        let value = if class == OCEAN {
            ocean_cells += 1;
            original | OCEAN_BIT
        } else {
            if class > ENCLOSED {
                return Err("Invalid ocean classification".into());
            }
            enclosed_cells += usize::from(class == ENCLOSED);
            original & !OCEAN_BIT
        };
        changed_topology_cells += usize::from(value != original);
        topology.push(value);
    }
    Ok(Some(ClassifiedTopology {
        classification,
        topology,
        ocean_cells,
        enclosed_cells,
        changed_topology_cells,
    }))
}

/// Samples native ocean-grid probes, then floods four-connected sea from the map
/// boundary. The callback owns height interpretation; errors/cancellation publish
/// no partial mask. Enclosed depressions do not become ocean.
pub fn classify_sampled(
    n: usize,
    mut below_sea: impl FnMut(usize, usize) -> Result<bool, String>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Mask>, String> {
    if !(32..=2048).contains(&n) || !n.is_power_of_two() {
        return Err("Invalid sampled ocean dimensions".into());
    }
    let mut classification = vec![LAND; n * n];
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..n {
            if below_sea(x, z)? {
                classification[z * n + x] = ENCLOSED;
            }
        }
    }
    let mut queue = VecDeque::new();
    for i in 0..n {
        if i % 64 == 0 && cancelled() {
            return Ok(None);
        }
        enqueue_span(&mut classification[..n], &mut queue, 0, i);
        enqueue_span(&mut classification[(n - 1) * n..], &mut queue, n - 1, i);
        let cells = &mut classification[i * n..(i + 1) * n];
        enqueue_span(cells, &mut queue, i, 0);
        enqueue_span(cells, &mut queue, i, n - 1);
    }
    let mut visited = 0;
    let mut check_after = 0;
    while let Some(Span { row, left, right }) = queue.pop_front() {
        let (row, left, right) = (usize::from(row), usize::from(left), usize::from(right));
        if visited >= check_after {
            if cancelled() {
                return Ok(None);
            }
            check_after = visited + 4096;
        }
        visited += right - left + 1;
        // A neighboring row can connect at any column in the current span.
        for neighbor in [row.checked_sub(1), (row + 1 < n).then_some(row + 1)]
            .into_iter()
            .flatten()
        {
            let cells = &mut classification[neighbor * n..(neighbor + 1) * n];
            let mut x = left;
            while x <= right {
                if cells[x] == ENCLOSED {
                    x = enqueue_span(cells, &mut queue, neighbor, x);
                } else {
                    x += 1;
                }
            }
        }
    }
    Ok((!cancelled()).then_some(Mask {
        resolution: n,
        classification,
    }))
}

struct Span {
    // Validated ocean grids cap every coordinate at 2047.
    row: u16,
    left: u16,
    right: u16,
}

/// Mark the whole unseen horizontal run reached at x. Return the next column
/// the caller needs to inspect, including when x is already marked or dry.
fn enqueue_span(cells: &mut [u8], queue: &mut VecDeque<Span>, row: usize, x: usize) -> usize {
    if cells[x] != ENCLOSED {
        return x + 1;
    }
    let mut left = x;
    while left > 0 && cells[left - 1] == ENCLOSED {
        left -= 1;
    }
    let mut right = x;
    while right + 1 < cells.len() && cells[right + 1] == ENCLOSED {
        right += 1;
    }
    if left == right {
        cells[x] = OCEAN;
    } else {
        cells[left..=right].fill(OCEAN);
    }
    queue.push_back(Span {
        row: row as u16,
        left: left as u16,
        right: right as u16,
    });
    right + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn sampled_mask_rejects_errors_and_cancels_without_partial_results() {
        assert!(classify_sampled(31, |_, _| Ok(true), || false).is_err());
        let visits = Cell::new(0);
        assert!(
            classify_sampled(
                32,
                |_, _| {
                    visits.set(visits.get() + 1);
                    if visits.get() == 3 {
                        Err("source stopped".into())
                    } else {
                        Ok(true)
                    }
                },
                || false
            )
            .is_err()
        );
        assert_eq!(visits.get(), 3);
        let calls = Cell::new(0);
        assert!(
            classify_sampled(
                128,
                |_, _| Ok(true),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 130
                }
            )
            .unwrap()
            .is_none()
        );
        let mask = classify_sampled(32, |x, z| Ok(x == 0 || (x == 10 && z == 10)), || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            mask.classification.iter().filter(|&&v| v == OCEAN).count(),
            32
        );
        assert_eq!(mask.classification[10 * 32 + 10], ENCLOSED);
    }
    #[test]
    fn ocean_is_boundary_connected_preserves_source_and_other_flags() {
        let n = 32;
        let hr = 33;
        let mut heights = vec![20000; hr * hr];
        heights[..hr].fill(15000);
        heights[10 * hr + 10] = 15000;
        heights[11 * hr + 11] = 15000;
        let original = heights.clone();
        let flags = vec![i32::MIN | OCEAN_BIT | 16384 | 65536; n * n];
        let saved = flags.clone();
        let result = reconstruct(&heights, hr, &flags, n, || false)
            .unwrap()
            .unwrap();
        let mask = classify_height(&heights, hr, n, || false).unwrap().unwrap();
        assert_eq!(mask.classification, result.classification);
        let topology = reconstruct_topology(&heights, hr, &flags, n, || false)
            .unwrap()
            .unwrap();
        assert_eq!(topology, result.topology);
        assert!(classify_height(&heights, hr, n, || true).unwrap().is_none());
        assert!(classify_height(&heights[..10], hr, n, || false).is_err());
        assert_eq!(result.ocean_cells, n);
        assert_eq!(result.enclosed_cells, 2);
        assert_eq!(result.classification[10 * n + 10], ENCLOSED);
        assert_eq!(result.topology[10 * n + 10] & OCEAN_BIT, 0);
        for (&a, &b) in flags.iter().zip(&result.topology) {
            assert_eq!(a & !OCEAN_BIT, b & !OCEAN_BIT);
        }
        assert!(result.local_water.iter().all(|w| *w == 0));
        assert_eq!(heights, original);
        assert_eq!(flags, saved);
        assert_eq!(
            result,
            reconstruct(&heights, hr, &flags, n, || false)
                .unwrap()
                .unwrap()
        );
    }

    #[test]
    fn reclassifying_sea_keeps_the_original_local_water_allocation() {
        let mut heights = vec![20000; 33 * 33];
        let flags = vec![16384; 32 * 32];
        let water = vec![18123; heights.len()];
        let mut existing = reconstruct_with_water(&heights, 33, &flags, 32, Some(&water), || false)
            .unwrap()
            .unwrap();
        let allocation = existing.local_water.as_ptr();
        heights[0] = 16000;
        let before = existing.clone();
        assert!(!existing.refresh_topology(&heights, 33, &flags, || true).unwrap());
        assert_eq!(existing, before);
        assert!(existing.refresh_topology(&heights, 33, &flags, || false).unwrap());
        let mut expected = reconstruct(&heights, 33, &flags, 32, || false).unwrap().unwrap();
        expected.local_water = water;
        assert_eq!(existing, expected);
        assert_eq!(existing.local_water.as_ptr(), allocation);
    }
    #[test]
    fn flooded_diagonals_are_not_connections_and_sea_level_is_inclusive() {
        let mut heights = vec![20000; 33 * 33];
        heights[0] = 16383;
        heights[34] = 16383;
        let result = reconstruct(&heights, 33, &vec![0; 32 * 32], 32, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.ocean_cells, 1);
        assert_eq!(result.enclosed_cells, 1);
        heights[1] = 16383;
        let result = reconstruct(&heights, 33, &vec![0; 32 * 32], 32, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.ocean_cells, 3);
        assert_eq!(result.enclosed_cells, 0);
    }
    #[test]
    fn flood_follows_bending_channels_with_single_cell_connections() {
        let n = 32;
        let mask = classify_sampled(
            n,
            |x, z| {
                Ok(match z {
                    10 => x <= 6,
                    11 => (6..=18).contains(&x),
                    12 => (18..=22).contains(&x) || x == 25,
                    13 => (22..=24).contains(&x),
                    _ => false,
                })
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(mask.classification[13 * n + 24], OCEAN);
        assert_eq!(mask.classification[12 * n + 25], ENCLOSED);
        assert_eq!(
            mask.classification.iter().filter(|&&v| v == OCEAN).count(),
            28
        );
    }
    #[test]
    fn cancellation_during_flood_and_invalid_inputs_return_no_partial_result() {
        let heights = vec![15000; 129 * 129];
        let flags = vec![0; 128 * 128];
        let calls = Cell::new(0);
        assert!(
            reconstruct(&heights, 129, &flags, 128, || {
                calls.set(calls.get() + 1);
                calls.get() > 130
            })
            .unwrap()
            .is_none()
        );
        assert!(reconstruct(&heights, usize::MAX, &flags, 32, || false).is_err());
        assert!(reconstruct(&heights[..10], 33, &flags, 32, || false).is_err());
        let mut bad = heights;
        bad[0] = -1;
        assert!(reconstruct(&bad, 33, &flags, 32, || false).is_err());
    }
}
