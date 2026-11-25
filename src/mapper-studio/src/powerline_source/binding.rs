//! Bind captured utility attachments to their own PathList family and Mapper policy.
use super::*;
use crate::road_object_placement::Policy;
use serde::Serialize;

const POLICY: &str = include_str!("../../../../compatibility/powerline-objects/mapper.json");

/// WorldConfig applies case-sensitive substring exclusions before inclusions.
/// An empty inclusion list admits every path that survives the exclusions.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRules {
    pub blacklist: Vec<String>,
    pub whitelist: Vec<String>,
}
impl ResourceRules {
    pub fn validate(&self) -> Result<(), String> {
        if self.blacklist.len() > 4096
            || self.whitelist.len() > 4096
            || self
                .blacklist
                .iter()
                .chain(&self.whitelist)
                .any(|pattern| pattern.len() > 512)
        {
            return Err("Utility resource filters exceed their bounds".into());
        }
        Ok(())
    }
    pub fn allows(&self, path: &str) -> bool {
        !self.blacklist.iter().any(|pattern| path.contains(pattern))
            && (self.whitelist.is_empty()
                || self.whitelist.iter().any(|pattern| path.contains(pattern)))
    }
}

fn placement(path: &str, prefab: &Prefab) -> Result<road_object_source::Prefab, String> {
    let mut placement: road_object_source::Prefab =
        serde_json::from_str(prefab.placement.get()).map_err(|error| error.to_string())?;
    let sequences: Vec<_> = prefab
        .runtime
        .iter()
        .filter(|row| {
            row.hierarchy_active_without_root
                && row.component.as_array().is_some_and(|owner| {
                    owner.len() == 3
                        && owner[0] == "Assembly-CSharp"
                        && owner[1] == ""
                        && owner[2] == "PathSequencePowerline"
                })
        })
        .collect();
    if sequences.len()
        != placement
            .components
            .get("PathSequencePowerline")
            .copied()
            .unwrap_or(0)
        || placement
            .attributes
            .iter()
            .any(|attribute| attribute["kind"] == "PathSequencePowerline")
    {
        return Err("Utility sequence runtime coverage differs from its placement capture".into());
    }
    if sequences.is_empty() {
        return Ok(placement);
    }
    let identity = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    let roots: BTreeSet<_> = prefab
        .runtime
        .iter()
        .filter(|row| {
            row.hierarchy_active_without_root
                && row.name.to_lowercase() == path
                && row.local_to_root == identity
        })
        .map(|row| row.game_object)
        .collect();
    if roots.len() != 1 {
        return Err("Utility sequence requires an unambiguous prefab root".into());
    }
    let root = *roots.first().ok_or("Missing utility prefab root")?;
    for row in sequences {
        let fields: serde_json::Value = serde_json::from_str(
            row.fields
                .as_ref()
                .ok_or("Missing utility sequence fields")?
                .get(),
        )
        .map_err(|error| error.to_string())?;
        let rule = fields
            .get("Rule")
            .and_then(serde_json::Value::as_u64)
            .filter(|rule| *rule <= 1)
            .ok_or("Invalid utility sequence rule")?;
        placement.attributes.push(serde_json::json!({
            "kind":"PathSequencePowerline", "root":row.game_object == root,
            "position":[row.local_to_root[0][3],row.local_to_root[1][3],row.local_to_root[2][3]],
            "local_to_root":row.local_to_root,"fields":{"Rule":rule},
        }));
    }
    Ok(placement)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingPolicy {
    format: String,
    schema: u32,
    policy: Policy,
}
impl BindingPolicy {
    fn read(raw: &str) -> Result<Self, String> {
        let profile: Self = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        profile.policy.validate()?;
        if profile.format != "mapper-powerline-object-policy"
            || profile.schema != 1
            || profile.policy.powerline_sequence.is_none()
            || profile.policy.resource_weights.is_none()
        {
            return Err("Invalid Mapper utility attachment policy".into());
        }
        Ok(profile)
    }
}

#[cfg(test)]
mod tests;
impl Source {
    /// Prepare the source-bound fitter input. Utility layout and execution order
    /// belong to the coordinator that supplies the generated utility paths.
    pub fn bind_objects(
        &self,
        road: &road_object_source::Source,
        resources: &ResourceRules,
        cancelled: impl Fn() -> bool,
    ) -> Result<road_object_source::Source, String> {
        if cancelled() {
            return Err("Utility attachment binding cancelled".into());
        }
        resources.validate()?;
        if self.document.procedural_order
            != *road.workflow.as_ref().ok_or("Missing road workflow")?
            || self.document.stage_layout != road.stage_layout().unwrap_or("legacy")
        {
            return Err("Utility attachments differ from the selected procedural workflow".into());
        }
        road.verify_common_receipts(&self.document.sources, &self.document.extra_sources)?;
        let profile = BindingPolicy::read(POLICY)?;
        self.objects(road, resources, profile.policy, POLICY, &cancelled)
    }

    fn objects(
        &self,
        road: &road_object_source::Source,
        resources: &ResourceRules,
        policy: Policy,
        profile: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<road_object_source::Source, String> {
        let mut stages = Vec::new();
        let mut requested = BTreeSet::new();
        for program in self
            .document
            .stages
            .iter()
            .filter(|program| program.kind == "PlacePowerlineObjects")
        {
            if cancelled() {
                return Err("Utility attachment binding cancelled".into());
            }
            let mut stage = road_object_source::Stage {
                object_id: program.object_id,
                order: stages.len(),
                scene_order: Some(program.order),
                name: program.description.clone(),
                mode: program.mode,
                disabled: program.disabled,
                component_enabled: program.component_enabled,
                hierarchy_active: program.hierarchy_active,
                start: program.start.clone().ok_or("Missing utility start rules")?,
                end: program.end.clone().ok_or("Missing utility end rules")?,
                side: program.side.clone().ok_or("Missing utility side rules")?,
                path: program.path.clone().ok_or("Missing utility path rules")?,
            };
            let groups = stage
                .start
                .iter_mut()
                .map(|rule| &mut rule.prefabs)
                .chain(stage.end.iter_mut().map(|rule| &mut rule.prefabs))
                .chain(stage.side.iter_mut().map(|rule| &mut rule.prefabs))
                .chain(stage.path.iter_mut().map(|rule| &mut rule.prefabs));
            for paths in groups {
                paths.retain(|path| resources.allows(path));
            }
            road_object_source::validate_attachment_resources(
                &stage.start,
                &stage.end,
                &stage.side,
                &stage.path,
                &mut requested,
            )?;
            stages.push(stage);
        }
        let prefabs = requested
            .iter()
            .map(|path| {
                let prefab = self
                    .document
                    .prefabs
                    .get(path)
                    .ok_or("Missing utility placement prefab")?;
                Ok((path, placement(path, prefab)?))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let current = self.document.stage_layout == "mode-description";
        let mut document = serde_json::json!({
            "format":"papyrus-road-object-source", "schema":if current {7} else {6},
            "adapter":road_object_source::PATH_VERSION, "path_family":"powerline",
            "infrastructure_sha256":road.infrastructure_sha256, "sources":self.document.sources,
            "extra_sources":self.document.extra_sources, "resources":self.document.wire_resources,
            "extractor_sha256":self.document.extractor_sha256, "path_policy":policy,
            "placement_profile_sha256":format!("{:x}",Sha256::digest(profile.as_bytes())),
            "stages":stages, "workflow":self.document.procedural_order, "prefabs":prefabs, "issues":[],
        });
        if current {
            document["stage_layout"] = "mode-description".into();
        }
        let bytes = serde_json::to_vec(&document).map_err(|error| error.to_string())?;
        road_object_source::Source::parse(
            &bytes,
            &road.infrastructure_sha256,
            &self.document.sources,
            cancelled,
        )
    }
}
