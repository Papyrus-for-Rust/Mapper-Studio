//! Exact live topology unions with small, transaction-owned block summaries.
use std::collections::BTreeSet;
pub(crate) const BLOCK: usize = 16;
pub(crate) struct Index {
    n: usize,
    side: usize,
    masks: Vec<u32>,
    cells_ptr: usize,
}
/// A summary bound to its immutable cell slice. A view with another topology
/// allocation cannot accidentally use this summary.
#[derive(Clone, Copy)]
pub struct Tiles<'a> {
    cells: &'a [i32],
    index: &'a Index,
}
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Query {
    pub rect: [usize; 4],
    pub center: [usize; 2],
    pub radius: f32,
    pub relevant: u32,
}
impl Query {
    /// Source radius sampling combines a world-normalized rectangle with a
    /// raw integer-cell circle. Callers validate the frame and grid first.
    pub(crate) fn normalized(
        n: usize,
        uv: [f32; 2],
        radius: f32,
        inverse_size: f32,
        relevant: u32,
    ) -> Self {
        let index = |v: f32| ((v * n as f32) as isize).clamp(0, n as isize - 1) as usize;
        let r = inverse_size * radius;
        Self {
            rect: [
                index(uv[0] - r),
                index(uv[0] + r),
                index(uv[1] - r),
                index(uv[1] + r),
            ],
            center: [index(uv[0]), index(uv[1])],
            radius,
            relevant,
        }
    }
}
impl Index {
    pub(crate) fn allocation_bytes(n: usize) -> Result<u64, String> {
        if !(1..=4096).contains(&n) {
            return Err("Invalid topology index grid".into());
        }
        Ok((n.div_ceil(BLOCK).pow(2) * std::mem::size_of::<u32>()) as u64)
    }

