use super::{Batch, CliffPrefab, PREDICTION_CHUNK, Position, Prediction, PredictionChunk};
use crate::{decor, native_layers::View, topology_query::BLOCK};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::{Arc, Mutex},
};

type Key = [u8; 32];
type Forecast = Arc<PredictionChunk>;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_OVERHEAD: usize =
    size_of::<Key>() + size_of::<Forecast>() + 32 + size_of::<PredictionChunk>();
const MIN_CHUNK_BYTES: usize = CHUNK_OVERHEAD + size_of::<Prediction>();
#[derive(Default)]
struct Cache {
    chunks: HashMap<Key, Forecast>,
    identities: HashSet<Key>,
    bytes: usize,
}

impl Cache {
    fn insert(&mut self, identity: Key, key: Key, forecast: Forecast, maximum_bytes: usize) {
        let identity_bytes =
            usize::from(!self.identities.contains(&identity)) * (size_of::<Key>() + 32);
        let bytes =
            CHUNK_OVERHEAD + size_of::<Prediction>() * forecast.entries.capacity() + identity_bytes;
        if bytes <= maximum_bytes.saturating_sub(self.bytes) && !self.chunks.contains_key(&key) {
            self.chunks.insert(key, forecast);
            if identity_bytes != 0 {
                self.identities.insert(identity);
            }
            self.bytes += bytes;
        }
    }
}

#[derive(Default)]
struct Replay {
    previous: Arc<Cache>,
    next: Cache,
    reused_chunks: usize,
    reused_predictions: usize,
    maximum_bytes: usize,
}

static ACCEPTED: Mutex<Option<Arc<Cache>>> = Mutex::new(None);
thread_local! {
    static ACTIVE: RefCell<Option<Replay>> = const { RefCell::new(None) };
    static RETAIN: Cell<bool> = const { Cell::new(true) };
}

/// A one-shot command has no later edit that can reuse the forecast cache.
pub fn without_forecast_replay<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            RETAIN.set(self.0);
        }
    }
    let _restore = Restore(RETAIN.replace(false));
    run()
}

/// Keep one bounded forecast cache across successful object-composition runs.
/// Each lookup independently verifies its inputs, including the live rasters.
pub fn while_composing<T>(
    maximum_bytes: usize,
    run: impl FnOnce() -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
    if !RETAIN.get() {
        return run();
    }
    struct Restore(Option<Replay>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.replace(self.0.take()));
        }
    }
    let Ok(mut accepted) = ACCEPTED.try_lock() else {
        return run();
    };
    let maximum_bytes = maximum_bytes.min(MAX_BYTES);
    if maximum_bytes == 0
        || accepted
            .as_ref()
            .is_some_and(|cache| cache.bytes > maximum_bytes)
    {
        *accepted = None;
    }
    if maximum_bytes == 0 {
        drop(accepted);
        return run();
    }
    let previous = accepted.as_ref().cloned().unwrap_or_default();
    let _restore = Restore(ACTIVE.with(|active| {
        active.replace(Some(Replay {
            previous,
            maximum_bytes,
            ..Replay::default()
        }))
    }));
    let result = run();
    if result.as_ref().is_ok_and(Option::is_some) {
        let replay = ACTIVE
            .with(|active| active.take())
            .expect("Active cliff replay");
        if std::env::var_os("MAPPER_COMPOSITION_TIMINGS").is_some() {
            eprintln!(
                "MAPPER_CLIFF_REPLAY {}",
                serde_json::json!({"reused_chunks": replay.reused_chunks,
                "reused_predictions":replay.reused_predictions,
                "retained_chunks":replay.next.chunks.len(), "retained_bytes":replay.next.bytes})
            );
        }
        *accepted = Some(Arc::new(replay.next));
    }
    result
}

pub(super) fn find(key: Key) -> Option<Forecast> {
    ACTIVE.with(|active| {
        let mut slot = active.borrow_mut();
        let replay = slot.as_mut()?;
        let found = replay.previous.chunks.get(&key)?.clone();
        replay.reused_chunks += 1;
        replay.reused_predictions += found.entries.len();
        Some(found)
    })
}

pub(super) struct Tiles {
    identity: Key,
    side: usize,
    radius: usize,
    materials: MaterialReads,
    values: Vec<Option<Key>>,
}

#[derive(Clone, Copy, Default)]
struct MaterialReads {
    biome: bool,
    splat: bool,
}

impl MaterialReads {
    fn from_filters<'a>(filters: impl Iterator<Item = &'a crate::spawn::Filter>) -> Self {
        filters.fold(Self::default(), |reads, filter| Self {
            biome: reads.biome || filter.biome_type != -1,
            splat: reads.splat || filter.splat_type != -1,
        })
    }
}

