//! Viewer-facing seed provenance. Displaying zero never destroys the generation seed.
use crate::Project;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditedStage {
    Terrain,
    Biomes,
    Rivers,
    Roads,
    Trails,
    Monuments,
    Scenery,
    Rails,
    Topology,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedStatus {
    Seeded,
    Edited,
    Derived,
    Disabled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    pub stage: String,
    /// Zero means no standalone seed describes this result; consult status.
    pub seed: u32,
    pub generation_seed: Option<u32>,
    pub status: SeedStatus,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub format: String,
    pub schema: u32,
    pub generator_version: String,
    pub edited_stages: Vec<EditedStage>,
    pub seeds: Vec<Seed>,
}
impl Provenance {
    pub fn for_recipe(recipe: &Project, edited: &[EditedStage]) -> Self {
        use EditedStage::*;
        let mut edits = edited.to_vec();
        if recipe
            .topology_edits
            .as_ref()
            .is_some_and(|e| e.enabled && !e.strokes.is_empty())
        {
            edits.push(Topology);
        }
        edits.sort();
        edits.dedup();
        let mut seeds = Vec::new();
        let mut add = |name: &str, domain, original: Option<u32>, enabled: bool| {
            let status = if !enabled {
                SeedStatus::Disabled
            } else if edits.contains(&domain) {
                SeedStatus::Edited
            } else if original.is_some() {
                SeedStatus::Seeded
            } else {
                SeedStatus::Derived
            };
            seeds.push(Seed {
                stage: name.into(),
                seed: if status == SeedStatus::Seeded {
                    original.unwrap_or(0)
                } else {
                    0
                },
                generation_seed: original,
                status,
            });
        };
        for (i, (name, domain, enabled)) in [
            ("terrain", Terrain, true),
            (
                "biomes",
                Biomes,
                recipe.preview_engine != crate::LEGACY_PREVIEW_ENGINE,
            ),
            ("rivers", Rivers, recipe.rivers.is_some()),
            ("roads", Roads, recipe.roads.is_some()),
            ("monuments", Monuments, recipe.performance.monuments),
            (
                "scenery",
                Scenery,
                recipe.decoration.is_some() || recipe.objects.is_some(),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            add(name, domain, Some(recipe.seeds[i].value), enabled);
        }
        // Trails use the road seed plus the installation's family domain, not a new independent seed.
        add(
            "trails",
            Trails,
            None,
            recipe
                .roads
                .as_ref()
                .is_some_and(|r| r.include_trails != Some(false)),
        );
        for (name, path) in [
            ("rail_ring", "/ring/seed"),
            ("rail_approaches", "/approaches/seed"),
            ("rail_branches", "/bed/branching/seed"),
            ("rail_sidings", "/bed/siding/seed"),
        ] {
            let original = recipe
                .rails
                .as_ref()
                .and_then(|r| r.settings.pointer(path))
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok());
            add(name, Rails, original, recipe.rails.is_some());
        }
        add("topology", Topology, None, true);
        seeds.push(Seed {
            stage: "master".into(),
            seed: recipe.master_seed,
            generation_seed: Some(recipe.master_seed),
            status: SeedStatus::Seeded,
        });
        Self {
            format: "papyrus-generation-provenance".into(),
            schema: 1,
            generator_version: env!("CARGO_PKG_VERSION").into(),
            edited_stages: edits,
            seeds,
        }
    }
    pub fn validate(&self, recipe: &Project) -> Result<(), String> {
        let mut expected = Self::for_recipe(recipe, &self.edited_stages);
        expected.generator_version = self.generator_version.clone();
        if self.generator_version.is_empty()
            || self.generator_version.len() > 64
            || self.generator_version.chars().any(char::is_control)
            || self != &expected
        {
            return Err("Seed provenance differs from its recipe or edit declarations".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edited_seed_is_zero_but_original_is_retained_and_other_stages_are_unchanged() {
        let recipe = Project::default();
        let value = Provenance::for_recipe(&recipe, &[EditedStage::Monuments]);
        let monument = value.seeds.iter().find(|s| s.stage == "monuments").unwrap();
        assert_eq!(monument.seed, 0);
        assert_eq!(monument.generation_seed, Some(recipe.seeds[4].value));
        assert_eq!(monument.status, SeedStatus::Edited);
        assert_eq!(value.seeds[0].seed, recipe.seeds[0].value);
        value.validate(&recipe).unwrap();
        let mut changed = value;
        changed.seeds[0].seed = 0;
        assert!(changed.validate(&recipe).is_err());
    }
}
