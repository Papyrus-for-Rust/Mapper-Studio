//! Cancellable workbench binding; completed work cannot publish over a changed recipe.
use crate::{
    Project,
    object_generation::{self, Recipe},
    prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};
#[derive(Clone)]
struct Key {
    project: Arc<Project>,
    decoration: Option<crate::decor_source::Recipe>,
    root: PathBuf,
    catalog: String,
    bindings: String,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.catalog == other.catalog
            && self.bindings == other.bindings
            && self.decoration == other.decoration
            && (Arc::ptr_eq(&self.project, &other.project) || self.project == other.project)
    }
}
pub struct Inputs {
    key: Key,
    catalog: Arc<Catalog>,
    bindings: Arc<Bindings>,
}
impl Inputs {
    pub fn with_project(&self, project: &Project) -> Option<Self> {
        if self.key.project.as_ref() == project {
            return Some(Self {
                key: self.key.clone(),
                catalog: self.catalog.clone(),
                bindings: self.bindings.clone(),
            });
        }
        Self::candidate(
            project,
            self.catalog.clone(),
            self.bindings.clone(),
            self.key.decoration.clone(),
        )
    }
    pub fn new(project: &Project, catalog: Arc<Catalog>, bindings: Arc<Bindings>) -> Option<Self> {
        Self::candidate(project, catalog, bindings, None)
    }
    pub fn with_decoration(
        project: &Project,
        catalog: Arc<Catalog>,
        bindings: Arc<Bindings>,
        decoration: crate::decor_source::Recipe,
    ) -> Option<Self> {
        Self::candidate(project, catalog, bindings, Some(decoration))
    }
    /// Reuse validated inputs for the same request, retaining current asset handles.
    pub fn refresh(
        previous: Option<Self>,
        project: &Project,
        catalog: Arc<Catalog>,
        bindings: Arc<Bindings>,
        decoration: Option<&crate::decor_source::Recipe>,
    ) -> Option<Self> {
        if let Some(mut inputs) = previous.filter(|inputs| {
            inputs.key.root == bindings.installation_root()
                && inputs.key.catalog == catalog.sha256
                && inputs.key.bindings == bindings.sha256
                && inputs.key.decoration.as_ref() == decoration
                && project.installation.as_ref() == Some(&catalog.installation)
                && bindings.catalog_sha256() == catalog.sha256
                && inputs.key.project.as_ref() == project
        }) {
            inputs.catalog = catalog;
            inputs.bindings = bindings;
            Some(inputs)
        } else {
            Self::candidate(project, catalog, bindings, decoration.cloned())
        }
    }
    fn candidate(
        project: &Project,
        catalog: Arc<Catalog>,
        bindings: Arc<Bindings>,
        decoration: Option<crate::decor_source::Recipe>,
    ) -> Option<Self> {
        if project.roads.is_none()
            || decoration
                .as_ref()
                .or(project.decoration.as_ref())
                .is_none()
            || project.monument_rules.is_none()
            || project
                .check_monument_assets(&catalog.sha256, &bindings.sha256)
                .is_err()
            || project.installation.as_ref() != Some(&catalog.installation)
            || bindings.catalog_sha256() != catalog.sha256
        {
            return None;
        }
        if let Some(decoration) = &decoration {
            let mut validation = project.clone();
            validation.decoration = Some(decoration.clone());
            // The worker binds the replacement object's real source digest.
            validation.objects = Some(Recipe::new("0".repeat(64)));
            validation.validate().ok()?;
        } else {
            project.validate().ok()?;
        }
        Some(Self {
            key: Key {
                project: Arc::new(project.clone()),
                decoration,
                root: bindings.installation_root().to_owned(),
                catalog: catalog.sha256.clone(),
                bindings: bindings.sha256.clone(),
            },
            catalog,
            bindings,
        })
    }
}

