//! Source-profiled water part of MonumentInfo.CheckPlacement. Runtime lookup
//! and collider bounds are supplied as facts, never inferred from static meshes.
use crate::{
    environment_runtime::Verified,
    monument_attributes::{WaterBody, WaterPlacement, WaterTrigger},
};
use papyrus_terrain::{environment::Verdict, offshore_tiers::WaterGate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    schema: u32,
    id: String,
    environment_runtime_sha256: String,
    native_sha256: String,
    program: String,
    lake_type: i32,
    include_inactive: bool,
}
#[derive(Clone)]
pub struct Model(Profile);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupState {
    /// The producer confirmed preprocessing left the captured body roster/types
    /// intact. This is not implied by having accepted a proposed world pose.
    AuthoredUnchanged,
    Unmodeled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    Unknown,
    /// Confirmed source lookup result, not simply an empty UI selection.
    None,
    Body(i64),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trigger {
    /// Null/destroyed runtime object, including a nonzero serialized reference.
    Null,
    BoundsMaximumY(f32),
    Unknown,
}
#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub profile: String,
    pub profile_sha256: String,
    pub water: WaterGate,
    pub selected_body: Option<i64>,
    pub work: usize,
}
impl Model {
    /// Explicit diagnostic reference; use for_runtime for a selected source.
    pub fn client_reference() -> Result<Self, String> {
        let p: Profile = serde_json::from_str(include_str!(
            "../../../compatibility/monuments/client-e7156192-water-placement.json"
        ))
        .map_err(|e| e.to_string())?;
        let hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if p.format != "papyrus-water-placement-runtime-profile"
            || p.schema != 1
            || p.id.is_empty()
            || p.id.len() > 128
            || !hash(&p.environment_runtime_sha256)
            || !hash(&p.native_sha256)
            || p.program != "first-child-lake-minimum-1"
            || p.include_inactive
        {
            return Err("Unsupported water placement runtime profile".into());
        }
        Ok(Self(p))
    }
    pub fn for_runtime(runtime: &Verified) -> Result<Option<Self>, String> {
        let model = Self::client_reference()?;
        Ok((runtime.native().sha256 == model.0.native_sha256
            && runtime.fingerprint()? == model.0.environment_runtime_sha256)
            .then_some(model))
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&self.0).map_err(|e| e.to_string())?)
        ))
    }
    /// Classification/minimum only. The owning gate adds the proposed position
    /// Y and applies its threshold. Callbacks must describe the same pre-spawn
    /// prefab lookup frame. Their own work must also be bounded by the producer.
    #[allow(clippy::too_many_arguments)]
    pub fn assess(
        &self,
        facts: Option<&WaterPlacement>,
        state: LookupState,
        selection: Selection,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
        mut body_y: impl FnMut(&WaterBody) -> Result<Option<f32>, String>,
        mut trigger: impl FnMut(&WaterTrigger) -> Result<Trigger, String>,
    ) -> Result<Result<Assessment, Verdict>, String> {
        if !(1..=128_000_000).contains(&maximum_work) {
            return Err("Invalid water placement work allowance".into());
        }
        if let Some(facts) = facts {
            facts.validate(&mut Default::default())?;
        }
        let mut result = Assessment {
            profile: self.0.id.clone(),
            profile_sha256: self.fingerprint()?,
            water: WaterGate::Unknown,
            selected_body: None,
            work: 0,
        };
        let charge = |work: &mut usize| -> Result<(), Verdict> {
            if cancelled() {
                return Err(Verdict::Cancelled);
            }
            if *work == maximum_work {
                return Err(Verdict::Budget);
            }
            *work += 1;
            Ok(())
        };
        if let Err(v) = charge(&mut result.work) {
            return Ok(Err(v));
        }
        let Some(facts) = facts else {
            return Ok(Ok(result));
        };
        if state == LookupState::Unmodeled || !facts.issues.is_empty() {
            return Ok(Ok(result));
        }
        let mut selected = None;
        let mut any_lake = false;
        for body in &facts.bodies {
            if let Err(v) = charge(&mut result.work) {
                return Ok(Err(v));
            }
            any_lake |= body.body_type == self.0.lake_type;
            if selection == Selection::Body(body.object_id) {
                selected = Some(body);
            }
        }
        match selection {
            Selection::None => result.water = WaterGate::NotLake,
            // Every possible selected child (or absence) is non-lake, without
            // guessing which inactive-root lookup policy chose it.
            Selection::Unknown if !any_lake => result.water = WaterGate::NotLake,
            Selection::Unknown => {}
            Selection::Body(_) => {
                let body = selected.ok_or("Selected water body is absent from captured roster")?;
                if !body.hierarchy_active_without_root {
                    return Err(
                        "Selected water body contradicts the inactive-child lookup policy".into(),
                    );
                }
                result.selected_body = Some(body.object_id);
                if body.body_type != self.0.lake_type {
                    result.water = WaterGate::NotLake;
                } else {
                    if let Err(v) = charge(&mut result.work) {
                        return Ok(Err(v));
                    }
                    let level = body_y(body)?;
                    if cancelled() {
                        return Ok(Err(Verdict::Cancelled));
                    }
                    let Some(mut minimum) = level else {
                        return Ok(Ok(result));
                    };
                    valid_level(minimum)?;
                    let mut unknown = false;
                    for reference in &body.triggers {
                        if let Err(v) = charge(&mut result.work) {
                            return Ok(Err(v));
                        }
                        if reference.file_id == 0 && reference.path_id == 0 {
                            continue;
                        }
                        match trigger(reference)? {
                            Trigger::Null => {}
                            Trigger::Unknown => unknown = true,
                            Trigger::BoundsMaximumY(value) => {
                                valid_level(value)?;
                                if value <= minimum {
                                    minimum = value;
                                }
                            }
                        }
                    }
                    if !unknown {
                        result.water = WaterGate::Lake {
                            minimum_level: minimum,
                        };
                    }
                }
            }
        }
        if cancelled() {
            return Ok(Err(Verdict::Cancelled));
        }
        Ok(Ok(result))
    }
}
fn valid_level(value: f32) -> Result<(), String> {
    if !value.is_finite() || value.abs() > 1_000_000. {
        Err("Invalid runtime water position or trigger bounds".into())
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests;
