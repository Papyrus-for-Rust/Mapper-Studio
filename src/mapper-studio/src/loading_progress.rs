//! Bounded live progress for background installation work. Never persisted in recipes.
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: u64,
    pub label: String,
    pub stage: String,
    pub item: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub unit: Unit,
    pub elapsed: Duration,
    pub can_cancel: bool,
    pub cancelling: bool,
}
impl Snapshot {
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.completed.min(t) as f64 / t as f64) as f32)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Items,
}
struct State {
    snapshot: Snapshot,
    started: Instant,
    active: bool,
    cancel: Option<Arc<AtomicBool>>,
}
#[derive(Clone)]
pub struct Reporter(Arc<Mutex<State>>);
static REGISTRY: OnceLock<Mutex<Vec<Weak<Mutex<State>>>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
thread_local! {static CURRENT:RefCell<Option<Reporter>>=const{RefCell::new(None)};}
/// The scope stays on its creating thread; reporters can cross into pipe readers.
pub struct Scope {
    reporter: Reporter,
    owner: bool,
    _thread: std::marker::PhantomData<Rc<()>>,
}
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(180)
        .collect()
}
pub fn begin(label: &str, cancel: Option<Arc<AtomicBool>>) -> Scope {
    if let Some(reporter) = CURRENT.with(|v| v.borrow().clone()) {
        reporter.stage(label);
        return Scope {
            reporter,
            owner: false,
            _thread: Default::default(),
        };
    }
    let reporter = Reporter(Arc::new(Mutex::new(State {
        snapshot: Snapshot {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            label: clean(label),
            stage: clean(label),
            item: String::new(),
            completed: 0,
            total: None,
            unit: Unit::Items,
            elapsed: Duration::ZERO,
            can_cancel: cancel.is_some(),
            cancelling: false,
        },
        started: Instant::now(),
        active: true,
        cancel,
    })));
    if let Ok(mut registry) = REGISTRY.get_or_init(Default::default).lock() {
        registry.retain(|r| {
            r.upgrade()
                .is_some_and(|s| s.lock().is_ok_and(|s| s.active))
        });
        if registry.len() < 64 {
            registry.push(Arc::downgrade(&reporter.0));
        }
    }
    CURRENT.with(|v| *v.borrow_mut() = Some(reporter.clone()));
    Scope {
        reporter,
        owner: true,
        _thread: Default::default(),
    }
}
impl Scope {
    pub fn reporter(&self) -> Reporter {
        self.reporter.clone()
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if self.owner {
            if let Ok(mut state) = self.reporter.0.lock() {
                state.active = false;
            }
            CURRENT.with(|v| *v.borrow_mut() = None);
        }
    }
}
impl Reporter {
    #[cfg(windows)]
    pub(crate) fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        self.0.lock().ok().and_then(|s| s.cancel.clone())
    }
    pub fn stage(&self, stage: &str) {
        if let Ok(mut s) = self.0.lock()
            && s.active
        {
            s.snapshot.stage = clean(stage);
            s.snapshot.item.clear();
            s.snapshot.completed = 0;
            s.snapshot.total = None;
        }
    }
    pub fn update(&self, item: &str, completed: u64, total: Option<u64>, unit: Unit) {
        if let Ok(mut s) = self.0.lock()
            && s.active
        {
            s.snapshot.item = clean(item);
            s.snapshot.completed = total.map_or(completed, |t| completed.min(t));
            s.snapshot.total = total.filter(|t| *t > 0);
            s.snapshot.unit = unit;
        }
    }
    /// Strict, small protocol emitted only by the bundled metadata helpers.
    pub fn structured_line(&self, line: &str) -> bool {
        let Some(payload) = line.strip_prefix("MAPPER_PROGRESS ") else {
            return false;
        };
        if payload.len() > 2048 {
            return true;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload)
            && v["schema"] == 1
            && let (Some(stage), Some(item)) = (v["stage"].as_str(), v["item"].as_str())
        {
            let done = v["completed"].as_u64().unwrap_or(0);
            let total = v["total"].as_u64();
            let unit = if v["unit"] == "bytes" {
                Unit::Bytes
            } else {
                Unit::Items
            };
            // Do not clear a file's counter on every update for that same file.
            if let Ok(mut s) = self.0.lock()
                && s.active
            {
                s.snapshot.stage = clean(stage);
                s.snapshot.item = clean(item);
                s.snapshot.completed = total.map_or(done, |t| done.min(t));
                s.snapshot.total = total.filter(|t| *t > 0);
                s.snapshot.unit = unit;
            }
        }
        true
    }
}
pub fn active() -> Vec<Snapshot> {
    active_with_priority(None).0
}

/// Active setup work sharing the same owner and cancellation signal.
pub fn active_for(token: &Arc<AtomicBool>) -> Vec<Snapshot> {
    collect(Some(token), true).0
}

/// Match the foreground worker by its cancellation token, independently of
/// registration order or the label emitted by its current stage.
pub fn active_with_priority(
    foreground: Option<&Arc<AtomicBool>>,
) -> (Vec<Snapshot>, Option<usize>) {
    collect(foreground, false)
}