struct Task {
    key: Key,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Recipe, String>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub struct Preparation {
    task: Option<Task>,
}
pub enum Event {
    Ready(Recipe),
    Failed(String),
    Changed,
}
impl Preparation {
    pub fn running(&self) -> bool {
        self.task.is_some()
    }
    pub fn cancel(&mut self) {
        self.task = None;
    }
    pub fn start(&mut self, inputs: Inputs, repaint: impl FnOnce() + Send + 'static) {
        let key = inputs.key.clone();
        self.start_notifying_job(
            key,
            move |signal| {
                let _verification = crate::installation::VerificationScope::default();
                let cancelled = || signal.load(Ordering::Relaxed);
                let mut project = inputs.key.project.as_ref().clone();
                if let Some(decoration) = &inputs.key.decoration {
                    project.decoration = Some(decoration.clone());
                }
                let parent = crate::infrastructure::Source::cached(
                    &inputs.catalog,
                    project.roads.as_ref().ok_or("Enable roads first")?,
                    cancelled,
                )?;
                let source = crate::object_extract::prepare_roads(
                    &inputs.key.root,
                    &parent,
                    project
                        .objects
                        .as_ref()
                        .map(|r| r.road_source_sha256.as_str()),
                    cancelled,
                )?;
                object_generation::bind(
                    &project,
                    &inputs.catalog,
                    &inputs.bindings,
                    source.sha256,
                    cancelled,
                )
            },
            repaint,
        );
    }
    pub fn discard_changed(&mut self, inputs: Option<&Inputs>) -> bool {
        if self
            .task
            .as_ref()
            .is_some_and(|t| inputs.map(|i| &i.key) != Some(&t.key))
        {
            self.cancel();
            return true;
        }
        false
    }
    #[cfg(test)]
    fn start_job(
        &mut self,
        key: Key,
        work: impl FnOnce(Arc<AtomicBool>) -> Result<Recipe, String> + Send + 'static,
    ) {
        self.start_notifying_job(key, work, || {});
    }
    fn start_notifying_job(
        &mut self,
        key: Key,
        work: impl FnOnce(Arc<AtomicBool>) -> Result<Recipe, String> + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
    ) {
        self.cancel();
        let signal = Arc::new(AtomicBool::new(false));
        let worker_signal = signal.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = work(worker_signal);
            let _ = sender.send(result);
            notify();
        });
        self.task = Some(Task {
            key,
            cancel: signal,
            receiver,
        });
    }
    pub fn poll(&mut self, inputs: Option<&Inputs>) -> Option<Event> {
        self.poll_key(inputs.map(|i| &i.key))
    }
    fn poll_key(&mut self, key: Option<&Key>) -> Option<Event> {
        let task = self.task.as_ref()?;
        if key != Some(&task.key) {
            self.cancel();
            return Some(Event::Changed);
        }
        let result = match task.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(
                "Object preparation ended before returning a result; try enabling it again".into(),
            ),
        };
        self.cancel();
        Some(match result {
            Ok(recipe) => Event::Ready(recipe),
            Err(error) => Event::Failed(error),
        })
    }
}
#[cfg(test)]
#[path = "object_preparation/input_tests.rs"]
mod input_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> Key {
        Key {
            project: Arc::new(Project::default()),
            decoration: None,
            root: "installation".into(),
            catalog: "a".repeat(64),
            bindings: "b".repeat(64),
        }
    }
    fn finished(state: &mut Preparation, key: &Key) -> Event {
        for _ in 0..1000 {
            if let Some(event) = state.poll_key(Some(key)) {
                return event;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("preparation did not finish");
    }
    #[test]
    fn changed_requested_decoration_discards_a_completed_binding() {
        let mut state = Preparation::default();
        let mut before = key();
        before.decoration = Some(crate::decor_source::Recipe::new("c".repeat(64)));
        state.start_job(before.clone(), |_| Ok(Recipe::new("d".repeat(64))));
        let mut after = before.clone();
        after.decoration = Some(crate::decor_source::Recipe::new("e".repeat(64)));
        assert!(matches!(state.poll_key(Some(&after)), Some(Event::Changed)));
        assert!(!state.running());
        assert!(state.poll_key(Some(&before)).is_none());
    }
    #[test]
    fn cancelled_or_replaced_work_cannot_publish_and_signals_its_worker() {
        let mut state = Preparation::default();
        let k = key();
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        state.start_job(k.clone(), move |signal| {
            ready_tx.send(signal.clone()).unwrap();
            rx.recv().unwrap();
            Ok(Recipe::new("c".repeat(64)))
        });
        let signal = ready_rx.recv().unwrap();
        let mut changed = k.clone();
        Arc::make_mut(&mut changed.project).shuffle_monuments();
        assert!(matches!(
            state.poll_key(Some(&changed)),
            Some(Event::Changed)
        ));
        assert!(signal.load(Ordering::Relaxed));
        assert!(!state.running());
        tx.send(()).unwrap();
        assert!(state.poll_key(Some(&k)).is_none());
        state.start_job(k.clone(), |_| Ok(Recipe::new("d".repeat(64))));
        state.cancel();
        assert!(state.poll_key(Some(&k)).is_none());
    }
    #[test]
    fn notifications_follow_delivery_and_replaced_jobs_cannot_win() {
        let mut state = Preparation::default();
        let k = key();
        let (notify_tx, notify_rx) = mpsc::channel();
        state.start_notifying_job(
            k.clone(),
            |_| Err("verification failed".into()),
            move || {
                notify_tx.send(()).unwrap();
            },
        );
        notify_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            matches!(state.poll_key(Some(&k)),Some(Event::Failed(e)) if e=="verification failed")
        );
        let (release_tx, release_rx) = mpsc::channel();
        state.start_job(k.clone(), move |_| {
            release_rx.recv().unwrap();
            Ok(Recipe::new("c".repeat(64)))
        });
        state.start_job(k.clone(), |_| Ok(Recipe::new("d".repeat(64))));
        release_tx.send(()).unwrap();
        assert!(
            matches!(finished(&mut state,&k),Event::Ready(r) if r.road_source_sha256=="d".repeat(64))
        );
        assert!(state.poll_key(Some(&k)).is_none());
    }
    #[test]
    fn success_errors_and_new_jobs_are_delivered_once_without_mutating_the_recipe() {
        let mut state = Preparation::default();
        let k = key();
        let original = k.project.clone();
        state.start_job(k.clone(), |_| Ok(Recipe::new("c".repeat(64))));
        let Event::Ready(recipe) = finished(&mut state, &k) else {
            panic!("missing recipe")
        };
        assert_eq!(recipe.road_source_sha256, "c".repeat(64));
        assert!(state.poll_key(Some(&k)).is_none());
        assert_eq!(k.project, original);
        state.start_job(k.clone(), |_| Err("missing coverage".into()));
        assert!(matches!(finished(&mut state,&k),Event::Failed(e) if e=="missing coverage"));
        state.start_job(k.clone(), |_| Ok(Recipe::new("d".repeat(64))));
        let mut moved = k.clone();
        moved.root = "other installation".into();
        assert!(matches!(state.poll_key(Some(&moved)), Some(Event::Changed)));
    }
}
