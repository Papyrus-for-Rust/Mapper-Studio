use super::*;
use mapper_studio::road_generation::RoadsidePreference;
use mapper_studio::roadside_placement::added_request_id;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(crate) struct MonumentTemplate {
    pub map: u64,
    pub name: String,
    pub prefab: String,
}

impl MonumentTemplate {
    pub(crate) fn blocker(recipe: &Project, prefab: &str) -> Option<&'static str> {
        if let Some(reason) = recipe.monument_blocker(Some(prefab)) {
            Some(reason)
        } else if ["/monument/offshore/", "/monument/underwater_lab/", "/tunnel-upwards/"]
            .iter().any(|part| prefab.contains(part))
        {
            Some("This monument is managed by its generation stage")
        } else if prefab.contains("/monument/roadside/")
            && recipe.roads.as_ref().is_none_or(|roads| roads.staged.is_none())
        {
            Some("Enable staged roads to add roadside monuments")
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Addition {
    pub name: String,
    pub prefab: String,
    pub position: [f32; 2],
    pub yaw: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct MonumentChanges {
    presence: BTreeMap<usize, Presence>,
    pub additions: Vec<Addition>,
}

impl MonumentChanges {
    pub(super) fn is_empty(&self) -> bool {
        self.presence.is_empty() && self.additions.is_empty()
    }

    pub(super) fn excluded(&self, index: usize) -> bool {
        self.presence.get(&index) == Some(&Presence::Excluded)
    }

    pub(super) fn apply(
        &self,
        recipe: &mut Project,
        monuments: &[map_monuments::Monument],
    ) -> Result<(), String> {
        use papyrus_terrain::placement::MAX_TARGETS;
        if self.presence.len() > MAX_TARGETS || self.additions.len() > MAX_TARGETS {
            return Err("Too many monument edits".into());
        }
        for (&index, &presence) in &self.presence {
            let monument = monuments.get(index).ok_or("The monument is unavailable")?;
            if let Some(target) = target_index(recipe, monument) {
                recipe.targets[target].presence = presence;
            } else if let Some(prefab) = roadside_prefab(recipe, monument) {
                let preferences = &mut recipe
                    .roads
                    .as_mut()
                    .unwrap()
                    .staged
                    .as_mut()
                    .unwrap()
                    .roadside_preferences;
                if let Some(saved) = preferences
                    .iter_mut()
                    .find(|saved| saved.request == monument.id)
                {
                    saved.presence = Some(presence);
                } else {
                    preferences.push(RoadsidePreference {
                        request: monument.id,
                        prefab: prefab.into(),
                        max_displacement_m: None,
                        presence: Some(presence),
                        position: [monument.position[0], monument.position[2]],
                        yaw: monument.yaw,
                    });
                }
            } else {
                return Err("This monument is managed by its generation stage".into());
            }
        }
        for addition in &self.additions {
            if let Some(error) = MonumentTemplate::blocker(recipe, &addition.prefab) {
                return Err(error.into());
            }
            let half = recipe.size as f32 / 2.;
            if !addition.prefab.starts_with("assets/")
                || !addition.prefab.ends_with(".prefab")
                || addition.prefab.contains(['\\', '\0'])
                || addition
                    .prefab
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
                || addition.prefab.len() > 512
                || addition.name.trim().is_empty()
                || addition.name.len() > 256
                || addition.name.chars().any(char::is_control)
                || addition
                    .position
                    .iter()
                    .any(|value| !(-half..=half).contains(value))
                || addition.yaw.is_some_and(|yaw| !(0. ..360.).contains(&yaw))
            {
                return Err("Invalid monument addition".into());
            }
            if addition.prefab.contains("/monument/roadside/") {
                let preferences = &mut recipe
                    .roads
                    .as_mut()
                    .and_then(|roads| roads.staged.as_mut())
                    .ok_or("Enable staged roads to add a roadside monument")?
                    .roadside_preferences;
                let request = added_request_id(&addition.prefab, |id| {
                    preferences.iter().any(|saved| saved.request == id)
                })
                .ok_or("No more roadside copies can be added")?;
                preferences.push(RoadsidePreference {
                    request,
                    prefab: addition.prefab.clone(),
                    position: addition.position,
                    yaw: addition.yaw.unwrap_or(0.),
                    max_displacement_m: None,
                    presence: Some(Presence::Required),
                });
            } else {
                recipe.add_target(addition.name.clone(), addition.prefab.clone())?;
                let target = recipe.targets.last_mut().unwrap();
                [target.x, target.z] = addition.position;
                target.yaw = addition.yaw;
                target.radius = 125.;
                target.strength = 10.;
                if target.yaw.is_some()
                    && !mapper_studio::placement_preview::is_earthwork(&recipe.placement_engine)
                {
                    recipe.placement_engine =
                        mapper_studio::placement_preview::ROTATION_VERSION.into();
                }
            }
        }
        if !self.is_empty() {
            if let Some(policy) = recipe
                .roads
                .as_mut()
                .and_then(|roads| roads.staged.as_mut())
            {
                policy
                    .roadside_preferences
                    .sort_unstable_by_key(|saved| saved.request);
            }
            recipe.validate()?;
        }
        Ok(())
    }
}

impl Candidate {
    fn monument_presence(&self, index: usize) -> Option<Presence> {
        let monument = self.monuments.get(index)?;
        if let Some(index) = target_index(&self.recipe, monument) {
            return Some(self.recipe.targets[index].presence);
        }
        roadside_prefab(&self.recipe, monument)?;
        Some(
            self.recipe
                .roads
                .as_ref()?
                .staged
                .as_ref()?
                .roadside_preferences
                .iter()
                .find(|saved| saved.request == monument.id)
                .and_then(|saved| saved.presence)
                .unwrap_or(Presence::Allowed),
        )
    }
}

impl MapEdit {
    fn stage_roster(&mut self, result: &Candidate, next: MonumentChanges) -> Result<bool, String> {
        if self.job.is_some()
            || self.dragging()
            || self.press.is_some()
            || result.missing != 0
            || !result.archive_issue.is_empty()
        {
            return Err("This map cannot be edited right now".into());
        }
        if next == *self.monuments {
            return Ok(false);
        }
        next.apply(&mut result.recipe.clone(), &result.monuments)?;
        let after = Arc::new(next);
        let before = std::mem::replace(&mut self.monuments, Arc::clone(&after));
        self.record(DraftChange::Roster { before, after });
        Ok(true)
    }

    pub(super) fn place_template(
        &mut self,
        result: &Candidate,
        template: &MonumentTemplate,
        position: [f32; 2],
        replacement: Option<usize>,
    ) -> Result<bool, String> {
        if template.map != result.id {
            return Err("Choose a monument from this map’s catalog".into());
        }
        let mut next = (*self.monuments).clone();
        let mut addition = Addition {
            name: template.name.clone(),
            prefab: template.prefab.clone(),
            position,
            yaw: None,
        };
        if let Some(index) = replacement {
            if next.excluded(index) {
                return Err("This monument already has a removal staged".into());
            }
            result
                .monument_presence(index)
                .ok_or("This monument is managed by its generation stage")?;
            let monument = &result.monuments[index];
            let [x, z, yaw] = self.poses.get(&index).copied().unwrap_or([
                monument.position[0],
                monument.position[2],
                monument.yaw,
            ]);
            addition.position = [x, z];
            addition.yaw = Some(yaw.rem_euclid(360.));
            next.presence.insert(index, Presence::Excluded);
        }
        next.additions.push(addition);
        self.stage_roster(result, next)
    }
}

impl Generation {
    pub(crate) fn result_recipe(&self, id: u64) -> Option<&Project> {
        self.results
            .iter()
            .find(|result| result.id == id)
            .map(|result| &result.recipe)
    }

    pub(crate) fn editable_monument_presence(&self, id: u64, index: usize) -> Option<Presence> {
        let result = self.results.iter().find(|result| result.id == id)?;
        if result.missing != 0 || !result.archive_issue.is_empty() {
            return None;
        }
        let original = result.monument_presence(index)?;
        match self.edits.get(&id) {
            Some(edit) if edit.job.is_some() => None,
            Some(edit) => Some(
                edit.monuments
                    .presence
                    .get(&index)
                    .copied()
                    .unwrap_or(original),
            ),
            None => Some(original),
        }
    }

    pub(crate) fn set_monument_presence(
        &mut self,
        id: u64,
        index: usize,
        presence: Presence,
    ) -> Result<(), String> {
        let result = self
            .results
            .iter()
            .find(|result| result.id == id)
            .ok_or("Map unavailable")?;
        let original = result
            .monument_presence(index)
            .ok_or("This monument is managed by its generation stage")?;
        let edit = self.edits.entry(id).or_default();
        let mut next = (*edit.monuments).clone();
        if presence == original {
            next.presence.remove(&index);
        } else {
            next.presence.insert(index, presence);
        }
        self.revision += u64::from(edit.stage_roster(result, next)?);
        Ok(())
    }

    pub(crate) fn replace_monument(
        &mut self,
        id: u64,
        index: usize,
        template: &MonumentTemplate,
    ) -> Result<(), String> {
        let result = self
            .results
            .iter()
            .find(|result| result.id == id)
            .ok_or("Map unavailable")?;
        let edit = self.edits.entry(id).or_default();
        self.revision += u64::from(edit.place_template(result, template, [0.; 2], Some(index))?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monument_roster_edits_round_trip_and_target_instances() {
        let mut project = Project::default();
        project.targets.clear();
        let path = "assets/test/supermarket.prefab";
        for _ in 0..3 {
            project
                .add_target("Supermarket".into(), path.into())
                .unwrap();
        }
        let monuments: Vec<_> = project
            .targets
            .iter()
            .map(|target| map_monuments::Monument {
                id: target.instance_id,
                name: target.name.clone(),
                prefab: target.prefab_path.clone(),
                position: [0.; 3],
                yaw: 0.,
                restriction_origin: None,
            })
            .collect();
        let mut changes = MonumentChanges::default();
        changes.presence.insert(1, Presence::Excluded);
        changes.additions.push(Addition {
            name: "New site".into(),
            prefab: "assets/test/site.prefab".into(),
            position: [25., -75.],
            yaw: Some(90.),
        });
        let restored: MonumentChanges =
            serde_json::from_slice(&serde_json::to_vec(&changes).unwrap()).unwrap();
        assert_eq!(restored, changes);
        restored.apply(&mut project, &monuments).unwrap();
        assert_eq!(project.targets[0].presence, Presence::Required);
        assert_eq!(project.targets[1].presence, Presence::Excluded);
        assert_eq!(project.targets[2].presence, Presence::Required);
        assert_eq!(project.targets[3].instance_id, 4);
        assert_eq!([project.targets[3].x, project.targets[3].z], [25., -75.]);
        assert_eq!(project.targets[3].yaw, Some(90.));
    }

    #[test]
    fn replacing_a_single_monument_is_one_undoable_edit() {
        let recipe = Project::default();
        let target = &recipe.targets[0];
        let result = Candidate {
            identity: None,
            overview: None,
            archive: None,
            archive_issue: String::new(),
            id: 7,
            label: "Test map".into(),
            recipe: recipe.clone(),
            image: None,
            zoom: 1.,
            pan: Vec2::ZERO,
            placed: 1,
            missing: 0,
            monuments: vec![map_monuments::Monument {
                id: target.instance_id,
                name: target.name.clone(),
                prefab: target.prefab_path.clone(),
                position: [12., 10., 24.],
                yaw: 90.,
                restriction_origin: None,
            }],
            routes: Vec::new(),
            restrictions: Default::default(),
            grid: false,
            no_build: false,
            map_font: None,
            palette: None,
            background: Color32::BLACK,
        };
        let mut edit = MapEdit::default();
        let mut template = MonumentTemplate {
            map: 8,
            name: "Replacement".into(),
            prefab: "assets/test/replacement.prefab".into(),
        };
        assert!(
            edit.place_template(&result, &template, [0.; 2], Some(0))
                .is_err()
        );
        assert!(!edit.has_changes());
        template.map = result.id;
        assert!(
            edit.place_template(&result, &template, [0.; 2], Some(0))
                .unwrap()
        );
        let edited = edit.recipe(&recipe, &result.monuments).unwrap();
        assert_eq!(edited.targets[0].presence, Presence::Excluded);
        assert_eq!(edited.targets.last().unwrap().yaw, Some(90.));
        assert_eq!(
            [
                edited.targets.last().unwrap().x,
                edited.targets.last().unwrap().z
            ],
            [12., 24.]
        );
        assert_eq!(edited.seeds, recipe.seeds);
        assert!(edit.step(false, &result).unwrap());
        assert_eq!(edit.recipe(&recipe, &result.monuments).unwrap(), recipe);
        assert!(edit.step(true, &result).unwrap());
        assert_eq!(edit.recipe(&recipe, &result.monuments).unwrap(), edited);
    }
}
