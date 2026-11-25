//! Bounded offshore candidate assembly, separate from land stamps.
//!
//! The inspected server's outer bands, repeated trials, best-score selection and
//! pre-decor 3D spacing are retained. RNG and execution budgets are independent,
//! versioned Mapper choices. Callers supply installed policies, ordered prefabs,
//! terrain sampling and complete decor/placement/environment validation. This
//! module alone cannot certify an oil rig or publish a playable map.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const VERSION: &str = "papyrus-offshore-search-1";

#[derive(Debug, Serialize)]
pub struct Scene {
    pub engine: String,
    pub source_sha256: String,
    pub monument_sha256: String,
    pub layout_fingerprint: [u8; 32],
    pub seed: u32,
    pub instances: Vec<Instance>,
    pub stages: Vec<serde_json::Value>,
}
#[derive(Debug, Serialize)]
pub struct Instance {
    pub id: u64,
    pub stage: i64,
    pub prefab: String,
    pub name: String,
    pub pose: Pose,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub minimum_world_size: u32,
    /// Zero means no count cap, as in the inspected Process implementation.
    pub target_count: usize,
    pub minimum_distance: f32,
    pub maximum_distance: f32,
    pub spacing: f32,
    /// Execution-profile controls, never inferred from prefab names.
    pub passes: usize,
    pub attempts_per_prefab: usize,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.minimum_world_size > 6000
            || self.target_count > 128
            || !self.minimum_distance.is_finite()
            || !self.maximum_distance.is_finite()
            || !(0. ..4096.).contains(&self.minimum_distance)
            || !(self.minimum_distance..=4096.).contains(&self.maximum_distance)
            || self.minimum_distance == self.maximum_distance
            || !self.spacing.is_finite()
            || !(0. ..=10000.).contains(&self.spacing)
            || !(1..=64).contains(&self.passes)
            || !(1..=10000).contains(&self.attempts_per_prefab)
        {
            return Err("Unsupported offshore search policy".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TerrainRect {
    pub size: u32,
    pub minimum: [f32; 2],
}
#[derive(Clone, Copy, Debug)]
pub struct Prefab {
    pub id: u64,
    /// Supplied by an installation's priority adapter. Input order is retained.
    pub score: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pose {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
impl Pose {
    fn valid(&self) -> bool {
        finite_position(self.position)
            && self.rotation.iter().all(|v| v.is_finite())
            && (0.999..=1.001).contains(&self.rotation.iter().map(|v| v * v).sum::<f32>())
            && self
                .scale
                .iter()
                .all(|v| v.is_finite() && (0.000001..=10000.).contains(&v.abs()))
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Placement {
    pub id: u64,
    /// Keep the winning decor transform, rather than a later trial's pose.
    pub pose: Pose,
}
pub enum Check {
    Accepted(Pose),
    Rejected,
    Cancelled,
}
#[derive(Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub seed: u32,
    pub final_seed: u32,
    pub attempts: usize,
    pub checks: usize,
    pub spacing_rejections: usize,
    pub completed_passes: usize,
    pub work: usize,
    pub best_score: u64,
}
#[derive(Debug)]
pub enum Outcome {
    Found {
        placements: Vec<Placement>,
        audit: Audit,
    },
    Skipped(Audit),
    Budget(Audit),
    Cancelled(Audit),
}
/// A callback failure is not an ordinary rejected site or an empty successful
/// stage. Typed callers can preserve missing support, unresolved coverage and
/// their own exhausted allowances without parsing a diagnostic string.
#[derive(Debug, PartialEq, Eq)]
pub enum SearchError<E> {
    Invalid(String),
    Callback(E),
}

struct Random(u32);
impl Random {
    fn value(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / 16_777_216.
    }
    fn range(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.value()
    }
}
fn candidate(rect: TerrainRect, policy: Policy, rng: &mut Random) -> [f32; 2] {
    let [x, z] = rect.minimum;
    let size = rect.size as f32;
    let lo = [x - policy.maximum_distance, z - policy.maximum_distance];
    let near_lo = [x - policy.minimum_distance, z - policy.minimum_distance];
    let near_hi = [
        x + size + policy.minimum_distance,
        z + size + policy.minimum_distance,
    ];
    let hi = [
        x + size + policy.maximum_distance,
        z + size + policy.maximum_distance,
    ];
    match rng.0 % 4 {
        0 => [rng.range(lo[0], near_lo[0]), rng.range(lo[1], hi[1])],
        1 => [rng.range(near_hi[0], hi[0]), rng.range(lo[1], hi[1])],
        // Preserve the source's asymmetric corner span: near_hi on X.
        2 => [rng.range(lo[0], near_hi[0]), rng.range(lo[1], near_lo[1])],
        _ => [rng.range(lo[0], near_hi[0]), rng.range(near_hi[1], hi[1])],
    }
}
fn finite_position(p: [f32; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= 1_000_000.)
}
fn too_close(a: [f32; 3], b: [f32; 3], radius: f32) -> bool {
    a.iter().zip(b).map(|(a, b)| (a - b) * (a - b)).sum::<f32>() < radius * radius
}

/// The evaluator is mandatory: a sampled sea-floor point is never implicitly
/// accepted. Cancellation/budget exhaustion discard every provisional pass.
#[allow(clippy::too_many_arguments)]
pub fn search(
    rect: TerrainRect,
    policy: Policy,
    seed: u32,
    prefabs: &[Prefab],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
    height: impl FnMut([f32; 2]) -> Result<f32, String>,
    evaluate: impl FnMut(&Prefab, [f32; 3]) -> Result<Check, String>,
) -> Result<Outcome, String> {
    try_search(
        rect,
        policy,
        seed,
        prefabs,
        maximum_work,
        cancelled,
        height,
        evaluate,
    )
    .map_err(|error| match error {
        SearchError::Invalid(s) | SearchError::Callback(s) => s,
    })
}

/// Like search, with the callback's failure type retained. Every provisional
/// trial and prior best group is discarded on error, cancellation or budget.
#[allow(clippy::too_many_arguments)]
pub fn try_search<E>(
    rect: TerrainRect,
    policy: Policy,
    seed: u32,
    prefabs: &[Prefab],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
    mut height: impl FnMut([f32; 2]) -> Result<f32, E>,
    mut evaluate: impl FnMut(&Prefab, [f32; 3]) -> Result<Check, E>,
) -> Result<Outcome, SearchError<E>> {
    policy.validate().map_err(SearchError::Invalid)?;
    if !(1000..=6000).contains(&rect.size)
        || seed == 0
        || seed > i32::MAX as u32
        || rect
            .minimum
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 100_000.)
        || prefabs.len() > 128
        || !(1..=64_000_000).contains(&maximum_work)
        || prefabs
            .iter()
            .any(|p| p.id == 0 || !(1..=1_u64 << 48).contains(&p.score))
        || prefabs.iter().map(|p| p.id).collect::<BTreeSet<_>>().len() != prefabs.len()
    {
        return Err(SearchError::Invalid(
            "Invalid offshore search inputs".into(),
        ));
    }
    let mut rng = Random(seed);
    let mut audit = Audit {
        engine: VERSION,
        seed,
        final_seed: seed,
        attempts: 0,
        checks: 0,
        spacing_rejections: 0,
        completed_passes: 0,
        work: 0,
        best_score: 0,
    };
    if cancelled() {
        return Ok(Outcome::Cancelled(audit));
    }
    if rect.size < policy.minimum_world_size || prefabs.is_empty() {
        return Ok(Outcome::Skipped(audit));
    }
    let mut best = Vec::new();
    for _ in 0..policy.passes {
        let mut placements: Vec<Placement> = Vec::new();
        let mut score = 0;
        for prefab in prefabs {
            for _ in 0..policy.attempts_per_prefab {
                if cancelled() {
                    audit.final_seed = rng.0;
                    return Ok(Outcome::Cancelled(audit));
                }
                if audit.work == maximum_work {
                    audit.final_seed = rng.0;
                    return Ok(Outcome::Budget(audit));
                }
                audit.work += 1;
                audit.attempts += 1;
                let point = candidate(rect, policy, &mut rng);
                let position = [
                    point[0],
                    height(point).map_err(SearchError::Callback)?,
                    point[1],
                ];
                if cancelled() {
                    audit.final_seed = rng.0;
                    return Ok(Outcome::Cancelled(audit));
                }
                if !finite_position(position) {
                    return Err(SearchError::Invalid(
                        "Nonfinite or unbounded offshore terrain height".into(),
                    ));
                }
                let mut blocked = false;
                for placed in &placements {
                    if cancelled() {
                        audit.final_seed = rng.0;
                        return Ok(Outcome::Cancelled(audit));
                    }
                    if audit.work == maximum_work {
                        audit.final_seed = rng.0;
                        return Ok(Outcome::Budget(audit));
                    }
                    audit.work += 1;
                    if too_close(placed.pose.position, position, policy.spacing) {
                        blocked = true;
                        break;
                    }
                }
                if blocked {
                    audit.spacing_rejections += 1;
                    continue;
                }
                if audit.work == maximum_work {
                    audit.final_seed = rng.0;
                    return Ok(Outcome::Budget(audit));
                }
                audit.work += 1;
                audit.checks += 1;
                let checked = evaluate(prefab, position).map_err(SearchError::Callback)?;
                if cancelled() {
                    audit.final_seed = rng.0;
                    return Ok(Outcome::Cancelled(audit));
                }
                match checked {
                    Check::Rejected => continue,
                    Check::Cancelled => {
                        audit.final_seed = rng.0;
                        return Ok(Outcome::Cancelled(audit));
                    }
                    Check::Accepted(pose) => {
                        if !pose.valid() {
                            return Err(SearchError::Invalid("Invalid offshore decor pose".into()));
                        }
                        placements.push(Placement {
                            id: prefab.id,
                            pose,
                        });
                        score += prefab.score;
                        break;
                    }
                }
            }
            if policy.target_count > 0 && placements.len() >= policy.target_count {
                break;
            }
        }
        audit.completed_passes += 1;
        // Strict comparison retains the first equal-scoring pass.
        if score > audit.best_score {
            audit.best_score = score;
            best = placements;
        }
    }
    audit.final_seed = rng.0;
    if cancelled() {
        return Ok(Outcome::Cancelled(audit));
    }
    Ok(Outcome::Found {
        placements: best,
        audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_callback_failure_discards_an_earlier_winning_group() {
        #[derive(Debug, PartialEq, Eq)]
        enum Missing {
            Environment,
        }
        let mut policy = policy();
        policy.passes = 2;
        policy.target_count = 1;
        let mut calls = 0;
        let result = try_search(
            RECT,
            policy,
            7,
            &PREFABS,
            100,
            || false,
            |_| Ok(-40.),
            |_, point| {
                calls += 1;
                if calls == 2 {
                    Err(Missing::Environment)
                } else {
                    Ok(Check::Accepted(pose(point)))
                }
            },
        );
        assert_eq!(
            result.unwrap_err(),
            SearchError::Callback(Missing::Environment)
        );
        assert_eq!(calls, 2);
    }
    #[test]
    fn callback_cancellation_stops_before_later_validation_or_checks() {
        use std::cell::Cell;
        let stopped = Cell::new(false);
        let result = search(
            RECT,
            policy(),
            7,
            &PREFABS,
            100,
            || stopped.get(),
            |_| {
                stopped.set(true);
                Ok(-40.)
            },
            |_, _| panic!("cancelled height query evaluated a site"),
        )
        .unwrap();
        let Outcome::Cancelled(audit) = result else {
            panic!()
        };
        assert_eq!(audit.checks, 0);
        stopped.set(false);
        let result = search(
            RECT,
            policy(),
            7,
            &PREFABS,
            100,
            || stopped.get(),
            |_| Ok(-40.),
            |_, _| {
                stopped.set(true);
                Ok(Check::Accepted(pose([f32::NAN; 3])))
            },
        )
        .unwrap();
        assert!(matches!(result, Outcome::Cancelled(_)));
    }
    #[test]
    fn typed_height_failure_is_preserved_and_invalid_input_never_calls_it() {
        let result = try_search(
            RECT,
            policy(),
            7,
            &PREFABS,
            100,
            || false,
            |_| Err(17),
            |_, _| panic!(),
        );
        assert_eq!(result.unwrap_err(), SearchError::Callback(17));
        let result = try_search(
            RECT,
            policy(),
            0,
            &PREFABS,
            100,
            || false,
            |_| Ok::<_, i32>(0.),
            |_, _| panic!(),
        );
        assert!(matches!(result, Err(SearchError::Invalid(_))));
    }
    #[test]
    fn spacing_matches_owned_server_helper_capture() {
        #[derive(Deserialize)]
        struct Row {
            point: [f32; 3],
            placed: Vec<[f32; 3]>,
            radius: f32,
            overlaps: bool,
        }
        let document: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/offshore-spacing-reference.json"
        ))
        .unwrap();
        assert_eq!(
            document["format"],
            "papyrus-owned-offshore-spacing-reference"
        );
        assert_eq!(document["gameProcessStarted"], false);
        assert_eq!(document["nativeLibraryLoaded"], false);
        let rows: Vec<Row> = serde_json::from_value(document["rows"].clone()).unwrap();
        assert_eq!(rows.len(), 8);
        for row in rows {
            assert_eq!(
                row.placed
                    .iter()
                    .any(|p| too_close(*p, row.point, row.radius)),
                row.overlaps
            );
        }
    }
    #[test]
    fn uncapped_stage_keeps_all_prefabs_and_winner_keeps_full_decor_pose() {
        let mut p = policy();
        p.passes = 1;
        p.target_count = 0;
        p.spacing = 0.;
        let Outcome::Found { placements, .. } = search(
            RECT,
            p,
            7,
            &PREFABS,
            100,
            || false,
            |_| Ok(-40.),
            |_, position| Ok(Check::Accepted(pose(position))),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(placements.len(), 2);
        p.target_count = 1;
        p.passes = 2;
        let mut first = None;
        let mut calls = 0;
        let Outcome::Found { placements, .. } = search(
            RECT,
            p,
            7,
            &PREFABS,
            100,
            || false,
            |_| Ok(-40.),
            |_, position| {
                calls += 1;
                let mut pose = pose(position);
                pose.scale = [calls as f32; 3];
                first.get_or_insert_with(|| pose.clone());
                Ok(Check::Accepted(pose))
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(placements[0].pose, first.unwrap());
        assert_eq!(calls, 2);
    }
    fn policy() -> Policy {
        Policy {
            minimum_world_size: 1000,
            target_count: 2,
            minimum_distance: 100.,
            maximum_distance: 500.,
            spacing: 500.,
            passes: 10,
            attempts_per_prefab: 100,
        }
    }
    const RECT: TerrainRect = TerrainRect {
        size: 4000,
        minimum: [-2000.; 2],
    };
    const PREFABS: [Prefab; 2] = [Prefab { id: 1, score: 16 }, Prefab { id: 2, score: 81 }];
    fn pose(position: [f32; 3]) -> Pose {
        Pose {
            position,
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    #[test]
    fn outer_bands_preserve_source_corner_spans_and_replay() {
        let run = || {
            search(
                RECT,
                policy(),
                24680,
                &PREFABS,
                100_000,
                || false,
                |_| Ok(-40.),
                |_, p| {
                    assert!(p[0] <= -2100. || p[0] >= 2100. || p[2] <= -2100. || p[2] >= 2100.);
                    assert!(p[0].abs() <= 2500. && p[2].abs() <= 2500.);
                    assert_eq!(p[1], -40.);
                    Ok(Check::Accepted(pose([p[0], 0., p[2]])))
                },
            )
            .unwrap()
        };
        let Outcome::Found { placements, audit } = run() else {
            panic!()
        };
        let Outcome::Found {
            placements: again,
            audit: other,
        } = run()
        else {
            panic!()
        };
        assert_eq!(placements, again);
        assert_eq!(audit.final_seed, other.final_seed);
        assert_eq!(placements.len(), 2);
        assert_eq!(audit.completed_passes, 10);
        assert_eq!(audit.best_score, 97);
    }
    #[test]
    fn full_3d_pre_decor_spacing_and_strict_best_score_match_control_flow() {
        let mut p = policy();
        p.passes = 1;
        p.spacing = 10000.;
        p.attempts_per_prefab = 1;
        let mut call = 0;
        let Outcome::Found { placements, .. } = search(
            RECT,
            p,
            7,
            &PREFABS,
            100,
            || false,
            |_| Ok(-40.),
            |_, mut position| {
                call += 1;
                if call == 1 {
                    position[1] = 20000.;
                }
                Ok(Check::Accepted(pose(position)))
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(placements.len(), 2);
        let Outcome::Found { placements, .. } = search(
            RECT,
            p,
            7,
            &PREFABS,
            100,
            || false,
            |_| Ok(-40.),
            |_, position| Ok(Check::Accepted(pose(position))),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(placements.len(), 1); // same search, rejected by full 3D spacing without decor lift
        p.target_count = 1;
        p.passes = 3;
        let Outcome::Found { placements, audit } = search(
            RECT,
            p,
            7,
            &PREFABS,
            100,
            || false,
            |_| Ok(-40.),
            |_, position| Ok(Check::Accepted(pose(position))),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].id, 1); // source uses input order and quota, not highest score first
        assert_eq!(audit.best_score, 16);
    }
    #[test]
    fn cancellation_and_budget_never_return_provisional_placements() {
        let mut p = policy();
        p.passes = 2;
        p.target_count = 1;
        assert!(matches!(
            search(
                RECT,
                p,
                7,
                &PREFABS,
                2,
                || false,
                |_| Ok(-40.),
                |_, p| Ok(Check::Accepted(pose(p)))
            )
            .unwrap(),
            Outcome::Budget(_)
        ));
        assert!(matches!(
            search(
                RECT,
                p,
                7,
                &PREFABS,
                100,
                || false,
                |_| Ok(-40.),
                |_, _| Ok(Check::Cancelled)
            )
            .unwrap(),
            Outcome::Cancelled(_)
        ));
        p.minimum_world_size = 5000;
        assert!(matches!(
            search(
                RECT,
                p,
                7,
                &PREFABS,
                100,
                || false,
                |_| panic!(),
                |_, _| panic!()
            )
            .unwrap(),
            Outcome::Skipped(_)
        ));
    }
    #[test]
    fn unknown_or_invalid_policies_and_nonfinite_callbacks_fail() {
        let mut p = policy();
        p.minimum_distance = p.maximum_distance;
        assert!(p.validate().is_err());
        assert!(
            search(
                RECT,
                policy(),
                7,
                &PREFABS,
                100,
                || false,
                |_| Ok(f32::NAN),
                |_, _| panic!()
            )
            .is_err()
        );
        assert!(
            search(
                RECT,
                policy(),
                7,
                &PREFABS,
                100,
                || false,
                |_| Ok(0.),
                |_, _| Ok(Check::Accepted(pose([f32::INFINITY; 3])))
            )
            .is_err()
        );
    }
}
