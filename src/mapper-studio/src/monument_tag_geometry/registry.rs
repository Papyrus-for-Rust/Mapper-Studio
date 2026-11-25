//! Deterministic registration of authored volumes at accepted monument poses.
use super::{AUTHORED_VERSION, Prepared};
use papyrus_terrain::{
    environment::{Box3, Coverage, Key, Transform},
    water_treatment_clearance::Clearance,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub struct Placement<'a> {
    pub instance: u64,
    pub prefab: &'a str,
    pub pose: Transform,
}

#[derive(Serialize)]
pub struct PendingTag {
    pub instance: u64,
    pub component: i64,
    pub reason: String,
}

/// Coverage describes this supplied, accepted monument roster under the authored
/// geometry policy. The caller supplies every relevant instantiated root.
#[derive(Serialize)]
pub struct Registry {
    engine: &'static str,
    matched_monuments: Vec<Key>,
    boxes: Vec<(Key, Box3)>,
    pending: Vec<PendingTag>,
    inactive_tags: usize,
    tags_without_colliders: usize,
    physics_validated: bool,
}
impl Registry {
    pub fn assemble(
        prefabs: &BTreeMap<String, Prepared>,
        placements: &[Placement<'_>],
        needle: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        super::monument::validate_query(needle)?;
        if placements.len() > 4096 {
            return Err("Excessive monument registry placements".into());
        }
        let mut instances = BTreeSet::new();
        let mut matches = BTreeMap::new();
        let mut registry = Self {
            engine: AUTHORED_VERSION,
            matched_monuments: vec![],
            boxes: vec![],
            pending: vec![],
            inactive_tags: 0,
            tags_without_colliders: 0,
            physics_validated: false,
        };
        for placement in placements {
            if cancelled() {
                return Ok(None);
            }
            if !instances.insert(placement.instance) {
                return Err("Duplicate monument registry instance".into());
            }
            let prefab = prefabs
                .get(placement.prefab)
                .ok_or("Placed monument is absent from the prepared tag source")?;
            let matched = match matches.entry(placement.prefab) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(prefab.matching_monuments(needle)?)
                }
            };
            let Some(assembly) =
                prefab.assemble_authored(placement.instance, placement.pose, &cancelled)?
            else {
                return Ok(None);
            };
            if registry.boxes.len() + assembly.boxes.len() > 131072
                || registry.pending.len() + assembly.pending.len() > 131072
                || registry.matched_monuments.len() + matched.len() > 131072
            {
                return Err("Monument tag registry exceeded its geometry budget".into());
            }
            registry
                .matched_monuments
                .extend(matched.iter().map(|&component| Key {
                    instance: placement.instance,
                    component,
                }));
            registry.boxes.extend(assembly.boxes);
            registry
                .pending
                .extend(assembly.pending.into_iter().map(|pending| PendingTag {
                    instance: placement.instance,
                    component: pending.tag,
                    reason: pending.reason,
                }));
            registry.inactive_tags += assembly.inactive_tags;
            registry.tags_without_colliders += assembly.tags_without_colliders;
        }
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(registry))
    }
    pub fn matched_monuments(&self) -> &[Key] {
        &self.matched_monuments
    }
    pub fn boxes(&self) -> &[(Key, Box3)] {
        &self.boxes
    }
    pub fn pending(&self) -> &[PendingTag] {
        &self.pending
    }
    pub fn coverage(&self) -> Coverage {
        if self.pending.is_empty() {
            Coverage::Complete
        } else {
            Coverage::Incomplete
        }
    }
    pub fn clearance(&self, padding: f32) -> Result<Clearance, String> {
        Clearance::new(&self.boxes, padding, self.coverage())
    }
}
