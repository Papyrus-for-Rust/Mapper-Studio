use super::*;
use mapper_studio::{
    AssetIdentity,
    monument_source::{Prepared, Recipe, Source},
    prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use std::sync::{
    Arc, Weak,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
struct Task {
    key: [String; 2],
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Source>, String>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct MonumentPanel {
    key: Option<[String; 2]>,
    rule_recipe: Option<mapper_studio::monument_source::Recipe>,
    task: Option<Task>,
    pub source: Option<Arc<Source>>,
    pub prepared: Option<Arc<Prepared>>,
    applied_defaults: Option<(Weak<Source>, Recipe)>,
    message: String,
    models: Option<Arc<Source>>,
    model_binding: Option<[String; 2]>,
    model_task: Option<Task>,
    model_attempt_rules: Option<String>,
    model_attempted: bool,
    model_message: String,
}
impl MonumentPanel {
    pub fn accept_prepared(&mut self, project: &Project, catalog: &Catalog, bindings: &Bindings, prepared: Prepared) {
        if project.monument_rules.as_ref().is_some_and(|rules| rules.source_sha256 != prepared.source().sha256) { return; }
        self.task = None;
        let key = [catalog.sha256.clone(), bindings.sha256.clone()];
        self.bind_model_assets(Some(&key));
        self.key = Some(key);
        self.rule_recipe = project.monument_rules.clone();
        self.set_rules_source(prepared.source().clone());
        self.prepared = Some(Arc::new(prepared));
        self.message.clear();
    }

    fn clear_models(&mut self) {
        self.models = None;
        self.model_task = None;
        self.model_attempt_rules = None;
        self.model_attempted = false;
        self.model_message.clear();
    }

    fn bind_model_assets(&mut self, key: Option<&[String; 2]>) {
        if let Some(key) = key && self.model_binding.as_ref() != Some(key) {
            self.clear_models();
            self.model_binding = Some(key.clone());
        }
    }

    fn set_rules_source(&mut self, source: Arc<Source>) {
        if self.models.as_ref().is_some_and(|models| !source.matches_placement_data(models)) {
            self.clear_models();
        }
        if self.models.is_none() && self.model_task.is_none()
            && self.model_attempt_rules.as_ref().is_some_and(|sha| *sha != source.sha256)
        {
            self.model_attempted = false;
            self.model_message.clear();
        }
        self.source = Some(source);
    }
    pub fn roster_readiness(&self) -> Result<(), String> {
        if self.task.is_some() || self.key.is_none() {
            return Ok(());
        }
        if self.source.as_ref().is_some_and(|s| s.stage_source.is_some()) {
            return Ok(());
        }
        if !self.message.is_empty() {
            return Err(self.message.clone());
        }
        Err("This Rust installation needs a tested monument-generation adapter. Choose a supported installation in Settings.".into())
    }
    pub fn populate_installation(&self, project: &mut Project, catalog: &Catalog) -> Result<usize, String> {
        self.populate_installation_targets(project, catalog, false)
    }
    fn populate_installation_targets(&self, project: &mut Project, catalog: &Catalog, landmarks_only: bool) -> Result<usize, String> {
        let source = self.source.as_ref().ok_or("Installation monument rules are still loading")?;
        let current_stages = source.stage_source.as_ref().ok_or("This installation needs a tested generation-scene adapter before its roster can be populated")?;
        let mut next = project.clone();
        let pins = self.asset_identity().ok_or("Monument asset bindings are not ready")?;
        if next.monument_rules.is_none() { Self::bind_source(&mut next, source, &pins)?; }
        else if next.monument_assets.as_ref()!=Some(&pins)
            || !mapper_studio::monument_source::supported(&next.placement_engine) {
            return Err("Choose matching installation monument placement before refreshing its roster".into());
        }
        let binding=next.monument_rules.as_ref().and_then(|r|r.stage_rules.as_ref())
            .ok_or("Enable the saved installation stage rules before refreshing its roster")?;
        let saved_stages;
        let stages=if binding.source_sha256==current_stages.sha256 {current_stages}
            else {saved_stages=mapper_studio::monument_stages::Source::cached_metadata(binding,catalog,||false)?;&saved_stages};
        let added = if landmarks_only {
            mapper_studio::placement_preview::populate_requested_landmarks(&mut next, catalog, stages)?
        } else {
            mapper_studio::placement_preview::populate_stage_targets(&mut next, catalog, stages)?
        };
        *project = next;
        Ok(added)
    }
    pub fn refresh_saved_roster(&self, project: &mut Project, catalog: Option<&Catalog>) -> Result<(), String> {
        let has_roster = project.monument_rules.as_ref()
            .and_then(|rules| rules.stage_rules.as_ref())
            .is_some_and(|rules| rules.population.is_some());
        let needs_landmarks = project.performance.monuments && project.options.requests_landmarks()
            && self.source.as_ref().is_some_and(|source| source.stage_source.is_some());
        if (has_roster || needs_landmarks) && let Some(catalog) = catalog {
            self.populate_installation_targets(project, catalog, !has_roster)
                .map_err(|error| format!("Cannot refresh installation roster: {error}"))?;
        }
        Ok(())
    }
    fn asset_identity(&self) -> Option<mapper_studio::AssetIdentity> {
        self.key.as_ref().map(|k| mapper_studio::AssetIdentity {
            catalog_sha256: k[0].clone(), bindings_sha256: k[1].clone(),
        })
    }
    fn bind_source(project: &mut Project, source: &Source, pins: &mapper_studio::AssetIdentity) -> Result<(), String> {
        if !mapper_studio::native_preview::is_native(&project.preview_engine) {
            return Err("Installation monument placement requires installed Rust terrain".into());
        }
        let mut next = project.clone();
        next.check_monument_assets(&pins.catalog_sha256, &pins.bindings_sha256)?;
        next.monument_assets = Some(pins.clone());
        next.composition_engine=Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
        let rules = source.recipe();
        next.placement_engine = rules.engine.clone();
        next.monument_rules = Some(rules);
        next.validate()?;
        *project = next;
        Ok(())
    }
    pub fn apply_defaults(&mut self, project: &mut Project) {
        let (Some(source), Some([catalog, bindings])) = (&self.source, &self.key) else {
            return;
        };
        if !mapper_studio::native_preview::is_native(&project.preview_engine) {
            return;
        }
        let pins_match = project.monument_assets.as_ref().is_some_and(|saved| {
            saved.catalog_sha256 == *catalog && saved.bindings_sha256 == *bindings
        });
        if pins_match
            && self.applied_defaults.as_ref().is_some_and(|(previous, recipe)| {
                std::ptr::eq(previous.as_ptr(), Arc::as_ptr(source))
                    && project.monument_rules.as_ref() == Some(recipe)
            })
        {
            return;
        }
        if let Err(error) = project.check_monument_assets(catalog, bindings) {
            self.message = error;
            return;
        }
        let mut desired = source.recipe();
        if let Some(saved) = &project.monument_rules {
            desired.surface_modifiers.clone_from(&saved.surface_modifiers);
            desired.land_alignment.clone_from(&saved.land_alignment);
            if let Some(policy) = &saved.coastal_search {
                desired.coastal_search = Some(policy.clone());
            }
            if let (Some(binding), Some(previous)) = (&mut desired.stage_rules, &saved.stage_rules) {
                binding.population.clone_from(&previous.population);
            }
        }
        if !pins_match || project.monument_rules.as_ref() != Some(&desired) {
            let pins = AssetIdentity {
                catalog_sha256: catalog.clone(),
                bindings_sha256: bindings.clone(),
            };
            let mut next = project.clone();
            let result = Self::bind_source(&mut next, source, &pins).and_then(|()| {
                next.monument_rules = Some(desired);
                mapper_studio::generation_defaults::apply(&mut next);
                next.validate()
            });
            if let Err(error) = result {
                self.message = error;
                return;
            }
            *project = next;
            self.message.clear();
        }
        self.applied_defaults = project
            .monument_rules
            .clone()
            .map(|recipe| (Arc::downgrade(source), recipe));
    }
    pub fn apply_terrain_defaults(&mut self, project: &mut Project, catalog: Option<&Catalog>) {
        if project
            .monument_rules
            .as_ref()
            .and_then(|r| r.surface_modifiers.as_deref())
            == Some(mapper_studio::monument_source::SOURCE_PRIORITY_VERSION)
        {
            return;
        }
        let Some(source) = &self.source else {
            return;
        };
        let Some(catalog) = catalog else {
            return;
        };
        if project
            .monument_rules
            .as_ref()
            .is_none_or(|r| r.source_sha256 != source.sha256)
        {
            return;
        }
        match source.recipe_with_source_defaults(catalog) {
            Ok(rules)
                if rules.surface_modifiers.as_deref()
                    == Some(mapper_studio::monument_source::SOURCE_PRIORITY_VERSION) =>
            {
                project.monument_rules.as_mut().unwrap().surface_modifiers = rules.surface_modifiers;
            }
            Ok(_) => {}
            Err(error) => self.message = error,
        }
    }

    #[cfg(test)]
    fn current(&self, project: &Project, source: &Source) -> bool {
        let mut recipe=source.recipe();
        if let Some(binding) = &mut recipe.stage_rules {
            binding.population = project.monument_rules.as_ref().and_then(|r| r.stage_rules.as_ref()).and_then(|b| b.population.clone());
        }
        project.placement_engine==recipe.engine
            && project.monument_rules.as_ref().is_some_and(|r|
                r.engine==recipe.engine && r.source_sha256==source.sha256
                    && r.stage_rules==recipe.stage_rules
                    && r.canonical_yaw==recipe.canonical_yaw
                    && r.coastal_normals==recipe.coastal_normals
                    && r.coastal_navigation==recipe.coastal_navigation
                    && r.coastal_search.as_ref().map(|p|p.engine.as_str())==recipe.coastal_search.as_ref().map(|p|p.engine.as_str()))
            && self.asset_identity().is_some_and(|pins|project.monument_assets.as_ref()==Some(&pins))
            && project.composition_engine.as_deref().is_some_and(|e|
                if source.coastal_profile.is_some() {papyrus_terrain::monument_composition::water_aware(Some(e))}
                else {papyrus_terrain::monument_composition::supported(e)})
    }
    /// The policy belongs beside generation controls, independently of selection.
    pub fn workflow(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        if !mapper_studio::native_preview::is_native(&project.preview_engine) {return;}

        if self.source.as_ref().is_none_or(|source| source.stage_source.is_none()) {
            ui.small(if self.task.is_some() || self.key.is_none() {
                "Preparing installation monument metadata…"
            } else {
                "Monument rules unavailable"
            });
        }
        if self.source.is_none() && self.task.is_none() && !self.message.is_empty()
            && ui.button("Retry monument preparation").clicked() {self.key=None;self.message.clear();}
        if !self.message.is_empty() {ui.add(egui::Label::new(egui::RichText::new(&self.message).small()).truncate()).on_hover_text(&self.message);}
    }
    pub fn tick(
        &mut self,
        rules: Option<&mapper_studio::monument_source::Recipe>,
        catalog: Option<Arc<Catalog>>,
        bindings: Option<Arc<Bindings>>,
        root: Option<PathBuf>,
        want_models: bool,
        ctx: &egui::Context,
    ) {
        let key = catalog
            .as_ref()
            .zip(bindings.as_ref())
            .map(|(c, b)| [c.sha256.clone(), b.sha256.clone()]);
        self.bind_model_assets(key.as_ref());
        if key.is_some() && key == self.key && self.task.is_none()
            && self.source.as_ref().is_some_and(|source| rules.is_none_or(|rules| {
                rules.validate().is_ok() && rules.source_sha256 == source.sha256
                    && rules.coastal_profile == source.coastal_profile
                    && rules.stage_rules.as_ref().map(|s| &s.source_sha256)
                        == source.stage_source.as_ref().map(|s| &s.sha256)
                    && rules.dungeon_source.as_ref().is_none_or(|binding|source.dungeon_source.as_ref()==Some(binding))
                    && rules.offshore_source.as_ref().is_none_or(|binding|source.offshore_source.as_ref()==Some(binding))
            }))
        {
            self.rule_recipe = rules.cloned();
        }
        if key != self.key || rules != self.rule_recipe.as_ref() {
            self.task = None;
            self.source = None;
            self.prepared = None;
            self.key = key.clone();
            self.rule_recipe = rules.cloned();
            self.message.clear();
            if let Some((catalog, bindings)) = catalog.clone().zip(bindings.clone()) {
                let (sender, receiver) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let repaint = ctx.clone();
                let task_key = key.unwrap();
                let root = root.clone();
                let rules = rules.cloned();
                std::thread::spawn(move || {
                    let _verification = mapper_studio::installation::VerificationScope::default();
            let _loading = mapper_studio::loading_progress::begin("Monument placement rules and models", Some(signal.clone()));
                    let cancelled = || signal.load(Ordering::Relaxed);
                    let result = if let Some(rules) = &rules {
                        Source::cached(rules, &catalog, &bindings, cancelled).map(Some)
                    } else {
                        Source::discover_rules(&catalog, &bindings, cancelled).and_then(|found| {
                            if found.is_none()
                                && let Some(root) = root
                            {
                                mapper_studio::monument_extract::prepare_rules(
                                    &root, &catalog, &bindings, cancelled,
                                )
                            } else {
                                Ok(found)
                            }
                        })
                    };
                    let result = result.and_then(|mut found| {
                        if let Some(source) = &mut found {
                            if source.coastal_profile.is_none() {
                                source.prepare_coastal(
                                    &catalog,
                                    bindings.installation_root(),
                                    cancelled,
                                )?;
                            }
                            if source.stage_source.is_none() {
                                source.prepare_stages(&catalog,bindings.installation_root(),cancelled)?;
                            }
                            source.dungeon_source = if let Some(binding) = &source.dungeon_source {
                                Some(mapper_studio::dungeon_source::Source::cached(&binding.source_sha256,bindings.installation_root(),&catalog,source,&cancelled)?.binding())
                            } else {
                                mapper_studio::dungeon_source::Source::prepare(bindings.installation_root(),&catalog,source,&cancelled)?.map(|s|s.binding())
                            };
                            source.offshore_source = if let Some(binding) = &source.offshore_source {
                                mapper_studio::offshore_generation::cached(binding,bindings.installation_root(),&catalog,&cancelled)?;
                                Some(binding.clone())
                            } else {
                                mapper_studio::offshore_generation::prepare(bindings.installation_root(),&catalog,&cancelled)?
                            };
                        }
                        Ok(found)
                    });
                    let _ = sender.send(result);
                    repaint.request_repaint();
                });
                self.task = Some(Task {
                    key: task_key,
                    cancel,
                    receiver,
                });
                self.message = "Preparing monument placement rules…".into();
            }
        }
        let result = self
            .task
            .as_ref()
            .and_then(|t| match t.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Monument preparation worker ended unexpectedly".into()))
                }
            });
        if let Some(result) = result {
            let task = self.task.take().unwrap();
            if Some(&task.key) == self.key.as_ref() && !task.cancel.load(Ordering::Relaxed) {
                match result {
                    Ok(Some(source)) => {
                        self.set_rules_source(Arc::new(source));
                        self.message.clear();
                    }
                    Ok(None) => {
                        self.message = "Monument source preparation cancelled or unavailable".into()
                    }
                    Err(error) => self.message = error,
                }
            }
        }
        self.tick_models(catalog, bindings, root, want_models, ctx);
    }
    pub fn model_source(&self) -> Option<&Arc<Source>> {
        let rules = self.source.as_ref()?;
        self.models.as_ref().or_else(|| rules.models_requested.then_some(rules))
    }
    pub fn model_status(&self) -> Option<&str> {
        (!self.model_message.is_empty()).then_some(self.model_message.as_str())
    }
    fn accept_models(&mut self, source: Source) -> Result<(), String> {
        if !source.models_requested || !self.source.as_ref().is_some_and(|rules| rules.matches_placement_data(&source)) {
            return Err("Preview models do not match the active monument placement data".into());
        }
        self.models = Some(Arc::new(source));
        Ok(())
    }
    fn tick_models(&mut self, catalog: Option<Arc<Catalog>>, bindings: Option<Arc<Bindings>>, root: Option<PathBuf>, want: bool, ctx: &egui::Context) {
        // A hidden viewport changes demand, not the lifetime of a source load.
        // Keep its receiver until the current placement rules can verify it.
        if self.source.is_some()
            && let Some(result) = self.model_task.as_ref().and_then(|t| match t.receiver.try_recv() {
            Ok(result) => Some(result), Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("Preview model worker ended unexpectedly".into())),
        }) {
            let task = self.model_task.take().unwrap();
            if Some(&task.key) == self.key.as_ref() {
                self.model_message = if task.cancel.load(Ordering::Relaxed) {
                    "3D monument model loading cancelled".into()
                } else { match result {
                    Ok(Some(source)) => self.accept_models(source).err().unwrap_or_default(),
                    Ok(None) => "Preview model preparation cancelled or unavailable".into(),
                    Err(error) => error,
                }};
                if self.models.is_none()
                    && let Some(previous) = &self.model_attempt_rules
                    && self.source.as_ref().is_some_and(|rules| rules.sha256 != *previous)
                {
                    self.model_attempted = false;
                }
            }
        }
        if !want || self.task.is_some() || self.source.is_none() || self.model_source().is_some() || self.model_task.is_some() || self.model_attempted { return; }
        let (Some(catalog),Some(bindings),Some(root),Some(key))=(catalog,bindings,root,self.key.clone()) else { return; };
        let (sender,receiver)=mpsc::channel();
        let cancel=Arc::new(AtomicBool::new(false));let signal=cancel.clone();let repaint=ctx.clone();
        self.model_attempted=true;
        let rules = self.source.as_ref().unwrap().clone();
        self.model_attempt_rules = Some(rules.sha256.clone());
        std::thread::spawn(move || {
            let _verification = mapper_studio::installation::VerificationScope::default();
            let _loading = mapper_studio::loading_progress::begin("Monument placement rules and models", Some(signal.clone()));
            let cancelled=||signal.load(Ordering::Relaxed);
            let result=rules.discover_models_for(&catalog,&bindings,cancelled).and_then(|found| {
                if found.is_some() {Ok(found)} else {mapper_studio::monument_extract::prepare_models_for(&root,&catalog,&bindings,&rules,cancelled)}
            });
            let _=sender.send(result);repaint.request_repaint();
        });
        self.model_task=Some(Task{key,cancel,receiver});
        self.model_message="Preparing optional 3D monument models…".into();
    }
    pub fn show(&mut self, ui: &mut egui::Ui, _project: &mut Project) {
        ui.separator();
        ui.strong("Source monument checks and models");
        if let Some(source) = &self.source {
            if let Some(stages) = &source.stage_source {
                ui.small(format!(
                    "{} land placement rules · {} additional source families",
                    stages.program_count(),
                    stages.unsupported_family_count(),
                )).on_hover_text("Additional families fall outside these land placement rules.");
            }
            let anchors: usize = source.prefabs.values().map(|p| p.rules.anchors.len()).sum();
            let filters: usize = source.prefabs.values().map(|p| p.rules.filters.len()).sum();
            let model_source = self.model_source().map(|s| s.as_ref()).unwrap_or(source);
            let models = model_source
                .prefabs
                .values()
                .filter(|p| !p.model.triangles.is_empty())
                .count();
            ui.small(format!(
                "{anchors} anchors · {filters} local filters · {models}/{} static LOD previews",
                source.prefabs.len()
            ));
            let complete = model_source.prefabs.values().filter(|p| p.model.complete).count();
            ui.small(format!(
                "{complete} complete static previews; {} partial previews",
                models - complete
            ));
            ui.small("Anchors, terrain filters, spacing and coastal checks are automatic.");
            ui.small("Static models are untextured inspection LODs. Stage-wide filters, placement-map occupancy and physics remain unvalidated.");
        } else {
            ui.small(&self.message);
        }
        if self.source.is_some() && !self.message.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(&self.message).small()).truncate()).on_hover_text(&self.message);
        }
        if let Some(task) = &self.task {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading installation data…");
            });
            if ui.button("Cancel monument preparation").clicked() {
                task.cancel.store(true, Ordering::Relaxed);
            }
        } else if ui.button("Rescan monument source cache").clicked() {
            self.key = None;
        }
        if let Some(message) = self.model_status() { ui.small(message); }
        if let Some(task) = &self.model_task {
            if ui.button("Cancel preview model loading").clicked() {
                task.cancel.store(true,Ordering::Relaxed);
            }
        } else if self.source.is_some() && self.model_source().is_none()
            && ui.button("Retry 3D monument models").on_hover_text("Models load when the 3D view is open. Placement rules stay unchanged.").clicked() {
            self.model_attempted=false;
            self.model_message.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an owned installation and saved source-bound recipe"]
    fn owned_saved_monument_rules_load_without_rebinding_and_missing_pins_stay_explicit() {
        let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_RECIPE_TEST_ROOT");
        let _verification = mapper_studio::installation::VerificationScope::default();
        let installation = mapper_studio::installation::inspect(&root, || false).unwrap();
        let catalog = Arc::new(mapper_studio::prefab_assets::load(
            &root, &path("MAPPER_RECIPE_TEST_CATALOG"), &installation, || false,
        ).unwrap());
        let bindings = Arc::new(Bindings::load(
            &root, &path("MAPPER_RECIPE_TEST_BINDINGS"), &catalog, || false,
        ).unwrap());
        let saved = Project::load(&path("MAPPER_RECIPE_TEST_PROJECT")).unwrap();
        let saved_bytes = serde_json::to_vec(&saved).unwrap();
        let source_pin = saved.monument_rules.as_ref().unwrap().source_sha256.clone();
        let ctx = egui::Context::default();
        let mut panel = MonumentPanel::default();
        for missing in [false, true, false] {
            let mut project = saved.clone();
            if missing {
                project.monument_rules.as_mut().unwrap().source_sha256 = "f".repeat(64);
            }
            let before = project.clone();
            let limit = std::time::Instant::now() + std::time::Duration::from_secs(180);
            loop {
                panel.tick(project.monument_rules.as_ref(), Some(catalog.clone()),
                    Some(bindings.clone()), Some(root.clone()), false, &ctx);
                if panel.task.is_none() { break; }
                assert!(std::time::Instant::now() < limit, "Saved monument preparation timed out");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if missing {
                assert!(panel.source.is_none());
                assert!(!panel.message.is_empty());
            } else {
                assert!(panel.message.is_empty(), "{}", panel.message);
                assert_eq!(panel.source.as_ref().unwrap().sha256, source_pin);
            }
            panel.apply_defaults(&mut project);
            assert_eq!(project, before);
            if !missing { assert_eq!(serde_json::to_vec(&project).unwrap(), saved_bytes); }
        }
        std::fs::write(path("MAPPER_RECIPE_TEST_REPORT"), serde_json::to_vec_pretty(
            &serde_json::json!({"format":"mapper-owned-saved-monument-rules-verification",
                "schema":1,"source_sha256":source_pin,"saved_recipe_preserved":true,
                "missing_pin_rejected_without_fallback":true,"restored_pin_reloaded":true})
        ).unwrap()).unwrap();
    }
    #[test]
    fn roster_readiness_distinguishes_loading_missing_adapter_and_worker_failure() {
        let mut panel = MonumentPanel::default();
        assert!(panel.roster_readiness().is_ok());
        panel = fixture(false).0;
        assert!(panel.roster_readiness().unwrap_err().contains("tested monument-generation adapter"));
        panel.message = "Monument metadata receipt mismatch".into();
        assert_eq!(panel.roster_readiness().unwrap_err(), panel.message);
        let (_sender, receiver) = mpsc::channel();
        panel.task = Some(Task {
            key: panel.key.clone().unwrap(),
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
        });
        assert!(panel.roster_readiness().is_ok());
    }
    #[test]
    fn unavailable_monument_rules_do_not_render_as_loading_or_ready() {
        let (mut panel, mut project) = fixture(false);
        let ctx = egui::Context::default();
        let output = frame(&ctx, &mut panel, &mut project, false, vec![]);
        let text = output.shapes.into_iter().filter_map(|shape| match shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
            _ => None,
        }).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Monument rules unavailable"), "{text}");
        assert!(!text.contains("Preparing installation monument metadata"));
        assert!(!text.contains("Installation placement rules are automatic"));
    }
    #[test]
    fn optional_model_completion_keeps_active_rules_and_project_snapshot_unchanged() {
        let (mut panel,mut project)=fixture(false);
        Arc::get_mut(panel.source.as_mut().unwrap()).unwrap().models_requested=false;
        MonumentPanel::bind_source(&mut project,panel.source.as_ref().unwrap(),&panel.asset_identity().unwrap()).unwrap();
        let saved=project.clone();let rules=panel.source.as_ref().unwrap().clone();
        let (sender,receiver)=mpsc::channel();let cancel=Arc::new(AtomicBool::new(false));
        panel.model_task=Some(Task{key:panel.key.clone().unwrap(),cancel,receiver});
        let (models,_)=fixture(false);
        let mut models=Arc::try_unwrap(models.source.unwrap()).ok().unwrap();models.sha256="e".repeat(64);
        sender.send(Ok(Some(models))).ok().unwrap();
        panel.tick_models(None,None,None,true,&egui::Context::default());
        assert!(Arc::ptr_eq(panel.source.as_ref().unwrap(),&rules));
        assert_eq!(panel.source.as_ref().unwrap().recipe(),saved.monument_rules.clone().unwrap());
        assert_eq!(panel.model_source().unwrap().sha256,"e".repeat(64));
        assert_eq!(project,saved);
    }
    #[test]
    fn switching_views_keeps_one_model_worker_and_reuses_its_result() {
        let (mut panel,project)=fixture(false);
        Arc::get_mut(panel.source.as_mut().unwrap()).unwrap().models_requested=false;
        let rules=panel.source.as_ref().unwrap().clone();let before=project.clone();
        let (sender,receiver)=mpsc::channel();let cancel=Arc::new(AtomicBool::new(false));
        panel.model_task=Some(Task{key:panel.key.clone().unwrap(),cancel:cancel.clone(),receiver});
        panel.model_attempted=true;
        let ctx = egui::Context::default();
        panel.tick_models(None,None,None,false,&ctx);
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&panel.model_task.as_ref().unwrap().cancel, &cancel));
        let (models, _) = fixture(false);
        sender.send(Ok(Some(Arc::try_unwrap(models.source.unwrap()).ok().unwrap()))).unwrap();
        panel.tick_models(None,None,None,false,&ctx);
        let loaded = panel.model_source().unwrap().clone();
        let (replacement, _) = fixture(false);
        panel.set_rules_source(replacement.source.unwrap());
        panel.tick_models(None,None,None,true,&ctx);
        assert!(panel.model_task.is_none());
        assert!(Arc::ptr_eq(panel.model_source().unwrap(), &loaded));
        panel.set_rules_source(rules.clone());
        assert!(Arc::ptr_eq(panel.source.as_ref().unwrap(),&rules));assert_eq!(project,before);
    }
    #[test]
    fn invalid_or_stale_optional_model_result_is_not_attached() {
        for stale in [false,true] {
            let(mut panel,_)=fixture(false);
            Arc::get_mut(panel.source.as_mut().unwrap()).unwrap().models_requested=false;
            let rules=panel.source.as_ref().unwrap().clone();
            let (sender,receiver)=mpsc::channel();
            panel.model_task=Some(Task{key:if stale {["x".repeat(64),"y".repeat(64)]}else{panel.key.clone().unwrap()},cancel:Arc::new(AtomicBool::new(false)),receiver});
            let (bad,_)=fixture(false);let mut bad=Arc::try_unwrap(bad.source.unwrap()).ok().unwrap();
            if !stale {bad.models_requested=false;}
            sender.send(Ok(Some(bad))).ok().unwrap();
            panel.tick_models(None,None,None,true,&egui::Context::default());
            assert!(panel.model_source().is_none());assert!(Arc::ptr_eq(panel.source.as_ref().unwrap(),&rules));
        }
    }
    fn roster_fixture() -> (MonumentPanel, Project, Catalog) {
        let (mut panel,mut project)=fixture(false);
        let doc:serde_json::Value=serde_json::from_str(include_str!("../tests/fixtures/monument-stage-source.json")).unwrap();
        let sources:Vec<mapper_studio::installation::SourceFile>=serde_json::from_value(doc["sources"].clone()).unwrap();
        let catalog=Catalog{sha256:"a".repeat(64),installation:mapper_studio::installation::Installation{
            kind:mapper_studio::installation::InstallationKind::Client,native:sources[0].clone(),scene_manifest:Some(sources[1].clone())},
            sources:sources[..3].to_vec(),entries:doc["prefab_stage_matches"].as_object().unwrap().keys().map(|p|(p.clone(),mapper_studio::prefab_assets::Asset{
                priority:None,bounds:None,yaw:None,modifier_size:None,modifier_offset:None,texture_guids:Default::default(),splat_mask:None,modifier:None,modifier_absent:false,water_geometry:None,no_build_geometry:None,display_name:None})).collect()};
        panel.key=Some([catalog.sha256.clone(),"c".repeat(64)]);
        project.installation=Some(catalog.installation.clone());
        let stages=mapper_studio::monument_stages::Source::decode(&serde_json::to_vec(&doc).unwrap(),&catalog).unwrap();
        Arc::get_mut(panel.source.as_mut().unwrap()).unwrap().stage_source=Some(stages);
        assert!(panel.roster_readiness().is_ok());
        MonumentPanel::bind_source(&mut project,panel.source.as_ref().unwrap(),&panel.asset_identity().unwrap()).unwrap();
        (panel, project, catalog)
    }
    #[test]
    fn roster_refresh_preserves_saved_model_snapshot_and_custom_policy() {
        let (panel, mut project, catalog) = roster_fixture();
        let saved=project.monument_rules.as_mut().unwrap();
        saved.source_sha256="d".repeat(64); // saved extractor revision, same asset receipts
        saved.canonical_yaw=None;
        let mut expected=project.clone();
        expected.monument_rules.as_mut().unwrap().stage_rules.as_mut().unwrap().population=Some(mapper_studio::monument_stages::POPULATION_VERSION.into());
        assert_eq!(panel.populate_installation(&mut project,&catalog).unwrap(),0);
        assert_eq!(project,expected);
    }
    #[test]
    fn refreshed_roster_is_shared_by_every_seeded_queue_recipe() {
        use mapper_studio::{generation_plan::BatchPlan, monument_stages::POPULATION_VERSION, prefab_assets::Bounds};

        let (panel, mut template, mut catalog) = roster_fixture();
        let path = "assets/bundled/prefabs/autospawn/monument/arctic_bases/test.prefab";
        catalog.entries.get_mut(path).unwrap().bounds = Some(Bounds {
            center: [0.; 3],
            extent: [12.; 3],
            minimum_world_size: 1000,
        });
        let binding = template.monument_rules.as_mut().unwrap().stage_rules.as_mut().unwrap();
        binding.population = Some(POPULATION_VERSION.into());
        let before = template.clone();
        let mut plan = BatchPlan {
            count: 3,
            random_master: true,
            ..BatchPlan::exact(template)
        };
        panel.refresh_saved_roster(&mut plan.recipe, Some(&catalog)).unwrap();
        assert_eq!(plan.recipe.targets.len(), before.targets.len() + 1);
        assert_eq!(plan.recipe.targets.last().unwrap().prefab_path.as_deref(), Some(path));

        let recipes = plan.recipes().unwrap();
        for recipe in &recipes {
            assert_eq!(recipe.targets, plan.recipe.targets);
            assert_ne!(recipe.master_seed, before.master_seed);
            let mut rechecked = recipe.clone();
            panel.refresh_saved_roster(&mut rechecked, Some(&catalog)).unwrap();
            assert_eq!(rechecked, *recipe);
        }
        assert_ne!(recipes[0].master_seed, recipes[1].master_seed);
    }
    fn fixture(coastal:bool)->(MonumentPanel,Project) {
        let source=Source{
            extractor_sha256: "a".repeat(64),
            profile_sha256: "a".repeat(64),
            stage_source:None,
            dungeon_source:None,offshore_source:None,
            code_sources:Vec::new(),
            sha256:"a".repeat(64),prefabs:Default::default(),models_requested:true,
            coastal_profile:coastal.then(||serde_json::from_str(include_str!("../../../compatibility/monuments/client-0f703d43-coastal.json")).unwrap()),
        };
        let panel=MonumentPanel{key:Some(["b".repeat(64),"c".repeat(64)]),source:Some(Arc::new(source)),..Default::default()};
        let project=Project{
            preview_engine:mapper_studio::native_preview::VERSION.into(),
            composition_engine:None,
            installation:Some(mapper_studio::installation::Installation{
                kind:mapper_studio::installation::InstallationKind::Client,
                native:mapper_studio::installation::SourceFile{relative_path:"RustClient_Data/Plugins/x86_64/RustNative.dll".into(),sha256:"0f703d43b57672b8bb3dfd29bab0d484f8622fbac66dee7f1f058893ef22a99c".into(),bytes:1737552},scene_manifest:None}),
            ..Default::default()
        };
        project.validate().unwrap();(panel,project)
    }
    fn frame(ctx:&egui::Context,panel:&mut MonumentPanel,project:&mut Project,inspector:bool,events:Vec<egui::Event>)->egui::FullOutput {
        let mut out=ctx.run_ui(egui::RawInput{screen_rect:Some(Rect::from_min_size(Pos2::ZERO,Vec2::new(900.,1000.))),events,focused:true,..Default::default()},|ui|{
            if inspector {panel.show(ui,project);}else{panel.workflow(ui,project);}
        });out.textures_delta.clear();out
    }









    #[test]
    fn settled_monument_defaults_reuse_the_recipe_and_check_replacement_sources() {
        let (mut panel, mut project) = fixture(false);
        panel.apply_defaults(&mut project);
        let original = project.clone();
        let retained = panel.applied_defaults.as_ref().unwrap().1.source_sha256.as_ptr();
        panel.apply_defaults(&mut project);
        assert_eq!(panel.applied_defaults.as_ref().unwrap().1.source_sha256.as_ptr(), retained);
        assert_eq!(project, original);

        let (replacement, _) = fixture(true);
        panel.source = replacement.source;
        panel.apply_defaults(&mut project);
        assert!(project.monument_rules.as_ref().unwrap().coastal_profile.is_some());
        assert!(panel.current(&project, panel.source.as_ref().unwrap()));

        project.monument_assets.as_mut().unwrap().catalog_sha256 = "d".repeat(64);
        let saved = project.clone();
        panel.apply_defaults(&mut project);
        assert_eq!(project, saved);
        assert!(!panel.message.is_empty());
    }

    #[test]
    fn monument_rules_bind_automatically_and_cannot_be_disabled_by_old_flags() {
        for coastal in [false,true] {
            let(mut panel,mut project)=fixture(coastal);let seeds=project.seeds;let targets=project.targets.clone();
            panel.apply_defaults(&mut project);assert!(panel.current(&project,panel.source.as_ref().unwrap()));
            let rules=project.monument_rules.as_mut().unwrap();rules.canonical_yaw=None;rules.stage_rules=None;
            panel.apply_defaults(&mut project);assert!(project.monument_rules.as_ref().unwrap().canonical_yaw.is_some());
            assert_eq!(project.monument_rules.as_ref().unwrap().stage_rules.is_some(),panel.source.as_ref().unwrap().stage_source.is_some());assert_eq!(project.seeds,seeds);assert_eq!(project.targets,targets);
            let stable=project.clone();panel.apply_defaults(&mut project);assert_eq!(project,stable);
        }
    }
    #[test]
    fn automatic_monument_binding_rejects_mixed_assets_without_mutation() {
        let(mut panel,mut project)=fixture(true);project.monument_assets=Some(mapper_studio::AssetIdentity{catalog_sha256:"f".repeat(64),bindings_sha256:"e".repeat(64)});
        let before=project.clone();panel.apply_defaults(&mut project);assert_eq!(project,before);assert!(!panel.message.is_empty());
    }
    #[test]
    fn prepared_monument_controls_preserve_saved_surface_fitting_policy() {
        let ctx=egui::Context::default();
        for coastal in [false,true] {
            let(mut panel,mut project)=fixture(coastal);
            panel.apply_defaults(&mut project);
            assert_eq!(project.monument_rules.as_ref().unwrap().surface_modifiers.as_deref(),Some(mapper_studio::monument_source::SURFACE_MODIFIER_VERSION));
            for policy in [None,Some(mapper_studio::monument_source::LEGACY_SURFACE_MODIFIER_VERSION.into()),Some(mapper_studio::monument_source::SURFACE_MODIFIER_VERSION.into())] {
                project.monument_rules.as_mut().unwrap().surface_modifiers=policy;
                let saved=project.clone();let bytes=serde_json::to_vec(&saved).unwrap();
                for _ in 0..3 {
                    panel.apply_defaults(&mut project);
                    frame(&ctx,&mut panel,&mut project,false,vec![]);
                    frame(&ctx,&mut panel,&mut project,true,vec![]);
                    assert_eq!(project,saved);
                    assert_eq!(serde_json::to_vec(&project).unwrap(),bytes);
                }
            }
        }
    }
    #[test]
    fn monument_panel_contains_status_instead_of_algorithm_switches() {
        let ctx=egui::Context::default();let(mut panel,mut project)=fixture(true);panel.apply_defaults(&mut project);
        let output=frame(&ctx,&mut panel,&mut project,true,vec![]);
        for shape in output.shapes {if let egui::Shape::Text(t)=shape.shape {for forbidden in ["Apply installation stage filters","Stable rotation when regenerating monuments","Use authored terrain allowances","Retry tier-boundary placements","Use source anchors, filters and tiers"] {assert_ne!(t.galley.job.text,forbidden);}}}
    }

}
