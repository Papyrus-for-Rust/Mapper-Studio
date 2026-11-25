//! Live, bounded decoration sampling. Each candidate is returned before the next
//! terrain read, so its caller can fit/admit it into the shared destination.
use crate::{
    decor::{self, Candidate, Kind, Stage, StageReport},
    native_layers::{View, dimensions},
    spawn,
};
pub const VERSION: &str = "papyrus-live-decor-candidates-3";
pub struct Budget {
    maximum: usize,
    used: usize,
    exhausted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_layers::Layers;
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            biome: vec![51; n * n * 5],
            splat: vec![32; n * n * 8],
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn stage(kind: Kind) -> Stage {
        Stage {
            id: "20".into(),
            order: 0,
            name: "Rocks".into(),
            kind,
            folder: "decor/rocks".into(),
            enabled: true,
            mode: -1,
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: 1,
                topology_all: 0,
                topology_not: 0,
            },
            filter_mode: 1,
            anchor_mode: 0,
            distance: 50.,
            dithering: 0.,
            density: 100.,
            cluster: (kind == Kind::ValueNoise).then_some(decor::Cluster {
                octaves: 2,
                frequency: 0.4,
                amplitude: 1.,
                offset: 0.,
            }),
            prefabs: vec![
                "assets/bundled/prefabs/autospawn/decor/rocks/a.prefab".into(),
                "assets/bundled/prefabs/autospawn/decor/rocks/b.prefab".into(),
            ],
        }
    }
    #[test]
    fn unmodified_small_stage_streams_match_existing_seed_positions_and_prefab_choices() {
        let mut l = layers();
        let cells = l.surface_resolution.pow(2);
        let mut alpha = vec![255; cells];
        for (cell, (alpha, topology)) in alpha.iter_mut().zip(&mut l.topology).enumerate() {
            if cell % 7 == 0 {
                *alpha = 0;
            }
            if cell % 11 == 0 {
                *topology = 0;
            }
        }
        for (kind, dithering, filter_mode, splat_type) in [
            (Kind::Uniform, 0., 1, 1),
            (Kind::Uniform, 20., 1, 1),
            (Kind::Uniform, 20., 0, 1),
            (Kind::Uniform, 20., 1, -1),
            (Kind::WhiteNoise, 0., 0, 1),
            (Kind::ValueNoise, 0., 0, -1),
        ] {
            let mut s = stage(kind);
            s.dithering = dithering;
            s.filter_mode = filter_mode;
            s.filter.splat_type = splat_type;
            let legacy =
                decor::generate(&l, &alpha, std::slice::from_ref(&s), 42, "test", || false)
                    .unwrap()
                    .unwrap();
            let mut stream = Stream::new(&s, 0, l.size, 42, 2).unwrap();
            let mut budget = Budget::new(10000).unwrap();
            let mut candidates = Vec::new();
            loop {
                match stream
                    .next(l.view(), &alpha, &mut budget, || false)
                    .unwrap()
                {
                    Advance::Candidate(c) => candidates.push(c),
                    Advance::Complete => break,
                    Advance::Cancelled => panic!("unexpected cancellation"),
                }
            }
            assert_eq!(
                serde_json::to_value(candidates).unwrap(),
                serde_json::to_value(legacy.candidates).unwrap()
            );
            assert_eq!(
                serde_json::to_value(stream.finish().unwrap()).unwrap(),
                serde_json::to_value(&legacy.stages[0]).unwrap()
            );
        }
    }
    #[test]
    fn next_candidate_reads_changed_height_and_filters_without_copying_or_resetting_sampling() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage(Kind::Uniform);
        let mut stream = Stream::new(&s, 0, l.size, 42, 2).unwrap();
        let mut budget = Budget::new(1000).unwrap();
        assert!(matches!(
            stream
                .next(l.view(), &alpha, &mut budget, || false)
                .unwrap(),
            Advance::Candidate(_)
        ));
        let height = vec![crate::erosion::encode(60.); l.height.len()];
        let view = View {
            height: &height,
            ..l.view()
        };
        let Advance::Candidate(c) = stream.next(view, &alpha, &mut budget, || false).unwrap()
        else {
            panic!("missing live candidate")
        };
        assert!((c.position[1] - 60.).abs() < 0.04);
        assert_eq!(budget.used(), 2);
        let topo = vec![0; l.topology.len()];
        assert!(matches!(
            stream
                .next(
                    View {
                        topology: &topo,
                        ..view
                    },
                    &alpha,
                    &mut budget,
                    || false
                )
                .unwrap(),
            Advance::Complete
        ));
        let report = stream.finish().unwrap();
        assert_eq!(report.candidates, 2);
        assert_eq!(report.rejected_filter, 398);
        assert_eq!(budget.used(), 400);
        assert!(l.height.iter().all(|h| *h == 18000));
    }
    #[test]
    fn live_placement_map_rejects_pivots_and_disabled_pivots_preserve_candidates() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let blocked = vec![1; alpha.len()];
        let view = View {
            placement_blockers: Some(&blocked),
            ..l.view()
        };
        for enabled in [true, false] {
            let mut s = stage(Kind::Uniform);
            s.filter_mode = i32::from(enabled);
            let mut stream = Stream::new(&s, 0, l.size, 42, 2).unwrap();
            let mut budget = Budget::new(1000).unwrap();
            let mut count = 0;
            loop {
                match stream.next(view, &alpha, &mut budget, || false).unwrap() {
                    Advance::Candidate(_) => count += 1,
                    Advance::Complete => break,
                    Advance::Cancelled => panic!("unexpected cancellation"),
                }
            }
            assert_eq!(count, if enabled { 0 } else { 400 });
        }
    }

    #[test]
    fn regular_tile_skips_preserve_live_reads_rng_and_attempt_charges() {
        for maximum in [127, 50_000] {
            let mut terrain = layers();
            let n = terrain.surface_resolution;
            terrain.topology.fill(0);
            terrain.topology[0] = 1;
            let mut alpha = vec![255; n * n];
            for hole in alpha.iter_mut().step_by(17).skip(1) {
                *hole = 0;
            }
            let mut index = crate::topology_query::Index::new(&terrain.topology, n, &|| false)
                .unwrap().unwrap();
            let mut s = stage(Kind::Uniform);
            s.distance = 5.;
            let mut indexed = Stream::new(&s, 0, terrain.size, 42, 2).unwrap();
            let mut scalar = Stream::new(&s, 0, terrain.size, 42, 2).unwrap();
            let mut indexed_budget = Budget::new(maximum).unwrap();
            let mut scalar_budget = Budget::new(maximum).unwrap();
            let mut candidates = 0;
            loop {
                let actual = indexed.next(View {
                    topology_tiles: Some(index.view(&terrain.topology)), ..terrain.view()
                }, &alpha, &mut indexed_budget, || false);
                let expected = scalar.next(terrain.view(), &alpha, &mut scalar_budget, || false);
                match (actual, expected) {
                    (Ok(Advance::Candidate(actual)), Ok(Advance::Candidate(expected))) => {
                        assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(expected).unwrap());
                        candidates += 1;
                        if candidates == 1 {
                            // Make a previously ineligible tile live between
                            // admissions, then compare the next draw.
                            let row = (500. / 1000. * n as f32) as usize;
                            let cell = row * n + row;
                            terrain.topology[cell] = 1;
                            index.refresh(&terrain.topology, &index.dirty(std::iter::once(cell)));
                        }
                    }
                    (Ok(Advance::Complete), Ok(Advance::Complete)) => break,
                    (Err(a), Err(b)) => {
                        assert_eq!(a, b);
                        break;
                    }
                    (a, b) => panic!("Indexed and scalar streams differ: {a:?}, {b:?}"),
                }
            }
            assert_eq!(candidates, if maximum == 127 { 1 } else { 2 });
            assert_eq!(indexed_budget.used(), scalar_budget.used());
            assert_eq!(indexed.rng, scalar.rng);
            assert_eq!(
                serde_json::to_value(indexed.report()).unwrap(),
                serde_json::to_value(scalar.report()).unwrap(),
            );
            assert!(indexed.report().rejected_holes > 0);
        }
    }
    #[test]
    fn dense_stages_are_not_thinned_and_shared_work_limits_survive_stage_boundaries() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage(Kind::Uniform);
        s.distance = 1.;
        let mut stream = Stream::new(&s, 0, l.size, 42, 2).unwrap();
        assert_eq!(stream.report().requested_attempts, 1_000_000);
        let mut budget = Budget::new(2).unwrap();
        for x in [-500., -499.] {
            let Advance::Candidate(c) = stream
                .next(l.view(), &alpha, &mut budget, || false)
                .unwrap()
            else {
                panic!("candidate missing")
            };
            assert_eq!(c.position[0], x);
        }
        assert!(
            stream
                .next(l.view(), &alpha, &mut budget, || false)
                .is_err()
        );
        assert!(stream.finish().is_err());
        let mut other = Stream::new(&s, 1, l.size, 42, 2).unwrap();
        assert!(other.next(l.view(), &alpha, &mut budget, || false).is_err());
        assert_eq!(budget.used(), 2);
    }
    #[test]
    fn cancellation_and_invalid_views_are_terminal_and_source_realms_remain_data_driven() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage(Kind::WhiteNoise);
        s.mode = 2;
        let mut budget = Budget::new(1000).unwrap();
        let mut excluded = Stream::new(&s, 0, l.size, 42, 1).unwrap();
        assert!(matches!(
            excluded
                .next(l.view(), &alpha, &mut budget, || false)
                .unwrap(),
            Advance::Complete
        ));
        assert_eq!(budget.used(), 0);
        let mut stream = Stream::new(&s, 0, l.size, 42, 2).unwrap();
        assert!(matches!(
            stream
                .next(l.view(), &alpha, &mut budget, || false)
                .unwrap(),
            Advance::Candidate(_)
        ));
        assert!(matches!(
            stream.next(l.view(), &alpha, &mut budget, || true).unwrap(),
            Advance::Cancelled
        ));
        assert!(matches!(
            stream
                .next(l.view(), &alpha, &mut budget, || false)
                .unwrap(),
            Advance::Cancelled
        ));
        assert!(stream.finish().is_err());
        let mut stream = Stream::new(&s, 0, l.size, 42, 2).unwrap();
        assert!(
            stream
                .next(
                    View {
                        height: &l.height[..1],
                        ..l.view()
                    },
                    &alpha,
                    &mut budget,
                    || false
                )
                .is_err()
        );
        assert!(
            stream
                .next(l.view(), &alpha, &mut budget, || false)
                .is_err()
        );
        assert!(stream.finish().is_err());
    }
}
impl Budget {
    pub fn new(maximum: usize) -> Result<Self, String> {
        if !(1..=128_000_000).contains(&maximum) {
            return Err("Invalid live decoration attempt allowance".into());
        }
        Ok(Self {
            maximum,
            used: 0,
            exhausted: false,
        })
    }
    pub fn used(&self) -> usize {
        self.used
    }
    pub(crate) fn remaining(&self) -> usize {
        self.maximum - self.used
    }
    pub(crate) fn take(&mut self) -> Result<(), String> {
        if self.exhausted || self.remaining() == 0 {
            self.exhausted = true;
            return Err("Live decoration exhausted its shared attempt allowance".into());
        }
        self.used += 1;
        Ok(())
    }
}
#[derive(Debug)]
pub enum Advance {
    Candidate(Candidate),
    Complete,
    Cancelled,
}
#[derive(PartialEq, Eq)]
enum State {
    Ready,
    Complete,
    Cancelled,
    Failed,
}
pub struct Stream<'a> {
    stage: &'a Stage,
    stage_index: usize,
    size: u32,
    hr: usize,
    n: usize,
    side: usize,
    regular_cells: Vec<usize>,
    rng: u64,
    report: StageReport,
    state: State,
}
impl<'a> Stream<'a> {
    pub fn new(
        stage: &'a Stage,
        stage_index: usize,
        size: u32,
        seed: u32,
        realm_mask: u32,
    ) -> Result<Self, String> {
        stage.validate()?;
        let (hr, n) = dimensions(size)?;
        if stage_index >= 128 || realm_mask == 0 {
            return Err("Invalid live decoration stage or realm".into());
        }
        let side = if stage.kind == Kind::Uniform {
            (size as f64 / f64::from(stage.distance)).ceil() as usize
        } else {
            0
        };
        let requested = if side > 0 {
            side * side
        } else {
            (f64::from(stage.density) * f64::from(size).powi(2) / 1_000_000.).round_ties_even()
                as usize
        };
        let initial_seed = decor::stage_seed(seed, &stage.id);
        let regular_cells = if side > 0
            && stage.dithering == 0.
            && stage.filter_mode & 1 != 0
            && stage.filter.topology_any != -1
        {
            let cells = (0..side)
                .map(|i| ((i as f32 * stage.distance / size as f32) * n as f32) as usize)
                .collect::<Vec<_>>();
            if cells.last().is_some_and(|&last| last < n) { cells } else { Vec::new() }
        } else {
            Vec::new()
        };
        let skipped = if !stage.enabled {
            Some("Disabled in source".into())
        } else if stage.mode as u32 & realm_mask == 0 {
            Some("Source realm excludes the selected generation realm".into())
        } else if stage.prefabs.is_empty() {
            Some("Missing source prefab roster".into())
        } else {
            None
        };
        let done = skipped.is_some() || requested == 0;
        Ok(Self {
            stage,
            stage_index,
            size,
            hr,
            n,
            side,
            regular_cells,
            rng: initial_seed,
            report: StageReport {
                id: stage.id.clone(),
                name: stage.name.clone(),
                seed: initial_seed,
                requested_attempts: requested,
                attempted: 0,
                candidates: 0,
                passing_candidates: 0,
                rejected_filter: 0,
                rejected_bounds: 0,
                rejected_holes: 0,
                skipped,
                budget_exhausted: false,
                prefabs: stage.prefabs.clone(),
            },
            state: if done { State::Complete } else { State::Ready },
        })
    }
    pub fn report(&self) -> &StageReport {
        &self.report
    }
    pub fn finish(self) -> Result<StageReport, String> {
        if self.state != State::Complete {
            return Err("Live decoration stage did not complete".into());
        }
        Ok(self.report)
    }
    pub fn next(
        &mut self,
        view: View<'_>,
        alpha: &[u8],
        budget: &mut Budget,
        cancelled: impl Fn() -> bool,
    ) -> Result<Advance, String> {
        match self.state {
            State::Complete => return Ok(Advance::Complete),
            State::Cancelled => return Ok(Advance::Cancelled),
            State::Failed => {
                return Err("Live decoration stream cannot resume after failure".into());
            }
            State::Ready => {}
        }
        let result = self.advance(view, alpha, budget, &cancelled);
        match &result {
            Err(_) => self.state = State::Failed,
            Ok(Advance::Cancelled) => self.state = State::Cancelled,
            Ok(Advance::Complete) => self.state = State::Complete,
            _ => {}
        }
        result
    }
    fn advance(
        &mut self,
        view: View<'_>,
        alpha: &[u8],
        budget: &mut Budget,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Advance, String> {
        if view.size != self.size
            || view.height_resolution != self.hr
            || view.surface_resolution != self.n
            || view.height.len() != self.hr * self.hr
            || view
                .placement_blockers
                .is_some_and(|b| b.len() != self.n * self.n)
            || view.topology.len() != self.n * self.n
            || view.biome.len() != self.n * self.n * 5
            || view.splat.len() != self.n * self.n * 8
            || alpha.len() != self.n * self.n
        {
            return Err("Live decoration terrain view changed shape".into());
        }
        let tiles = view.topology_tiles.filter(|tiles| {
            !self.regular_cells.is_empty() && tiles.matches(view.topology, self.n)
        });
        let stage = self.stage;
        let cluster = stage.cluster.as_ref().filter(|_| stage.kind == Kind::ValueNoise);
        let check_filter = stage.filter_mode & 1 != 0 || cluster.is_some();
        let topology_only = cluster.is_none()
            && stage.filter.biome_type == -1
            && stage.filter.splat_type == -1;
        let size = self.size as f32;
        while self.report.attempted < self.report.requested_attempts {
            if cancelled() {
                return Ok(Advance::Cancelled);
            }
            if let Some(tiles) = tiles {
                let i = self.report.attempted;
                let x = i % self.side;
                let z = self.regular_cells[i / self.side];
                if let Some(end) = tiles.missing_any_until(
                    self.regular_cells[x], z, self.stage.filter.topology_any as u32,
                ) {
                    let candidates = &self.regular_cells[x..];
                    let count = candidates.partition_point(|&cell| cell < end)
                        .min(budget.maximum - budget.used);
                    if count != 0 {
                        let row = &alpha[z * self.n..][..self.n];
                        let holes = candidates[..count].iter().filter(|&&cell| row[cell] < 128).count();
                        self.report.rejected_holes += holes;
                        self.report.rejected_filter += count - holes;
                        self.report.attempted += count;
                        budget.used += count;
                        // X, Z, the probability roll and the prefab draw.
                        decor::advance_random(&mut self.rng, count as u64 * 4);
                        continue;
                    }
                }
            }
            if let Err(error) = budget.take() {
                self.report.budget_exhausted = true;
                return Err(error);
            }
            let i = self.report.attempted;
            self.report.attempted += 1;
            let (x, z) = if self.side > 0 {
                let x = (i % self.side) as f32 * stage.distance;
                let z = (i / self.side) as f32 * stage.distance;
                if stage.dithering == 0. {
                    decor::advance_random(&mut self.rng, 2);
                    (x, z)
                } else {
                    (
                        x + (decor::unit(&mut self.rng) * 2. - 1.) * stage.dithering,
                        z + (decor::unit(&mut self.rng) * 2. - 1.) * stage.dithering,
                    )
                }
            } else {
                (
                    decor::unit(&mut self.rng) * size,
                    decor::unit(&mut self.rng) * size,
                )
            };
            let u = x / size;
            let v = z / size;
            // Every attempt consumes the same four draws. Advance past the
            // probability and prefab draws when their values cannot matter.
            if !(0. ..1.).contains(&u) || !(0. ..1.).contains(&v) {
                decor::advance_random(&mut self.rng, 2);
                self.report.rejected_bounds += 1;
                continue;
            }
            let cell = (v * self.n as f32) as usize * self.n + (u * self.n as f32) as usize;
            if alpha[cell] < 128 {
                decor::advance_random(&mut self.rng, 2);
                self.report.rejected_holes += 1;
                continue;
            }
            if check_filter && topology_only {
                if view.placement_blocked_at(cell)
                    || !stage.filter.topology_matches(view.topology[cell] as u32)
                {
                    decor::advance_random(&mut self.rng, 2);
                    self.report.rejected_filter += 1;
                    continue;
                }
                // Passing topology gives a factor of one, so every roll passes.
                decor::advance_random(&mut self.rng, 1);
            } else if check_filter {
                let factor = if view.placement_blocked_at(cell) {
                    0.
                } else {
                    spawn::factor_view_at(view, &stage.filter, cell, u, v)
                };
                if factor == 0. {
                    decor::advance_random(&mut self.rng, 2);
                    self.report.rejected_filter += 1;
                    continue;
                }
                let weight = cluster.map_or(1., |cluster| {
                    decor::cluster(cluster, x, z, self.report.seed)
                }) * factor * factor;
                let roll = decor::unit(&mut self.rng);
                if weight <= 0. || roll > weight {
                    decor::advance_random(&mut self.rng, 1);
                    self.report.rejected_filter += 1;
                    continue;
                }
            } else {
                decor::advance_random(&mut self.rng, 1);
            }
            let prefab = (decor::random(&mut self.rng) % stage.prefabs.len() as u64) as usize;
            let height = spawn::sample_height_taps(view, u, v)?;
            self.report.passing_candidates += 1;
            self.report.candidates += 1;
            return Ok(Advance::Candidate(Candidate {
                stage: self.stage_index,
                prefab,
                position: [
                    x - size * 0.5,
                    height,
                    z - size * 0.5,
                ],
            }));
        }
        Ok(Advance::Complete)
    }
}