impl Tiles {
    pub(super) fn new(
        stage: &decor::Stage,
        seed: u32,
        view: View<'_>,
        prefabs: &[CliffPrefab<'_>],
        radii: &[Option<usize>],
    ) -> Option<Self> {
        if !needs_keys(None) {
            return None;
        }
        let radius = radii.iter().flatten().copied().max()?;
        let metadata: Vec<_> = prefabs
            .iter()
            .map(|entry| (entry.index, entry.prefab.metadata()))
            .collect();
        let mut key = Sha256::new();
        key.update(
            serde_json::to_vec(&(
                stage,
                seed,
                view.size,
                view.height_resolution,
                view.surface_resolution,
                metadata,
            ))
            .ok()?,
        );
        key.update([
            u8::from(view.topology_tiles.is_some()),
            u8::from(view.placement_blockers.is_some()),
        ]);
        let identity = key.finalize().into();
        if !needs_keys(Some(identity)) {
            return None;
        }
        // The stage and prefab filters are already covered by the identity.
        // The tilt search and transforms do not sample any other materials.
        let materials = MaterialReads::from_filters(std::iter::once(&stage.filter).chain(
            prefabs.iter().flat_map(|entry| {
                entry
                    .prefab
                    .metadata()
                    .filters
                    .iter()
                    .map(|point| &point.filter)
            }),
        ));
        let side = view.surface_resolution.div_ceil(BLOCK);
        Some(Self {
            identity,
            side,
            radius,
            materials,
            values: vec![None; side * side],
        })
    }

    pub(super) fn keys(
        &mut self,
        batch: &Batch<'_, '_>,
        positions: &[Position],
    ) -> Vec<Option<Key>> {
        // Once there is nothing to look up and no room for another nonempty
        // chunk, the remaining batches can fit normally without hashing.
        if !needs_keys(Some(self.identity)) {
            return Vec::new();
        }
        for &index in batch.transaction.changed_cliff_tiles() {
            self.values[index] = None;
        }
        positions
            .chunks(PREDICTION_CHUNK)
            .map(|group| self.key(batch, group))
            .collect()
    }

    pub(super) fn retain(&self, key: Key, forecast: Forecast) {
        ACTIVE.with(|active| {
            if let Some(replay) = active.borrow_mut().as_mut() {
                replay
                    .next
                    .insert(self.identity, key, forecast, replay.maximum_bytes);
            }
        });
    }

    fn key(&mut self, batch: &Batch<'_, '_>, positions: &[Position]) -> Option<Key> {
        let view = batch.transaction.view();
        let n = view.surface_resolution;
        let (mut left, mut right, mut top, mut bottom) = (n, 0, n, 0);
        for &position in positions {
            if let Some([x, z]) = position.cell(n) {
                let cell = z * n + x;
                if batch.alpha[cell] >= 128
                    && position.passes(cell, batch.stage.stage(), batch.transaction)
                {
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(z);
                    bottom = bottom.max(z);
                }
            }
        }
        if left > right {
            return None;
        }
        let mut key = Sha256::new();
        key.update(self.identity);
        for position in positions {
            key.update(bytemuck::bytes_of(&[
                position.xz[0],
                position.xz[1],
                position.threshold,
            ]));
        }
        let left = left.saturating_sub(self.radius) / BLOCK;
        let right = (right + self.radius).min(n - 1) / BLOCK;
        let top = top.saturating_sub(self.radius) / BLOCK;
        let bottom = (bottom + self.radius).min(n - 1) / BLOCK;
        key.update(bytemuck::bytes_of(&[left, right, top, bottom]));
        for z in top..=bottom {
            for x in left..=right {
                let value = self.values[z * self.side + x]
                    .get_or_insert_with(|| digest_tile(view, batch.alpha, x, z, self.materials));
                key.update(value);
            }
        }
        Some(key.finalize().into())
    }
}

fn needs_keys(identity: Option<Key>) -> bool {
    if !RETAIN.get() {
        return false;
    }
    ACTIVE.with(|active| {
        active.borrow().as_ref().is_some_and(|replay| {
            let available = replay.maximum_bytes - replay.next.bytes;
            match identity {
                Some(identity) => {
                    let identity_bytes = usize::from(!replay.next.identities.contains(&identity))
                        * (size_of::<Key>() + 32);
                    replay.previous.identities.contains(&identity)
                        || available >= MIN_CHUNK_BYTES + identity_bytes
                }
                None => !replay.previous.chunks.is_empty() || available >= MIN_CHUNK_BYTES,
            }
        })
    })
}

fn digest_tile(view: View<'_>, alpha: &[u8], x: usize, z: usize, reads: MaterialReads) -> Key {
    let n = view.surface_resolution;
    let hr = view.height_resolution;
    let left = x * BLOCK;
    let right = ((x + 1) * BLOCK).min(n);
    let top = z * BLOCK;
    let bottom = ((z + 1) * BLOCK).min(n);
    let vertex = |cell: usize| {
        if cell == n {
            hr
        } else {
            (cell * (hr - 1)).div_ceil(n)
        }
    };
    let mut hash = Sha256::new();
    let materials = view
        .biome
        .chunks_exact(n * n)
        .take(if reads.biome { 5 } else { 0 })
        .chain(
            view.splat
                .chunks_exact(n * n)
                .take(if reads.splat { 8 } else { 0 }),
        );
    for row in vertex(top)..vertex(bottom) {
        hash.update(bytemuck::cast_slice(
            &view.height[row * hr + vertex(left)..row * hr + vertex(right)],
        ));
    }
    for row in top..bottom {
        let from = row * n + left;
        let to = row * n + right;
        hash.update(bytemuck::cast_slice(&view.topology[from..to]));
        hash.update(&alpha[from..to]);
        if let Some(placement) = view.placement_blockers {
            hash.update(&placement[from..to]);
        }
        for layer in materials.clone() {
            hash.update(&layer[from..to]);
        }
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_cache_only_hashes_matching_runs_and_one_shot_work_skips_replay() {
        let identity = [1; 32];
        let key = [2; 32];
        let forecast = Arc::new(PredictionChunk {
            entries: vec![Prediction {
                prefab: 0,
                reserved_scale: None,
                work: [1, 0],
                outcome: crate::decor_fit::OrientedOutcome::Rejected(
                    crate::decor_fit::OrientedRejection::Filter,
                ),
            }],
            ..Default::default()
        });
        let mut cache = Cache::default();
        let maximum_bytes = MIN_CHUNK_BYTES + size_of::<Key>() + 32;
        cache.insert(identity, key, forecast.clone(), maximum_bytes);
        assert_eq!(cache.bytes, maximum_bytes);
        ACTIVE.with(|active| {
            active.replace(Some(Replay {
                previous: Arc::new(cache),
                maximum_bytes,
                next: Cache {
                    bytes: maximum_bytes,
                    ..Cache::default()
                },
                ..Replay::default()
            }));
        });
        assert!(needs_keys(Some(identity)));
        assert!(Arc::ptr_eq(&find(key).unwrap(), &forecast));
        assert!(!needs_keys(Some([3; 32])));
        without_forecast_replay(|| assert!(!needs_keys(Some(identity))));
        assert!(needs_keys(Some(identity)));
        ACTIVE.with(|active| active.replace(None));
    }

    #[test]
    fn tile_receipts_follow_live_channels_and_the_last_height_vertex() {
        let n = 32;
        let hr = 65;
        let mut height = vec![1_i16; hr * hr];
        let mut topology = vec![0_i32; n * n];
        let mut splat = vec![0_u8; n * n * 8];
        let mut biome = vec![0_u8; n * n * 5];
        let mut alpha = vec![255_u8; n * n];
        let mut placement = vec![0_u8; n * n];
        macro_rules! tile {
            ($x:expr, $z:expr, $reads:expr) => {
                digest_tile(
                    View {
                        size: 1000,
                        height_resolution: hr,
                        surface_resolution: n,
                        height: &height,
                        biome: &biome,
                        splat: &splat,
                        topology: &topology,
                        placement_blockers: Some(&placement),
                        topology_tiles: None,
                    },
                    &alpha,
                    $x,
                    $z,
                    $reads,
                )
            };
        }
        let unweighted_filter = crate::spawn::Filter {
            biome_type: -1,
            splat_type: -1,
            topology_any: 1,
            topology_all: 0,
            topology_not: 0,
        };
        let biome_filter = crate::spawn::Filter {
            biome_type: 16,
            ..unweighted_filter.clone()
        };
        let splat_filter = crate::spawn::Filter {
            splat_type: 128,
            ..unweighted_filter.clone()
        };
        let both = MaterialReads::from_filters([&biome_filter, &splat_filter].into_iter());
        let topology_only = MaterialReads::from_filters(std::iter::once(&unweighted_filter));
        let biome_only = MaterialReads::from_filters(std::iter::once(&biome_filter));
        let splat_only = MaterialReads::from_filters(std::iter::once(&splat_filter));
        let unweighted = tile!(1, 1, topology_only);
        let original_biome = tile!(1, 1, biome_only);
        let original_splat = tile!(1, 1, splat_only);
        biome[n * n * 5 - 1] = 192;
        assert_ne!(tile!(1, 1, biome_only), original_biome);
        assert_eq!(tile!(1, 1, splat_only), original_splat);
        let current_biome = tile!(1, 1, biome_only);
        splat[n * n * 8 - 1] = 127;
        assert_ne!(tile!(1, 1, splat_only), original_splat);
        assert_eq!(tile!(1, 1, biome_only), current_biome);
        assert_eq!(tile!(1, 1, topology_only), unweighted);
        let left = tile!(0, 1, both);
        let mut right = tile!(1, 1, both);
        height[hr * hr - 1] = 2;
        assert_ne!(tile!(1, 1, both), right);
        right = tile!(1, 1, both);
        topology[n * n - 1] = 1 << 4;
        assert_ne!(tile!(1, 1, both), right);
        right = tile!(1, 1, both);
        alpha[n * n - 1] = 0;
        assert_ne!(tile!(1, 1, both), right);
        right = tile!(1, 1, both);
        placement[n * n - 1] = 1;
        assert_ne!(tile!(1, 1, both), right);
        assert_eq!(tile!(0, 1, both), left);
    }
}