fn collect(
    foreground: Option<&Arc<AtomicBool>>,
    only_matching: bool,
) -> (Vec<Snapshot>, Option<usize>) {
    let mut snapshots = Vec::new();
    let mut primary = None;
    if let Ok(mut registry) = REGISTRY.get_or_init(Default::default).lock() {
        registry.retain(|weak| {
            let Some(state) = weak.upgrade() else {
                return false;
            };
            if let Ok(s) = state.lock()
                && s.active
            {
                let matches = s.cancel.as_ref().zip(foreground)
                    .is_some_and(|(actual, expected)| Arc::ptr_eq(actual, expected));
                if only_matching && !matches {
                    return true;
                }
                let mut snapshot = s.snapshot.clone();
                snapshot.elapsed = s.started.elapsed();
                snapshot.cancelling = s.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
                if matches {
                    primary.get_or_insert(snapshots.len());
                }
                snapshots.push(snapshot);
                return true;
            }
            false
        });
    }
    (snapshots, primary)
}
pub fn cancel(id: u64) -> bool {
    if let Ok(registry) = REGISTRY.get_or_init(Default::default).lock() {
        for weak in registry.iter() {
            if let Some(state) = weak.upgrade()
                && let Ok(s) = state.lock()
                && s.active
                && s.snapshot.id == id
                && let Some(cancel) = &s.cancel
            {
                cancel.store(true, Ordering::Relaxed);
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreground_matches_the_task_token_after_the_stage_changes() {
        let selected = Arc::new(AtomicBool::new(false));
        let unrelated = Arc::new(AtomicBool::new(false));
        let scope = begin("Rust installation", Some(selected.clone()));
        let reporter = scope.reporter();
        reporter.stage("Checking Rust files");
        let (jobs, primary) = active_with_priority(Some(&selected));
        let primary = &jobs[primary.unwrap()];
        assert_eq!(primary.id, reporter.0.lock().unwrap().snapshot.id);
        assert_eq!(primary.stage, "Checking Rust files");
        assert_eq!(active_with_priority(Some(&unrelated)).1, None);
    }

    #[test]
    fn setup_group_tracks_only_workers_sharing_its_cancel_token() {
        let token = Arc::new(AtomicBool::new(false));
        let _parent = begin("Installation setup", Some(token.clone()));
        std::thread::scope(|threads| {
            let (ready, started) = std::sync::mpsc::channel();
            let (keep_open, finished) = std::sync::mpsc::channel::<()>();
            let prepare_token = token.clone();
            let notify = ready.clone();
            threads.spawn(move || {
                let _worker = begin("Monument data", Some(prepare_token));
                notify.send(()).unwrap();
                let _ = finished.recv();
            });
            let (keep_other_open, other_finished) = std::sync::mpsc::channel::<()>();
            threads.spawn(move || {
                let _worker = begin("Unrelated asset read", Some(Arc::new(AtomicBool::new(false))));
                ready.send(()).unwrap();
                let _ = other_finished.recv();
            });
            started.recv().unwrap();
            started.recv().unwrap();
            let jobs = active_for(&token);
            assert_eq!(jobs.iter().map(|job| job.label.as_str()).collect::<Vec<_>>(),
                ["Installation setup", "Monument data"]);
            drop((keep_open, keep_other_open));
        });
        assert_eq!(active_for(&token).len(), 1);
    }

    #[test]
    fn nested_stages_share_one_job_and_drop_clears_it_even_with_a_late_reporter() {
        let scope = begin("Installation test", None);
        let reporter = scope.reporter();
        let id = reporter.0.lock().unwrap().snapshot.id;
        {
            let child = begin("Verifying data", None);
            assert_eq!(child.reporter().0.lock().unwrap().snapshot.id, id);
            child
                .reporter()
                .update("large.bundle", 25, Some(100), Unit::Bytes);
            assert_eq!(
                active()
                    .into_iter()
                    .find(|s| s.id == id)
                    .unwrap()
                    .fraction(),
                Some(0.25)
            );
        }
        assert!(active().iter().any(|s| s.id == id));
        drop(scope);
        reporter.update("late output", 100, Some(100), Unit::Bytes);
        assert!(!active().iter().any(|s| s.id == id));
    }
    #[test]
    fn structured_updates_are_bounded_sanitized_and_cancel_the_original_worker() {
        let signal = Arc::new(AtomicBool::new(false));
        let scope = begin("Assets", Some(signal.clone()));
        let reporter = scope.reporter();
        assert!(!reporter.structured_line("unrelated stdout"));
        assert!(reporter.structured_line(r#"MAPPER_PROGRESS {"schema":1,"stage":"Reading textures","item":"file\nname","completed":20,"total":10,"unit":"items"}"#));
        let snapshot = active()
            .into_iter()
            .find(|s| s.id == reporter.0.lock().unwrap().snapshot.id)
            .unwrap();
        assert_eq!(snapshot.item, "file name");
        assert_eq!(snapshot.fraction(), Some(1.));
        assert!(cancel(snapshot.id));
        assert!(signal.load(Ordering::Relaxed));
    }
}
