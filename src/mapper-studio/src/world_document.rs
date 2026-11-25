//! In-memory ownership and completion gate for future editable viewport documents.
//!
//! A producer must request every recipe stage, bind the exact recipe/source to
//! `inputs`, and report success only after validating that stage's output. This
//! coordinator does not infer completion from images or certify playable export.
//! Normal generation and persisted per-tab snapshots are not wired here yet.
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Terrain,
    Biomes,
    Monuments,
    CustomContent,
    RiversAndWater,
    RoadsAndTrails,
    Rails,
    Underground,
    Offshore,
    Scenery,
    Composition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Generating,
    Incomplete,
    ReadyToEdit,
    Rebuilding,
}

/// Opaque identity for one attempt in one document. Identical inputs in another
/// attempt/document do not make a late worker result eligible for publication.
#[derive(Clone, Debug)]
pub struct Ticket(Arc<Request>);

#[derive(Debug)]
struct Request {
    inputs: [u8; 32],
    required: BTreeSet<Stage>,
}

struct Build {
    ticket: Ticket,
    completed: BTreeSet<Stage>,
}

/// Immutable accepted world payload, not an overview texture. Producers choose
/// the payload type; the UI must retain terrain, objects, networks and receipts.
pub struct Snapshot<T> {
    pub inputs: [u8; 32],
    pub world: T,
    required: BTreeSet<Stage>,
}

impl<T> Snapshot<T> {
    /// The complete requested feature set of this accepted revision. Editing a
    /// monument or route cannot turn a complete world into a partial preview by
    /// silently dropping the other requested stages.
    pub fn required_stages(&self) -> &BTreeSet<Stage> {
        &self.required
    }
}

/// Each viewport tab owns its own document. Failed/cancelled rebuilds keep the
/// accepted revision but cannot unlock mutations against a pending proposal.
pub struct Document<T> {
    accepted: Option<Arc<Snapshot<T>>>,
    pending: Option<Build>,
    issue: Option<String>,
}

impl<T> Default for Document<T> {
    fn default() -> Self {
        Self {
            accepted: None,
            pending: None,
            issue: None,
        }
    }
}

