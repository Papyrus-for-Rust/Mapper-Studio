//! Reusable authored candidate checks against an exact ordered generation view.
use super::*;

pub struct Candidate {
    sources_sha256: String,
    prefab: String,
    geometry: Prepared,
    preparation_work_reserved: usize,
}

impl Source {
    /// Prepare once per prefab, before its placement trials. Does not instantiate
    /// it or assert that decorators, tier, terrain and water checks have passed.
    pub fn prepare_candidate(
        &self,
        prefab: &str,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Candidate, Verdict>, String> {
        let attributes = self.attributes(prefab)?;
        Candidate::prepare(&self.sha256, prefab, attributes, maximum_work, cancelled)
    }
}

impl Candidate {
    pub(super) fn prepare(
        sources_sha256: &str,
        prefab: &str,
        attributes: &Attributes,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Self, Verdict>, String> {
        if !(1..=MAX_WORK).contains(&maximum_work) {
            return Err("Invalid environment candidate preparation allowance".into());
        }
        let mut work = 0;
        if let Err(v) = charge(
            &mut work,
            preparation_cost(attributes)?,
            maximum_work,
            &cancelled,
        ) {
            return Ok(Err(v));
        }
        let Some(geometry) = Prepared::new(attributes, &cancelled)? else {
            return Ok(Err(Verdict::Cancelled));
        };
        if cancelled() {
            return Ok(Err(Verdict::Cancelled));
        }
        Ok(Ok(Self {
            sources_sha256: sources_sha256.into(),
            prefab: prefab.into(),
            geometry,
            preparation_work_reserved: work,
        }))
    }
    pub fn prefab(&self) -> &str {
        &self.prefab
    }
    pub fn preparation_work_reserved(&self) -> usize {
        self.preparation_work_reserved
    }
    /// Assess the post-decorator trial frame, before spawning its winning group.
    /// The forbidden mask belongs to the owning verified placement adapter.
    /// Missing coverage stops the stage even if this prefab has no query shapes.
    /// Only Blocked is an ordinary rejected location; all other non-Clear states
    /// must stop/propagate through the owning search, never become empty success.
    /// This allowance covers this assessment only, separately from preparation,
    /// prefix construction and the candidate search's own work.
    #[allow(clippy::too_many_arguments)]
    pub fn assess(
        &self,
        snapshot: &scene::Snapshot,
        request: &Request,
        prefix: &scene::Prefix,
        pose: Transform,
        forbidden: u32,
        tolerance: f64,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<environment::Assessment, String> {
        if request.context.sources_sha256 != self.sources_sha256 {
            return Err("Environment candidate belongs to a different source".into());
        }
        let complete = validate_prefix(snapshot, request, prefix)?;
        pose.validate()?;
        if !(1..=MAX_WORK).contains(&maximum_work)
            || !tolerance.is_finite()
            || !(0. ..=1.).contains(&tolerance)
        {
            return Err("Invalid environment candidate assessment allowance or tolerance".into());
        }
        let stopped = cancelled();
        if stopped || !complete {
            return Ok(environment::Assessment {
                verdict: if stopped {
                    Verdict::Cancelled
                } else {
                    Verdict::Incomplete
                },
                work: 0,
                candidates: 0,
            });
        }
        self.geometry.assess(
            prefix.world(),
            pose,
            forbidden,
            tolerance,
            maximum_work,
            cancelled,
        )
    }
}
