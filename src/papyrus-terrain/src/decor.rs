//! Deterministic static-prefab candidate generation, independent of runtime populations.
//! Candidates still require authored anchor/check/terrain-effect and collision passes.
use crate::{
    native_layers::{Layers, dimensions},
    spawn::{self, Filter},
};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-decor-candidates-1";
const MAX_ATTEMPTS: usize = 8_000_000;
const MAX_STAGE_ATTEMPTS: usize = 262_144;
const MAX_INSTANCES: usize = 100_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Uniform,
    WhiteNoise,
    ValueNoise,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct Cluster {
    pub octaves: u32,
    pub frequency: f32,
    pub amplitude: f32,
    pub offset: f32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub id: String,
    pub order: usize,
    pub name: String,
    pub kind: Kind,
    pub folder: String,
    pub enabled: bool,
    pub mode: i32,
    pub filter: Filter,
    pub filter_mode: i32,
    pub anchor_mode: i32,
    pub distance: f32,
    pub dithering: f32,
    pub density: f32,
    pub cluster: Option<Cluster>,
    pub prefabs: Vec<String>,
}
fn path(p: &str) -> bool {
    (p.starts_with("assets/bundled/prefabs/autospawn/decor/")
        || p.starts_with("assets/bundled/prefabs/autospawn/resource/"))
        && p.ends_with(".prefab")
        && p.len() <= 512
        && !p.contains(['\\', '\0'])
        && !p.split('/').any(|s| s.is_empty() || s == ".." || s == ".")
}
impl Stage {
    pub fn validate(&self) -> Result<(), String> {
        self.validate_scope(false)
    }
    pub fn validate_path_attached(&self) -> Result<(), String> {
        self.validate_scope(true)
    }
    fn validate_scope(&self, path_attached: bool) -> Result<(), String> {
        self.filter.validate()?;
        let distance_range = if path_attached { 0.001..=100000. } else { 1. ..=1000. };
        let maximum_dithering = if path_attached { 10000. } else { 1000. };
        if self.id.is_empty()
            || self.id.len() > 128
            || self.name.len() > 256
            || self.order >= 128
            || (!path_attached
                && !self.folder.starts_with("decor/")
                && !self.folder.starts_with("resource/"))
            || self.folder.len() > 256
            || self.folder.contains(['\\', '\0'])
            || self
                .folder
                .split('/')
                .any(|s| s.is_empty() || s == ".." || s == ".")
            || self.prefabs.len() > 4096
            || self.prefabs.iter().any(|p| {
                !(if path_attached {
                    p.starts_with("assets/bundled/prefabs/autospawn/")
                        && p.ends_with(".prefab")
                        && p.len() <= 512
                        && !p.contains(['\\', '\0'])
                        && p.split('/').all(|s| !matches!(s, "" | "." | ".."))
                } else {
                    path(p)
                }) || !p.starts_with(&format!(
                    "assets/bundled/prefabs/autospawn/{}/",
                    self.folder
                ))
            })
            || self.prefabs.windows(2).any(|w| w[0] >= w[1])
            || !self.distance.is_finite()
            || !self.dithering.is_finite()
            || !self.density.is_finite()
            || !(0. ..=maximum_dithering).contains(&self.dithering)
            || !(0. ..=1_000_000.).contains(&self.density)
            || (self.kind == Kind::Uniform && !distance_range.contains(&self.distance))
            || (self.kind == Kind::ValueNoise && self.cluster.is_none())
            || self.cluster.as_ref().is_some_and(|c| {
                !(1..=12).contains(&c.octaves)
                    || !c.frequency.is_finite()
                    || !(0. ..=1000.).contains(&c.frequency)
                    || !c.amplitude.is_finite()
                    || !(0. ..=10.).contains(&c.amplitude)
                    || !c.offset.is_finite()
                    || !(-10. ..=10.).contains(&c.offset)
            })
        {
            return Err("Invalid static decoration stage".into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct Candidate {
    pub stage: usize,
    pub prefab: usize,
    pub position: [f32; 3],
}
#[derive(Debug, Serialize)]
pub struct StageReport {
    pub id: String,
    pub name: String,
    pub seed: u64,
    pub requested_attempts: usize,
    pub attempted: usize,
    pub candidates: usize,
    pub passing_candidates: usize,
    pub rejected_filter: usize,
    pub rejected_bounds: usize,
    pub rejected_holes: usize,
    pub skipped: Option<String>,
    pub budget_exhausted: bool,
    pub prefabs: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub source_sha256: String,
    pub seed: u32,
    pub candidates: Vec<Candidate>,
    pub stages: Vec<StageReport>,
    pub validated_instances: usize,
    pub budget_exhausted: bool,
    pub source_issues: Vec<String>,
    pub pending_checks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fitting: Option<crate::decor_fit::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terrain_placement: Option<crate::footprints::Report>,
}
// SplitMix64 with an explicit name-based stage seed: roster order cannot perturb another stage.
pub(crate) fn advance_random(state: &mut u64, draws: u64) {
    *state = state.wrapping_add(0x9e3779b97f4a7c15_u64.wrapping_mul(draws));
}

pub(crate) fn random(state: &mut u64) -> u64 {
    advance_random(state, 1);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
pub(crate) fn unit(state: &mut u64) -> f32 {
    (random(state) >> 40) as f32 / 16_777_216.
}
pub(crate) fn stage_seed(seed: u32, id: &str) -> u64 {
    b"papyrus-decor-candidates-1"
        .iter()
        .copied()
        .chain(seed.to_le_bytes())
        .chain(id.bytes())
        .fold(0xcbf29ce484222325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        })
}
pub(crate) fn cluster(c: &Cluster, x: f32, z: f32, seed: u64) -> f32 {
    let mut frequency = f64::from(c.frequency) * 0.01;
    let mut amplitude = f64::from(c.amplitude);
    let mut value = f64::from(c.offset);
    for octave in 0..c.octaves {
        value += amplitude
            * crate::noise(
                f64::from(x) * frequency,
                f64::from(z) * frequency,
                seed.wrapping_add(u64::from(octave)) as i32,
            )
            .abs();
        frequency *= 2.;
        amplitude *= 0.5;
    }
    value.clamp(0., 1.) as f32
}
pub fn generate(
    layers: &Layers,
    alpha: &[u8],
    stages: &[Stage],
    seed: u32,
    source_sha256: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let (hr, n) = dimensions(layers.size)?;
    let cells = n * n;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || layers.height.len() != hr * hr
        || layers.biome.len() != cells * 5
        || layers.splat.len() != cells * 8
        || layers.topology.len() != cells
        || alpha.len() != cells
        || stages.len() > 128
        || layers.height.iter().any(|h| !(0..=32766).contains(h))
    {
        return Err("Invalid static decoration input".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for stage in stages {
        stage.validate()?;
        if !ids.insert(&stage.id) {
            return Err("Duplicate static decoration stage".into());
        }
    }
    let mut out = Report {
        engine: VERSION,
        source_sha256: source_sha256.into(),
        seed,
        candidates: Vec::new(),
        stages: Vec::new(),
        validated_instances: 0,
        fitting: None,
        terrain_placement: None,
        budget_exhausted: false,
        source_issues: Vec::new(),
        pending_checks: vec![
            "Authored decor transforms and terrain filters at child points".into(),
            "Terrain anchors, water and environment-volume checks".into(),
            "Placement occupancy, terrain effects and collision validation".into(),
        ],
    };
    let mut remaining = MAX_ATTEMPTS;
    for (stage_index, stage) in stages.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let initial_seed = stage_seed(seed, &stage.id);
        let mut retention_rng = initial_seed ^ 0xd01b_deca_fbad_1234;
        let capacity = (MAX_INSTANCES / stages.len().max(1)).min(4096);
        let start = out.candidates.len();
        let mut rng = initial_seed;
        let side = if stage.kind == Kind::Uniform {
            (layers.size as f64 / f64::from(stage.distance)).ceil() as usize
        } else {
            0
        };
        let requested = if side > 0 {
            side * side
        } else {
            (f64::from(stage.density) * f64::from(layers.size).powi(2) / 1_000_000.)
                .round_ties_even() as usize
        };
        let mut stats = StageReport {
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
            skipped: None,
            budget_exhausted: false,
            prefabs: stage.prefabs.clone(),
        };
        if !stage.enabled {
            stats.skipped = Some("Disabled in source".into());
        } else if stage.mode != -1 {
            stats.skipped = Some("Source run-mode requires a dedicated adapter".into());
        } else if stage.prefabs.is_empty() {
            stats.skipped = Some("Missing source prefab roster".into());
        }
        if stats.skipped.is_some() {
            out.stages.push(stats);
            continue;
        }
        let limit = requested.min(MAX_STAGE_ATTEMPTS);
        if requested > limit {
            stats.budget_exhausted = true;
            out.budget_exhausted = true;
        }
        for sample in 0..limit {
            // Thin the entire uniform grid, rather than visiting only one corner.
            let i = if side > 0 {
                sample * requested / limit
            } else {
                sample
            };
            if sample % 4096 == 0 && cancelled() {
                return Ok(None);
            }
            if remaining == 0 {
                stats.budget_exhausted = true;
                out.budget_exhausted = true;
                break;
            }
            remaining -= 1;
            stats.attempted += 1;
            let (x, z) = if side > 0 {
                (
                    (i % side) as f32 * stage.distance
                        + (unit(&mut rng) * 2. - 1.) * stage.dithering,
                    (i / side) as f32 * stage.distance
                        + (unit(&mut rng) * 2. - 1.) * stage.dithering,
                )
            } else {
                (
                    unit(&mut rng) * layers.size as f32,
                    unit(&mut rng) * layers.size as f32,
                )
            };
            let u = x / layers.size as f32;
            let v = z / layers.size as f32;
            let roll = unit(&mut rng);
            let prefab = (random(&mut rng) % stage.prefabs.len() as u64) as usize;
            if !(0. ..1.).contains(&u) || !(0. ..1.).contains(&v) {
                stats.rejected_bounds += 1;
                continue;
            }
            let cell = (v * n as f32) as usize * n + (u * n as f32) as usize;
            if alpha[cell] < 128 {
                stats.rejected_holes += 1;
                continue;
            }
            let f = spawn::factor(layers, &stage.filter, u, v);
            let weight = if stage.kind == Kind::ValueNoise {
                cluster(stage.cluster.as_ref().unwrap(), x, z, initial_seed) * f * f
            } else if stage.filter_mode & 1 != 0 {
                f * f
            } else {
                1.
            };
            if weight <= 0. || roll > weight {
                stats.rejected_filter += 1;
                continue;
            }
            let candidate = Candidate {
                stage: stage_index,
                prefab,
                position: [
                    x - layers.size as f32 * 0.5,
                    spawn::height(layers, u, v),
                    z - layers.size as f32 * 0.5,
                ],
            };
            stats.passing_candidates += 1;
            if stats.candidates < capacity {
                out.candidates.push(candidate);
                stats.candidates += 1;
            } else {
                // Bounded reservoir retains coverage across the entire sampled stage.
                let slot = (random(&mut retention_rng) % stats.passing_candidates as u64) as usize;
                if slot < capacity {
                    out.candidates[start + slot] = candidate;
                }
                stats.budget_exhausted = true;
                out.budget_exhausted = true;
            }
        }
        out.stages.push(stats);
    }
    Ok((!cancelled()).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let cells = n * n;
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            biome: vec![51; cells * 5],
            splat: vec![32; cells * 8],
            topology: vec![1; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn stage(id: &str) -> Stage {
        Stage {
            id: id.into(),
            order: 0,
            name: "Rocks".into(),
            kind: Kind::WhiteNoise,
            folder: "decor/rocks".into(),
            enabled: true,
            mode: -1,
            filter: Filter {
                splat_type: -1,
                biome_type: -1,
                topology_any: 1,
                topology_all: 0,
                topology_not: 0,
            },
            filter_mode: 1,
            anchor_mode: 0,
            distance: 10.,
            dithering: 0.,
            density: 100.,
            cluster: None,
            prefabs: vec![
                "assets/bundled/prefabs/autospawn/decor/rocks/a.prefab".into(),
                "assets/bundled/prefabs/autospawn/decor/rocks/b.prefab".into(),
            ],
        }
    }
    #[test]
    fn world_uniform_resources_keep_prefabs_within_the_authored_family() {
        let mut stage = stage("vines");
        stage.kind = Kind::Uniform;
        stage.folder = "resource/vine_swinging".into();
        stage.prefabs = vec![
            "assets/bundled/prefabs/autospawn/resource/vine_swinging/tree.prefab".into(),
        ];
        stage.validate().unwrap();
        stage.prefabs[0] = "assets/bundled/prefabs/autospawn/resource/elsewhere/tree.prefab".into();
        assert!(stage.validate().is_err());
        stage.prefabs[0] = "assets/bundled/prefabs/autospawn/resource/vine_swinging/../tree.prefab".into();
        assert!(stage.validate().is_err());
        stage.folder = "monument/vines".into();
        stage.prefabs[0] = "assets/bundled/prefabs/autospawn/monument/vines/tree.prefab".into();
        assert!(stage.validate().is_err());
    }
    #[test]
    fn same_seed_is_reproducible_and_stage_streams_are_independent() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let stages = [stage("one"), stage("two")];
        let run = |seed| {
            generate(&l, &alpha, &stages, seed, "source", || false)
                .unwrap()
                .unwrap()
        };
        let first = run(123);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(run(123)).unwrap()
        );
        assert_ne!(
            first.candidates[0].position,
            run(124).candidates[0].position
        );
        let alone = generate(&l, &alpha, &stages[1..], 123, "source", || false)
            .unwrap()
            .unwrap();
        let second: Vec<_> = first
            .candidates
            .iter()
            .filter(|c| c.stage == 1)
            .map(|c| c.position)
            .collect();
        assert_eq!(
            second,
            alone
                .candidates
                .iter()
                .map(|c| c.position)
                .collect::<Vec<_>>()
        );
        assert_eq!(first.validated_instances, 0);
        assert!(!first.pending_checks.is_empty());
    }
    #[test]
    fn holes_topology_and_disabled_modes_are_respected_without_mutation() {
        let l = layers();
        let n = l.surface_resolution;
        let before = l.topology.clone();
        let mut s = stage("one");
        s.filter.topology_not = 1;
        let result = generate(&l, &vec![255; n * n], &[s], 1, "source", || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.stages[0].rejected_filter, 100);
        assert!(result.candidates.is_empty());
        let result = generate(&l, &vec![0; n * n], &[stage("one")], 1, "source", || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.stages[0].rejected_holes, 100);
        let mut s = stage("one");
        s.mode = 1;
        let result = generate(&l, &vec![255; n * n], &[s], 1, "source", || false)
            .unwrap()
            .unwrap();
        assert!(result.stages[0].skipped.is_some());
        assert_eq!(result.stages[0].attempted, 0);
        assert_eq!(l.topology, before);
    }
    #[test]
    fn uniform_budget_samples_the_full_map_and_retention_is_bounded() {
        let l = layers();
        let mut s = stage("uniform");
        s.kind = Kind::Uniform;
        s.distance = 1.;
        let result = generate(
            &l,
            &vec![255; l.surface_resolution.pow(2)],
            &[s],
            5,
            "source",
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.stages[0].requested_attempts, 1_000_000);
        assert_eq!(result.stages[0].attempted, MAX_STAGE_ATTEMPTS);
        assert_eq!(result.candidates.len(), 4096);
        assert_eq!(result.stages[0].passing_candidates, MAX_STAGE_ATTEMPTS);
        assert!(result.budget_exhausted);
        assert!(result.candidates.iter().any(|c| c.position[2] > 450.));
        assert!(result.candidates.iter().any(|c| c.position[2] < -450.));
    }
    #[test]
    fn source_noise_parameters_are_applied_and_unsupported_data_fails_early() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let mut s = stage("noise");
        s.kind = Kind::ValueNoise;
        assert!(generate(&l, &alpha, &[s.clone()], 1, "source", || false).is_err());
        s.cluster = Some(Cluster {
            octaves: 2,
            frequency: 1.,
            amplitude: 0.,
            offset: 0.,
        });
        assert!(
            generate(&l, &alpha, &[s.clone()], 1, "source", || false)
                .unwrap()
                .unwrap()
                .candidates
                .is_empty()
        );
        s.cluster.as_mut().unwrap().offset = 1.;
        assert_eq!(
            generate(&l, &alpha, &[s.clone()], 1, "source", || false)
                .unwrap()
                .unwrap()
                .candidates
                .len(),
            100
        );
        s.prefabs[0] = "../escape.prefab".into();
        assert!(s.validate().is_err());
    }
    #[test]
    fn cancellation_and_duplicate_stages_publish_no_partial_report() {
        let l = layers();
        let alpha = vec![255; l.surface_resolution.pow(2)];
        let s = stage("one");
        assert!(
            generate(&l, &alpha, std::slice::from_ref(&s), 1, "source", || true)
                .unwrap()
                .is_none()
        );
        assert!(generate(&l, &alpha, &[s.clone(), s], 1, "source", || false).is_err());
        assert!(generate(&l, &alpha[..1], &[stage("one")], 1, "source", || false).is_err());
    }
}