    pub fn new(
        cells: &[i32],
        n: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if !(1..=4096).contains(&n) || n.checked_mul(n) != Some(cells.len()) {
            return Err("Invalid topology index grid".into());
        }
        let side = n.div_ceil(BLOCK);
        let mut index = Self {
            n,
            side,
            masks: vec![0; side * side],
            cells_ptr: cells.as_ptr() as usize,
        };
        for z in 0..side {
            if cancelled() {
                return Ok(None);
            }
            for x in 0..side {
                index.refresh_one(cells, z * side + x);
            }
        }
        Ok(Some(index))
    }
    pub fn view<'a>(&'a self, cells: &'a [i32]) -> Tiles<'a> {
        Tiles { cells, index: self }
    }
    pub fn dirty(&self, indices: impl Iterator<Item = usize>) -> BTreeSet<usize> {
        indices
            .map(|i| (i / self.n / BLOCK) * self.side + (i % self.n / BLOCK))
            .collect()
    }
    pub fn refresh_cost(&self, dirty: &BTreeSet<usize>) -> usize {
        dirty
            .iter()
            .map(|&i| {
                (self.n - i / self.side * BLOCK).min(BLOCK)
                    * (self.n - i % self.side * BLOCK).min(BLOCK)
            })
            .sum()
    }
    pub fn refresh(&mut self, cells: &[i32], dirty: &BTreeSet<usize>) {
        for &i in dirty {
            self.refresh_one(cells, i);
        }
    }
    fn refresh_one(&mut self, cells: &[i32], i: usize) {
        let x = i % self.side * BLOCK;
        let z = i / self.side * BLOCK;
        let mut mask = 0;
        for row in z..(z + BLOCK).min(self.n) {
            for &v in &cells[row * self.n + x..row * self.n + (x + BLOCK).min(self.n)] {
                mask |= v as u32;
            }
        }
        self.masks[i] = mask;
    }
}
impl Tiles<'_> {
    pub(crate) fn matches(self, cells: &[i32], n: usize) -> bool {
        self.index.n == n
            && self.index.cells_ptr == cells.as_ptr() as usize
            && std::ptr::eq(self.cells, cells)
    }
    /// An absent required-any bit rules out the rest of this tile on the row.
    pub(crate) fn missing_any_until(self, x: usize, z: usize, any: u32) -> Option<usize> {
        let tx = x / BLOCK;
        let tz = z / BLOCK;
        (self.index.masks[tz * self.index.side + tx] & any == 0)
            .then(|| ((tx + 1) * BLOCK).min(self.index.n))
    }
    pub(crate) fn union(self, q: Query, work: &mut usize, limit: usize) -> Result<u32, ()> {
        let charge = |work: &mut usize| {
            if *work >= limit {
                Err(())
            } else {
                *work += 1;
                Ok(())
            }
        };
        let [x0, x1, z0, z1] = q.rect;
        let radius_squared = q.radius * q.radius;
        let inside = |x: usize, z: usize| {
            let dx = x as f32 - q.center[0] as f32;
            let dz = z as f32 - q.center[1] as f32;
            dx * dx + dz * dz <= radius_squared
        };
        let mut mask = 0;
        if q.relevant == 0 {
            return Ok(mask);
        }
        for tz in z0 / BLOCK..=z1 / BLOCK {
            for tx in x0 / BLOCK..=x1 / BLOCK {
                charge(work)?;
                let bits = self.index.masks[tz * self.index.side + tx] & q.relevant;
                if bits == 0 {
                    continue;
                }
                let bx = tx * BLOCK;
                let bz = tz * BLOCK;
                let ex = (bx + BLOCK).min(self.index.n) - 1;
                let ez = (bz + BLOCK).min(self.index.n) - 1;
                let (left, right) = (bx.max(x0), ex.min(x1));
                let (top, bottom) = (bz.max(z0), ez.min(z1));
                // The circle contains the intersection if it contains the
                // corner farthest from its center on both axes.
                let dx = left.abs_diff(q.center[0]).max(right.abs_diff(q.center[0])) as f32;
                let dz = top.abs_diff(q.center[1]).max(bottom.abs_diff(q.center[1])) as f32;
                let covered = dx * dx + dz * dz <= radius_squared;
                if covered && (left, right, top, bottom) == (bx, ex, bz, ez) {
                    mask |= bits;
                    continue;
                }
                let count = (right - left + 1) * (bottom - top + 1);
                // The scalar scan charged every cell, including those outside
                // the circle. A partial scan consumed the remaining budget.
                let charged = count.min(limit - *work);
                *work += charged;
                if charged != count {
                    return Err(());
                }
                // A block with no new bits cannot change the result. Keep
                // charging the original area so work limits stay identical.
                if bits & !mask == 0 {
                    continue;
                }
                for z in top..=bottom {
                    let start = z * self.index.n;
                    let row = &self.cells[start + left..=start + right];
                    if covered {
                        for &cell in row {
                            mask |= cell as u32;
                        }
                    } else {
                        for (i, &cell) in row.iter().enumerate() {
                            let new_bits = cell as u32 & bits & !mask;
                            if new_bits != 0 && inside(left + i, z) {
                                mask |= new_bits;
                            }
                        }
                    }
                    if bits & !mask == 0 {
                        break;
                    }
                }
            }
        }
        Ok(mask & q.relevant)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_unions_match_exact_rectangular_and_circle_edges_and_updates() {
        let n = 67;
        let mut cells: Vec<_> = (0..n * n)
            .map(|i| 1_i32 << ((i * 17 + i / n) % 24))
            .collect();
        let mut index = Index::new(&cells, n, &|| false).unwrap().unwrap();
        for changed in [false, true] {
            if changed {
                let mut dirty = index.dirty((0..cells.len()).filter(|i| i % 7 == 0));
                for i in (0..cells.len()).filter(|i| i % 7 == 0) {
                    cells[i] = 0;
                }
                for z in 0..BLOCK {
                    for x in 0..BLOCK {
                        cells[z * n + x] = 0;
                    }
                }
                dirty.insert(0);
                cells[33 * n + 34] = 1 << 30;
                dirty.extend(index.dirty(std::iter::once(33 * n + 34)));
                assert!(index.refresh_cost(&dirty) <= n * n);
                index.refresh(&cells, &dirty);
                assert_eq!(index.masks[0], 0);
                assert_eq!(
                    index.view(&cells).union(
                        Query {
                            rect: [32, 47, 32, 47],
                            center: [34, 33],
                            radius: 90.,
                            relevant: 1 << 30
                        },
                        &mut 0,
                        10000
                    ),
                    Ok(1 << 30)
                );
            }
            for k in 0..128 {
                let rect = [
                    k % n,
                    (k % n + 19).min(n - 1),
                    (k * 11) % n,
                    ((k * 11) % n + 23).min(n - 1),
                ];
                let center = [(k * 3) % n, (k * 13) % n];
                let radius = (k % 41) as f32 + 0.25;
                let relevant = if k % 3 == 0 { 0 } else { 0x40a5_a55a };
                let mut expected = 0;
                for z in rect[2]..=rect[3] {
                    for x in rect[0]..=rect[1] {
                        let dx = x as f32 - center[0] as f32;
                        let dz = z as f32 - center[1] as f32;
                        if dx * dx + dz * dz <= radius * radius {
                            expected |= cells[z * n + x] as u32 & relevant;
                        }
                    }
                }
                let got = index
                    .view(&cells)
                    .union(
                        Query {
                            rect,
                            center,
                            radius,
                            relevant,
                        },
                        &mut 0,
                        100000,
                    )
                    .unwrap();
                assert_eq!(got, expected, "query {k}, changed {changed}");
            }
        }
        let other = cells.clone();
        assert!(!index.view(&cells).matches(&other, n));
        assert!(!index.view(&other).matches(&other, n));
        assert!(Index::new(&cells, n, &|| true).unwrap().is_none());
    }
    #[test]
    fn clipped_windows_keep_the_same_cell_charge_inside_and_across_the_circle() {
        let n = 32;
        let mut cells = vec![0; n * n];
        cells[15 * n + 15] = 1;
        cells[8 * n + 14] = 2;
        cells[16 * n + 15] = 4;
        cells[19 * n + 20] = 8;
        let index = Index::new(&cells, n, &|| false).unwrap().unwrap();
        let tiles = index.view(&cells);
        let mut query = Query {
            rect: [14, 20, 8, 19],
            center: [15, 15],
            radius: 1.,
            relevant: 15,
        };
        // Four intersecting tiles, one with no relevant bits. The other
        // clipped areas contain 16, 8, and 20 cells.
        let cost = 4 + 16 + 8 + 20;
        for (radius, expected) in [(1., 5), (100., 15)] {
            query.radius = radius;
            for allowance in 0..=cost + 1 {
                let mut work = 3;
                let limit = work + allowance;
                assert_eq!(
                    tiles.union(query, &mut work, limit),
                    if allowance >= cost {
                        Ok(expected)
                    } else {
                        Err(())
                    }
                );
                assert_eq!(work, 3 + cost.min(allowance));
            }
        }
    }
    #[test]
    fn repeated_bits_in_clipped_blocks_still_consume_the_full_allowance() {
        let n = 32;
        let mut cells = vec![1; n * n];
        cells[10 * n + 16] = 3;
        let index = Index::new(&cells, n, &|| false).unwrap().unwrap();
        let tiles = index.view(&cells);
        let query = Query {
            rect: [10, 20, 10, 20],
            center: [15, 15],
            radius: 8.,
            relevant: 3,
        };
        let cost = 4 + 11 * 11;
        let mut work = 0;
        assert_eq!(tiles.union(query, &mut work, cost), Ok(3));
        assert_eq!(work, cost);
        work = 0;
        assert_eq!(tiles.union(query, &mut work, cost - 1), Err(()));
        assert_eq!(work, cost - 1);
    }

    #[test]
    fn irrelevant_blocks_are_cheap_and_budget_exhaustion_is_explicit() {
        let cells = vec![1; 256 * 256];
        let index = Index::new(&cells, 256, &|| false).unwrap().unwrap();
        let query = || Query {
            rect: [0, 255, 0, 255],
            center: [128, 128],
            radius: 1000.,
            relevant: 2,
        };
        let mut work = 0;
        assert_eq!(index.view(&cells).union(query(), &mut work, 1000), Ok(0));
        assert_eq!(work, 256);
        assert!(index.view(&cells).union(query(), &mut 0, 1).is_err());
    }
}
