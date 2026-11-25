//! Freeze exact per-map recipes before generation starts.
use crate::{MAX_SEED, Project, STAGES};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_BATCH: usize = 32;
pub const NO_SEED_VARIATION: &str = "Set a stage seed to Auto or choose one to vary.";
pub const DOMAINS: [&str; 7] = [
    "Terrain",
    "Biomes",
    "Rivers",
    "Roads & trails",
    "Monuments",
    "Scenery",
    "Rails",
];
const RAIL_SEEDS: [&str; 4] = [
    "/ring/seed",
    "/approaches/seed",
    "/bed/branching/seed",
    "/bed/siding/seed",
];

pub fn available_seed_domains(recipe: &Project) -> [bool; DOMAINS.len()] {
    [
        true,
        recipe.preview_engine != crate::LEGACY_PREVIEW_ENGINE,
        recipe.rivers.is_some(),
        recipe.roads.is_some(),
        recipe.performance.monuments,
        recipe.decoration.is_some() || recipe.objects.is_some(),
        recipe.rails.is_some(),
    ]
}

pub fn has_varying_seeds(
    recipe: &Project,
    random_master: bool,
    randomize: [bool; DOMAINS.len()],
) -> bool {
    available_seed_domains(recipe)
        .into_iter()
        .enumerate()
        .any(|(index, available)| {
            available && (randomize[index]
                || (random_master && recipe.seeds.get(index).is_some_and(|seed| seed.inherited)))
        })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchPlan {
    pub schema: u32,
    pub recipe: Project,
    pub count: usize,
    pub nonce: u64,
    #[serde(default)]
    pub random_master: bool,
    pub randomize: [bool; 7],
}
fn varied(seed: u32, nonce: u64, ordinal: usize, domain: &str) -> u32 {
    let mut hash = Sha256::new();
    hash.update(b"papyrus-batch-seeds-1\0");
    hash.update(seed.to_le_bytes());
    hash.update(nonce.to_le_bytes());
    hash.update((ordinal as u64).to_le_bytes());
    hash.update(domain.as_bytes());
    let result = u32::from_le_bytes(hash.finalize()[..4].try_into().unwrap()) % MAX_SEED + 1;
    if result == seed {
        result % MAX_SEED + 1
    } else {
        result
    }
}
impl BatchPlan {
    pub fn exact(recipe: Project) -> Self {
        Self {
            schema: 1,
            recipe,
            count: 1,
            nonce: 0,
            random_master: false,
            randomize: [false; 7],
        }
    }
    pub fn recipes(&self) -> Result<Vec<Project>, String> {
        self.recipe.validate()?;
        self.recipe.require_master_seed()?;
        if self.schema != 1 || !(1..=MAX_BATCH).contains(&self.count) {
            return Err(format!("Batch size must be 1–{MAX_BATCH}"));
        }
        if self.randomize[6] && self.recipe.rails.is_none() {
            return Err("Enable rails before randomizing rail seeds".into());
        }
        if self.count > 1 && !has_varying_seeds(&self.recipe, self.random_master, self.randomize) {
            return Err(NO_SEED_VARIATION.into());
        }
        let mut recipes = Vec::with_capacity(self.count);
        let mut used_masters = vec![self.recipe.master_seed];
        let mut seed_sets = std::collections::HashSet::with_capacity(self.count);
        let available = available_seed_domains(&self.recipe);
        for ordinal in 0..self.count {
            let mut recipe = self.recipe.clone();
            if self.random_master {
                let mut seed = varied(recipe.master_seed, self.nonce, ordinal, "Master");
                while used_masters.contains(&seed) {
                    seed = seed % MAX_SEED + 1;
                }
                used_masters.push(seed);
                recipe.master_seed = seed;
                recipe.refresh_inherited_seeds();
            }
            for (i, name) in STAGES.iter().enumerate() {
                if self.randomize[i] {
                    recipe.seeds[i].value =
                        varied(recipe.seeds[i].value, self.nonce, ordinal, name);
                    recipe.seeds[i].inherited = false;
                }
            }
            let mut rail_seeds = [0; RAIL_SEEDS.len()];
            if self.randomize[6]
                && let Some(rails) = &mut recipe.rails
            {
                for (path, varied_seed) in RAIL_SEEDS.into_iter().zip(&mut rail_seeds) {
                    let slot = rails
                        .settings
                        .pointer_mut(path)
                        .ok_or("Rail seed is missing from this recipe")?;
                    let seed = slot
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|s| (1..=MAX_SEED).contains(s))
                        .ok_or("Invalid rail seed")?;
                    *varied_seed = varied(seed, self.nonce, ordinal, path);
                    *slot = (*varied_seed).into();
                }
            }
            recipe.validate()?;
            // Compare the seeds that drive generation. Master and inheritance
            // are provenance once the stage values have been resolved.
            let seeds = std::array::from_fn::<_, { STAGES.len() }, _>(|i| {
                if available[i] { recipe.seeds[i].value } else { 0 }
            });
            if !seed_sets.insert((seeds, rail_seeds)) {
                return Err("Batch seed collision; generate a new batch".into());
            }
            recipes.push(recipe);
        }
        Ok(recipes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_request_preserves_the_entire_recipe() {
        let recipe = Project::default();
        assert_eq!(
            BatchPlan::exact(recipe.clone()).recipes().unwrap(),
            vec![recipe]
        );
    }
    #[test]
    fn monument_only_batch_preserves_every_other_setting_and_replays() {
        let recipe = Project::default();
        let plan = BatchPlan {
            count: 12,
            nonce: 42,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(recipe.clone())
        };
        let variants = plan.recipes().unwrap();
        let replay: BatchPlan =
            serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(variants, replay.recipes().unwrap());
        for mut variant in variants {
            assert_ne!(variant.seeds[4].value, recipe.seeds[4].value);
            assert!(!variant.seeds[4].inherited);
            variant.seeds[4] = recipe.seeds[4];
            assert_eq!(variant, recipe);
        }
    }
    #[test]
    fn empty_randomization_and_unbounded_batches_are_rejected() {
        let mut plan = BatchPlan::exact(Project::default());
        plan.count = 2;
        assert!(plan.recipes().is_err());
        plan.randomize[0] = true;
        plan.count = MAX_BATCH + 1;
        assert!(plan.recipes().is_err());
    }

    #[test]
    fn randomized_masters_refresh_inherited_seeds_and_preserve_locked_seeds() {
        let mut recipe = Project::default();
        recipe.seeds[3].inherited = false;
        let plan = BatchPlan {
            count: MAX_BATCH,
            nonce: 42,
            random_master: true,
            ..BatchPlan::exact(recipe.clone())
        };
        let variants = plan.recipes().unwrap();
        let mut masters = std::collections::BTreeSet::from([recipe.master_seed]);
        for variant in &variants {
            assert!(masters.insert(variant.master_seed));
            assert_eq!(variant.seeds[3], recipe.seeds[3]);
            let mut expected = recipe.clone();
            expected.master_seed = variant.master_seed;
            expected.refresh_inherited_seeds();
            assert_eq!(*variant, expected);
        }
        assert_eq!(variants, plan.recipes().unwrap());
    }

    #[test]
    fn batch_requires_an_enabled_seed_that_can_change_the_world() {
        let mut recipe = Project::default();
        for seed in &mut recipe.seeds {
            seed.inherited = false;
        }
        assert!(recipe.rivers.is_none());
        recipe.seeds[2].inherited = true;
        let mut plan = BatchPlan {
            count: 4,
            nonce: 42,
            random_master: true,
            ..BatchPlan::exact(recipe)
        };
        assert_eq!(plan.recipes().unwrap_err(), NO_SEED_VARIATION);
        plan.randomize[2] = true;
        assert_eq!(plan.recipes().unwrap_err(), NO_SEED_VARIATION);
        plan.randomize[0] = true;
        let variants = plan.recipes().unwrap();
        let terrain: std::collections::HashSet<_> =
            variants.iter().map(|recipe| recipe.seeds[0].value).collect();
        assert_eq!(terrain.len(), plan.count);
    }
}