impl<T> Document<T> {
    /// Start a geometry edit against the exact complete world being displayed.
    /// Inspector, drag, keyboard and context-menu commands share this boundary.
    /// `inputs` binds the proposed recipe/source; `current_inputs` binds the
    /// accepted revision before the edit. Required dependent stages must be
    /// supplied by the command owner, including unsupported requested stages.
    /// They are added to the accepted world's stage set, never substituted for
    /// it. Feature removal belongs to a new recipe generation, not a geometry
    /// command. Unchanged stages still need validated output/reuse receipts.
    /// Rejection must not supersede a running build or discard its diagnostics.
    pub fn begin_edit(
        &mut self,
        displayed: &Arc<Snapshot<T>>,
        current_inputs: &[u8; 32],
        inputs: [u8; 32],
        required: impl IntoIterator<Item = Stage>,
    ) -> Result<Ticket, &'static str> {
        if !self.can_edit(displayed, current_inputs) {
            return Err("Editing requires the current complete world with no pending rebuild");
        }
        let mut stages = displayed.required.clone();
        stages.extend(required);
        Ok(self.begin(inputs, stages))
    }

    /// `required` includes unsupported requested stages. Deliberately disabled
    /// stages alone may be omitted. Terrain and final composition are mandatory.
    pub fn begin(&mut self, inputs: [u8; 32], required: impl IntoIterator<Item = Stage>) -> Ticket {
        let mut required: BTreeSet<_> = required.into_iter().collect();
        required.extend([Stage::Terrain, Stage::Composition]);
        let ticket = Ticket(Arc::new(Request { inputs, required }));
        self.pending = Some(Build {
            ticket: ticket.clone(),
            completed: BTreeSet::new(),
        });
        self.issue = None;
        ticket
    }

    fn current(&mut self, ticket: &Ticket) -> Result<&mut Build, &'static str> {
        self.pending
            .as_mut()
            .filter(|build| Arc::ptr_eq(&build.ticket.0, &ticket.0))
            .ok_or("Obsolete or foreign world build")
    }

    /// Called by the stage owner after its output and input binding are checked.
    /// This is not a caller-controlled UI completion toggle.
    pub fn stage_succeeded(&mut self, ticket: &Ticket, stage: Stage) -> Result<(), &'static str> {
        if self.issue.is_some() {
            return Err("World build has stopped");
        }
        let build = self.current(ticket)?;
        if !build.ticket.0.required.contains(&stage) {
            return Err("Stage was not requested by this build");
        }
        build.completed.insert(stage);
        Ok(())
    }

    pub fn missing(&self) -> Vec<Stage> {
        self.pending.as_ref().map_or_else(Vec::new, |build| {
            build
                .ticket
                .0
                .required
                .difference(&build.completed)
                .copied()
                .collect()
        })
    }

    /// Consumes a fully assembled immutable world only after all requested
    /// stages succeed. A rejected publish leaves both revisions untouched.
    pub fn publish(&mut self, ticket: &Ticket, world: T) -> Result<(), &'static str> {
        if self.issue.is_some() {
            return Err("World build has stopped");
        }
        let build = self.current(ticket)?;
        if build.completed != build.ticket.0.required {
            return Err("Requested world stages are incomplete");
        }
        self.accepted = Some(Arc::new(Snapshot {
            inputs: ticket.0.inputs,
            world,
            required: ticket.0.required.clone(),
        }));
        self.pending = None;
        self.issue = None;
        Ok(())
    }

    /// Stops this attempt; retry must start a new ticket. The proposal/input
    /// binding remains until retry or discard; accepted data stays inspectable.
    pub fn fail(&mut self, ticket: &Ticket, reason: String) -> Result<(), &'static str> {
        self.current(ticket)?;
        self.issue = Some(reason);
        Ok(())
    }

    /// Retry a stopped proposal with its exact inputs and complete stage set.
    /// Previously successful stages must be validated again, including any
    /// explicitly reused outputs. A fresh ticket rejects results still arriving
    /// from the failed attempt. A rejected retry leaves the current build intact.
    pub fn retry(&mut self, ticket: &Ticket) -> Result<Ticket, &'static str> {
        self.current(ticket)?;
        if self.issue.is_none() {
            return Err("Only a stopped world build can be retried");
        }
        Ok(self.begin(ticket.0.inputs, ticket.0.required.iter().copied()))
    }

    /// Explicitly abandon a running or failed proposal and return to the last
    /// accepted world. A stale cancellation cannot cancel a newer attempt.
    pub fn discard(&mut self, ticket: &Ticket) -> Result<(), &'static str> {
        self.current(ticket)?;
        self.pending = None;
        self.issue = None;
        Ok(())
    }

    pub fn state(&self) -> State {
        match (&self.accepted, &self.pending, &self.issue) {
            (_, _, Some(_)) => State::Incomplete,
            (None, Some(_), _) => State::Generating,
            (Some(_), Some(_), _) => State::Rebuilding,
            (None, None, None) => State::Incomplete,
            (Some(_), None, None) => State::ReadyToEdit,
        }
    }

    pub fn accepted(&self) -> Option<&Arc<Snapshot<T>>> {
        self.accepted.as_ref()
    }
    pub fn issue(&self) -> Option<&str> {
        self.issue.as_deref()
    }

    /// All future mutation entry points must call this with the displayed input
    /// binding, including inspector, context menu, keyboard and drag commands.
    pub fn can_edit(&self, displayed: &Arc<Snapshot<T>>, current_inputs: &[u8; 32]) -> bool {
        self.state() == State::ReadyToEdit
            && self
                .accepted
                .as_ref()
                .is_some_and(|s| Arc::ptr_eq(s, displayed) && &s.inputs == current_inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn can_edit(document: &Document<&str>, inputs: &[u8; 32]) -> bool {
        document
            .accepted()
            .is_some_and(|snapshot| document.can_edit(snapshot, inputs))
    }

    fn finish(document: &mut Document<&str>, ticket: &Ticket, value: &'static str) {
        for stage in document.missing() {
            document.stage_succeeded(ticket, stage).unwrap();
        }
        document.publish(ticket, value).unwrap();
    }

    #[test]
    fn images_and_partial_stages_do_not_unlock_editing() {
        let mut document = Document::default();
        let ticket = document.begin([1; 32], [Stage::Monuments, Stage::Offshore]);
        document.stage_succeeded(&ticket, Stage::Terrain).unwrap();
        document
            .stage_succeeded(&ticket, Stage::Composition)
            .unwrap();
        assert_eq!(document.missing(), vec![Stage::Monuments, Stage::Offshore]);
        assert!(document.publish(&ticket, "image only").is_err());
        assert!(document.accepted().is_none());
        assert!(!can_edit(&document, &[1; 32]));
        assert_eq!(document.state(), State::Generating);
    }

    #[test]
    fn exact_requested_stages_publish_and_bind_editing_to_inputs() {
        let mut document = Document::default();
        let ticket = document.begin([2; 32], [Stage::Monuments, Stage::RoadsAndTrails]);
        assert!(document.stage_succeeded(&ticket, Stage::Rails).is_err());
        finish(&mut document, &ticket, "complete world");
        assert_eq!(document.state(), State::ReadyToEdit);
        assert!(can_edit(&document, &[2; 32]));
        assert!(!can_edit(&document, &[3; 32]));
        assert!(document.stage_succeeded(&ticket, Stage::Terrain).is_err());
    }

    #[test]
    fn late_and_cross_tab_results_cannot_publish_even_with_identical_inputs() {
        let mut first = Document::default();
        let mut second = Document::default();
        let old = first.begin([1; 32], []);
        let current = first.begin([1; 32], []);
        second.begin([1; 32], []);
        assert!(first.stage_succeeded(&old, Stage::Terrain).is_err());
        assert!(first.fail(&old, "late failure".into()).is_err());
        assert!(second.publish(&current, "foreign world").is_err());
        finish(&mut first, &current, "first tab");
        assert!(second.accepted().is_none());
        assert_eq!(second.state(), State::Generating);
    }

    #[test]
    fn rebuilds_preserve_immutable_accepted_world_and_failures_lock_edits() {
        let mut document = Document::default();
        let initial = document.begin([1; 32], []);
        finish(&mut document, &initial, "old world");
        let retained = document.accepted().unwrap().clone();
        let edit = document.begin([2; 32], [Stage::RoadsAndTrails]);
        assert_eq!(document.state(), State::Rebuilding);
        assert_eq!(document.accepted().unwrap().world, "old world");
        assert!(!can_edit(&document, &[1; 32]));
        assert!(document.publish(&edit, "partial world").is_err());
        document
            .fail(&edit, "Road attachment failed".into())
            .unwrap();
        assert_eq!(document.state(), State::Incomplete);
        assert_eq!(document.issue(), Some("Road attachment failed"));
        assert!(Arc::ptr_eq(document.accepted().unwrap(), &retained));
        assert!(!can_edit(&document, &[1; 32]));
        assert!(document.publish(&edit, "late world").is_err());
        let retry = document.begin([2; 32], [Stage::RoadsAndTrails]);
        finish(&mut document, &retry, "new world");
        assert_eq!(retained.world, "old world");
        assert_eq!(document.accepted().unwrap().world, "new world");
        assert!(can_edit(&document, &[2; 32]));
    }

    #[test]
    fn explicit_discard_restores_accepted_revision_and_rejects_late_results() {
        let mut document = Document::default();
        let initial = document.begin([1; 32], []);
        finish(&mut document, &initial, "accepted");
        let cancelled = document.begin([2; 32], [Stage::Rails]);
        document.discard(&cancelled).unwrap();
        assert!(can_edit(&document, &[1; 32]));
        assert!(document.publish(&cancelled, "late").is_err());
        let failed = document.begin([3; 32], [Stage::Offshore]);
        document.fail(&failed, "Unsupported stage".into()).unwrap();
        assert!(document.stage_succeeded(&failed, Stage::Offshore).is_err());
        assert!(document.missing().contains(&Stage::Offshore));
        assert!(document.discard(&cancelled).is_err());
        document.discard(&failed).unwrap();
        assert!(can_edit(&document, &[1; 32]));
        assert!(document.issue().is_none());
    }

    #[test]
    fn identical_inputs_do_not_make_a_foreign_or_old_display_editable() {
        let mut first = Document::default();
        let mut second = Document::default();
        let a = first.begin([1; 32], []);
        finish(&mut first, &a, "first");
        let old = first.accepted().unwrap().clone();
        let b = second.begin([1; 32], []);
        finish(&mut second, &b, "second");
        assert!(!second.can_edit(&old, &[1; 32]));
        let replacement = first.begin([1; 32], []);
        finish(&mut first, &replacement, "replacement");
        assert!(!first.can_edit(&old, &[1; 32]));
        assert!(can_edit(&first, &[1; 32]));
    }

    #[test]
    fn geometry_commands_require_the_current_complete_display_and_preserve_pending_work() {
        let mut first = Document::default();
        let initial = first.begin([1; 32], [Stage::Monuments, Stage::RoadsAndTrails]);
        let mut other = Document::default();
        let foreign = other.begin([1; 32], []);
        finish(&mut other, &foreign, "other tab");
        let foreign_display = other.accepted().unwrap().clone();
        assert!(
            first
                .begin_edit(&foreign_display, &[1; 32], [2; 32], [])
                .is_err()
        );
        // The rejected command cannot retire the original generation ticket.
        finish(&mut first, &initial, "accepted world");
        let displayed = first.accepted().unwrap().clone();
        assert!(
            first
                .begin_edit(&foreign_display, &[1; 32], [2; 32], [])
                .is_err()
        );
        assert!(first.begin_edit(&displayed, &[9; 32], [2; 32], []).is_err());
        assert_eq!(first.state(), State::ReadyToEdit);
        let edit = first
            .begin_edit(
                &displayed,
                &[1; 32],
                [2; 32],
                [Stage::Monuments, Stage::RoadsAndTrails],
            )
            .unwrap();
        assert_eq!(first.state(), State::Rebuilding);
        assert!(Arc::ptr_eq(first.accepted().unwrap(), &displayed));
        assert!(
            first
                .begin_edit(&displayed, &[1; 32], [3; 32], [Stage::Rails])
                .is_err()
        );
        assert!(first.publish(&edit, "monument-only raster").is_err());
        first
            .fail(&edit, "Road entrance no longer fits".into())
            .unwrap();
        assert!(first.begin_edit(&displayed, &[1; 32], [3; 32], []).is_err());
        assert_eq!(first.issue(), Some("Road entrance no longer fits"));
        first.discard(&edit).unwrap();
        let replacement = first
            .begin_edit(
                &displayed,
                &[1; 32],
                [2; 32],
                [Stage::Monuments, Stage::RoadsAndTrails],
            )
            .unwrap();
        finish(&mut first, &replacement, "replacement and rebuilt roads");
        assert!(first.begin_edit(&displayed, &[1; 32], [3; 32], []).is_err());
        assert_eq!(displayed.world, "accepted world");
        assert_eq!(
            first.accepted().unwrap().world,
            "replacement and rebuilt roads"
        );
    }

    #[test]
    fn monument_edit_cannot_drop_routes_water_or_other_requested_world_stages() {
        let mut document = Document::default();
        let initial = document.begin(
            [1; 32],
            [
                Stage::Monuments,
                Stage::RiversAndWater,
                Stage::RoadsAndTrails,
                Stage::Rails,
            ],
        );
        finish(&mut document, &initial, "complete map");
        let displayed = document.accepted().unwrap().clone();
        let expected = displayed.required_stages().clone();
        // A Replace Monument handler only names its immediate work. Roads,
        // rails and water must not disappear from the publication boundary.
        let edit = document
            .begin_edit(
                &displayed,
                &[1; 32],
                [2; 32],
                [Stage::Monuments, Stage::CustomContent],
            )
            .unwrap();
        for stage in [
            Stage::Terrain,
            Stage::Composition,
            Stage::Monuments,
            Stage::CustomContent,
        ] {
            document.stage_succeeded(&edit, stage).unwrap();
        }
        assert_eq!(
            document.missing(),
            vec![Stage::RiversAndWater, Stage::RoadsAndTrails, Stage::Rails]
        );
        assert!(document.publish(&edit, "monument-only edit").is_err());
        assert!(Arc::ptr_eq(document.accepted().unwrap(), &displayed));
        assert!(!document.can_edit(&displayed, &[1; 32]));
        finish(
            &mut document,
            &edit,
            "replacement with complete rebuilt world",
        );
        let accepted = document.accepted().unwrap().clone();
        let mut required = expected;
        required.insert(Stage::CustomContent);
        assert_eq!(accepted.required_stages(), &required);
        // A later spline edit inherits this same contract, even with no added
        // stages. Repeated edits cannot gradually erase feature requirements.
        document
            .begin_edit(&accepted, &[2; 32], [3; 32], [])
            .unwrap();
        assert_eq!(document.missing(), required.into_iter().collect::<Vec<_>>());
        assert_eq!(displayed.world, "complete map");
    }

    #[test]
    fn retry_preserves_the_proposal_binding_and_rejects_failed_attempt_results() {
        let mut document = Document::default();
        let initial = document.begin([1; 32], [Stage::Monuments, Stage::RoadsAndTrails]);
        finish(&mut document, &initial, "accepted map");
        let displayed = document.accepted().unwrap().clone();
        let edit = document
            .begin_edit(&displayed, &[1; 32], [2; 32], [Stage::Rails])
            .unwrap();
        // Retrying a running attempt must not replace its ticket.
        assert!(document.retry(&edit).is_err());
        document.stage_succeeded(&edit, Stage::Terrain).unwrap();
        document
            .fail(&edit, "Rail attachment failed".into())
            .unwrap();
        let retry = document.retry(&edit).unwrap();
        assert_eq!(document.state(), State::Rebuilding);
        assert!(document.issue().is_none());
        assert!(document.missing().contains(&Stage::Terrain));
        assert!(document.missing().contains(&Stage::Rails));
        assert!(Arc::ptr_eq(document.accepted().unwrap(), &displayed));
        assert!(document.stage_succeeded(&edit, Stage::Rails).is_err());
        assert!(document.fail(&edit, "Late failure".into()).is_err());
        assert!(document.discard(&edit).is_err());
        assert!(document.retry(&edit).is_err());
        assert!(document.publish(&edit, "late map").is_err());
        // A foreign retry cannot disturb even a stopped proposal's diagnostics.
        let mut other = Document::<&str>::default();
        let foreign = other.begin([2; 32], [Stage::Rails]);
        document.fail(&retry, "Still blocked".into()).unwrap();
        assert!(document.retry(&foreign).is_err());
        assert_eq!(document.issue(), Some("Still blocked"));
        let final_attempt = document.retry(&retry).unwrap();
        finish(&mut document, &final_attempt, "rebuilt map");
        assert_eq!(document.accepted().unwrap().inputs, [2; 32]);
        assert!(document.can_edit(document.accepted().unwrap(), &[2; 32]));
        assert_eq!(displayed.world, "accepted map");
    }
}
