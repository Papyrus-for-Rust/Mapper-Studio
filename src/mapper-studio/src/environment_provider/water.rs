use super::*;
use crate::{
    monument_attributes::{WaterBody, WaterPlacement, WaterTrigger},
    placement_water::{self, LookupState, Selection, Trigger},
};

/// A source-bound water gate for a loaded prefab, not a spawned world's water.
pub struct WaterCandidate {
    pub(super) sources_sha256: String,
    pub(super) model: placement_water::Model,
    pub(super) facts: Option<WaterPlacement>,
}
impl Source {
    /// None means the selected runtime has no inspected water-program adapter.
    /// A supported runtime with an old capture returns an Unknown assessment.
    pub fn prepare_water(&self, prefab: &str) -> Result<Option<WaterCandidate>, String> {
        let Some(model) = &self.water else {
            return Ok(None);
        };
        let attributes = self.attributes(prefab)?;
        attributes.validate()?;
        Ok(Some(WaterCandidate {
            sources_sha256: self.sha256.clone(),
            model: model.clone(),
            facts: attributes
                .environment
                .as_ref()
                .and_then(|e| e.water_placement.clone()),
        }))
    }
}
impl WaterCandidate {
    /// The producer must confirm the loaded prefab frame and, when supplied,
    /// the actual first-child lookup result. Raw body positions/mesh extents
    /// must not substitute for runtime transform Y or collider.bounds.max.y.
    #[allow(clippy::too_many_arguments)]
    pub fn assess(
        &self,
        request: &Request,
        state: LookupState,
        selection: Selection,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
        body_y: impl FnMut(&WaterBody) -> Result<Option<f32>, String>,
        trigger: impl FnMut(&WaterTrigger) -> Result<Trigger, String>,
    ) -> Result<Result<placement_water::Assessment, Verdict>, String> {
        if request.context.sources_sha256 != self.sources_sha256 {
            return Err("Water placement candidate belongs to another source".into());
        }
        self.model.assess(
            self.facts.as_ref(),
            state,
            selection,
            maximum_work,
            cancelled,
            body_y,
            trigger,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_bound_water_candidate_rejects_a_different_request_source() {
        let context = scene::Context {
            request_sha256: "a".repeat(64),
            sources_sha256: "b".repeat(64),
        };
        let request = request(&context, 1, &[], |_| true).unwrap();
        let mut candidate = WaterCandidate {
            sources_sha256: "c".repeat(64),
            model: placement_water::Model::client_reference().unwrap(),
            facts: None,
        };
        assert!(
            candidate
                .assess(
                    &request,
                    LookupState::AuthoredUnchanged,
                    Selection::Unknown,
                    100,
                    || false,
                    |_| panic!(),
                    |_| panic!()
                )
                .is_err()
        );
        candidate.sources_sha256 = context.sources_sha256;
        assert_eq!(
            candidate
                .assess(
                    &request,
                    LookupState::AuthoredUnchanged,
                    Selection::Unknown,
                    100,
                    || false,
                    |_| panic!(),
                    |_| panic!()
                )
                .unwrap()
                .unwrap()
                .water,
            papyrus_terrain::offshore_tiers::WaterGate::Unknown
        );
    }
}
