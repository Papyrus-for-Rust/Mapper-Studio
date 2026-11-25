use super::*;
use mapper_studio::{prefab_assets::Catalog, prefab_textures::Bindings, rail_generation::Recipe};
use std::sync::Arc;

#[derive(Clone)]
struct Key {
    project: Arc<Project>,
    root: PathBuf,
    catalog: String,
    bindings: String,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.catalog == other.catalog
            && self.bindings == other.bindings
            && (Arc::ptr_eq(&self.project, &other.project) || self.project == other.project)
    }
}
impl Key {
    fn same_base(&self, other: &Self) -> bool {
        if self.root != other.root || self.catalog != other.catalog || self.bindings != other.bindings {
            return false;
        }
        if self.project == other.project {
            return true;
        }
        let mut project = self.project.as_ref().clone();
        project.rails.clone_from(&other.project.rails);
        project == *other.project
    }
}
struct Inputs {
    key: Key,
    catalog: Arc<Catalog>,
    bindings: Arc<Bindings>,
}
#[derive(Default)]
pub(super) struct RailPanel {
    prepared: Option<mapper_studio::client_preparation::ReadyRails>,
    prepared_attempt: Option<Key>,
    inputs: Option<Inputs>,
    ready: Option<(Key, Recipe)>,
    remembered: Option<(Key, Recipe)>,
    message: String,
}
impl RailPanel {
    pub(super) fn ready_for_import(&self, project: &Project) -> Option<mapper_studio::rail_generation::Recipe> {
        let inputs = self.inputs.as_ref()?;
        self.prepared.as_ref()?.bind(project, &inputs.catalog, &inputs.bindings).ok()
    }
    pub fn accept_prepared(
        &mut self,
        prepared: Option<mapper_studio::client_preparation::ReadyRails>,
    ) {
        self.prepared = prepared;
        self.prepared_attempt = None;
        self.ready = None;
        self.remembered = None;
    }
    pub fn tick(
        &mut self,
        project: &Project,
        root: Option<PathBuf>,
        catalog: Option<Arc<Catalog>>,
        bindings: Option<Arc<Bindings>>,
        _ctx: &egui::Context,
    ) {
        self.inputs = root
            .zip(catalog)
            .zip(bindings)
            .filter(|((root, catalog), bindings)| {
                mapper_studio::native_preview::is_native(&project.preview_engine)
                    && project.composition_engine.is_some()
                    && project.monument_rules.is_some()
                    && project.roads.is_some()
                    && bindings.installation_root() == root
                    && bindings.catalog_sha256() == catalog.sha256
                    && project.installation.as_ref() == Some(&catalog.installation)
                    && project
                        .check_monument_assets(&catalog.sha256, &bindings.sha256)
                        .is_ok()
            })
            .map(|((root, catalog), bindings)| {
                if let Some(mut inputs) = self.inputs.take().filter(|inputs| {
                    inputs.key.root == root
                        && inputs.key.catalog == catalog.sha256
                        && inputs.key.bindings == bindings.sha256
                        && inputs.key.project.as_ref() == project
                }) {
                    // Keep current asset handles even when their verified identities match.
                    inputs.catalog = catalog;
                    inputs.bindings = bindings;
                    inputs
                } else {
                    Inputs {
                        key: Key {
                            project: Arc::new(project.clone()),
                            root,
                            catalog: catalog.sha256.clone(),
                            bindings: bindings.sha256.clone(),
                        },
                        catalog,
                        bindings,
                    }
                }
            });
        if self.inputs.is_none() {
            self.poll_key(None);
            self.prepared_attempt = None;
            self.message.clear();
        } else if self.inputs.as_ref().is_some_and(|inputs| {
            self.prepared_attempt.as_ref() != Some(&inputs.key)
        }) {
            self.refresh(project);
        }
    }
    fn refresh(&mut self, project: &Project) {
        let Some(inputs) = &mut self.inputs else {
            return;
        };
        if inputs.key.project.as_ref() == project
            && self.prepared_attempt.as_ref() == Some(&inputs.key)
        {
            return;
        }
        if inputs.key.project.as_ref() != project {
            inputs.key.project = Arc::new(project.clone());
        }
        let key = inputs.key.clone();
        if let Some((before, recipe)) = &self.ready {
            self.remembered = Some((before.clone(), recipe.clone()));
        }
        self.poll_key(Some(&key));
        self.prepared_attempt = Some(key.clone());
        if self.ready.is_some() {
            return;
        }
        let Some(inputs) = &self.inputs else {
            return;
        };
        let Some(prepared) = &self.prepared else {
            self.message =
                "Rail data is unavailable for this installation. See installation loading details."
                    .into();
            return;
        };
        let mut candidate = project.clone();
        if candidate.rails.is_none()
            && let Some((remembered, recipe)) = &self.remembered
            && remembered.root == key.root
            && remembered.catalog == key.catalog
            && remembered.bindings == key.bindings
            && remembered.project.roads.as_ref().map(|r| &r.source_sha256)
                == project.roads.as_ref().map(|r| &r.source_sha256)
        {
            candidate.rails = Some(recipe.clone());
        }
        match prepared.bind(
            &candidate,
            &inputs.catalog,
            &inputs.bindings,
        ) {
            Ok(recipe) => {
                self.remembered = Some((key.clone(), recipe.clone()));
                self.ready = Some((key, recipe));
                self.message.clear();
            }
            Err(error) => self.message = error,
        }
    }
    fn poll_key(&mut self, key: Option<&Key>) {
        if self.ready.as_ref().is_some_and(|(ready, recipe)| {
            key.is_none_or(|key| {
                !ready.same_base(key)
                    || key.project.rails.as_ref().is_some_and(|r| {
                        r.source_sha256 != recipe.source_sha256
                            || r.mesh_sha256 != recipe.mesh_sha256
                    })
            })
        }) {
            self.ready = None;
        }
        if key.is_none() {
            self.remembered = None;
        }
    }
    fn toggle(&mut self, project: &mut Project, enabled: bool) -> Result<(), String> {
        let mut next = project.clone();
        if enabled {
            let (key, recipe) = self
                .ready
                .as_ref()
                .ok_or("Rail data is unavailable for this recipe")?;
            let mut before = key.project.as_ref().clone();
            before.rails = None;
            let mut current = project.clone();
            current.rails = None;
            if before != current {
                return Err("Rail inputs changed before binding. Try enabling rails again.".into());
            }
            next.rails = Some(recipe.clone());
        } else {
            if let Some(recipe) = &project.rails
                && let Some((_, ready)) = &mut self.ready
            {
                *ready = recipe.clone();
            }
            next.rails = None;
        }
        next.validate()?;
        *project = next;
        self.message.clear();
        Ok(())
    }
    pub fn show(&mut self, ui: &mut egui::Ui, project: &mut Project, reveal: bool) {
        let native = mapper_studio::native_preview::is_native(&project.preview_engine);
        let enabled = project.rails.is_some();
        let (mut header, changed) = super::panels::recipe_section(
            ui, "Rails", enabled, enabled || (native && self.ready.is_some()), false,
            "Generate surface rails. Surface rails need roads and compatible installation data.",
        );
        if let Some(enabled) = changed
            && let Err(error) = self.toggle(project, enabled)
        {
            self.message = error;
        }
        if reveal {
            header.set_open(true);
        }
        let (toggle, _) = header.body(|ui| self.rail_settings(ui, project, reveal));
        if reveal {
            toggle.scroll_to_me(Some(egui::Align::Min));
        }
    }

    fn rail_settings(&mut self, ui: &mut egui::Ui, project: &mut Project, reveal: bool) {
        if !mapper_studio::native_preview::is_native(&project.preview_engine) {
            ui.weak("Rails require native terrain");
            return;
        }
        self.refresh(project);
        if self.inputs.is_none() {
            ui.small("Enable roads and terrain composition to use surface rails.");
        }
        if !self.message.is_empty() {
            ui.small(&self.message);
        }
        if let Some(recipe) = &project.rails {
            let mut next = recipe.clone();
            let ring = recipe.settings.get("main_ring").and_then(serde_json::Value::as_bool);
            if let Some(change) = super::panels::generation_override(
                ui, "Ring rail", ring,
                "Close the main rail line into a loop.",
                "Generate the main rail line without closing the loop.",
                "Use the generator default, which enables ring rail.",
            )
                && let Some(settings) = next.settings.as_object_mut()
            {
                if let Some(enabled) = change {
                    settings.insert("main_ring".into(), enabled.into());
                } else {
                    settings.remove("main_ring");
                }
            }
            let settings = |ui: &mut egui::Ui| {
                for (label, path) in [
                    ("Ring seed", "/ring/seed"),
                    ("Approach seed", "/approaches/seed"),
                    ("Branch seed", "/bed/branching/seed"),
                    ("Siding seed", "/bed/siding/seed"),
                ] {
                    let mut value = next
                        .settings
                        .pointer(path)
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(label);
                        if ui
                            .add(egui::DragValue::new(&mut value).range(0..=u32::MAX))
                            .changed()
                        {
                            *next.settings.pointer_mut(path).unwrap() = value.into();
                        }
                    });
                }
                ui.separator();
                for (label, path) in [
                    ("Ring grade (%)", "/ring/maximum_grade"),
                    ("Approach grade (%)", "/approaches/maximum_grade"),
                    ("Branch grade (%)", "/bed/branching/maximum_grade"),
                    ("Bed grade (%)", "/bed/grade_fit/maximum_grade"),
                ] {
                    number(ui, &mut next.settings, label, path, 100., 100.);
                }
                number(
                    ui,
                    &mut next.settings,
                    "Maximum cut/fill (m)",
                    "/bed/maximum_cut_fill",
                    1.,
                    64.,
                );
            };
            egui::CollapsingHeader::new("Rail settings")
                .open(reveal.then_some(true))
                .show(ui, settings);
            if next != *recipe {
                if let Err(error) = next.validate() {
                    self.message = error;
                } else {
                    if let Some((_, ready)) = &mut self.ready {
                        *ready = next.clone();
                    }
                    project.rails = Some(next);
                }
            }
        }
    }
}
fn number(
    ui: &mut egui::Ui,
    settings: &mut serde_json::Value,
    label: &str,
    path: &str,
    scale: f64,
    maximum: f64,
) {
    let Some(raw) = settings.pointer(path).and_then(serde_json::Value::as_f64) else {
        return;
    };
    let mut value = raw * scale;
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        if ui
            .add(
                egui::DragValue::new(&mut value)
                    .range(0. ..=maximum)
                    .speed(0.25),
            )
            .changed()
        {
            *settings.pointer_mut(path).unwrap() = serde_json::json!(value / scale);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mapper_studio::rail_generation;
    pub(super) fn fixture() -> (RailPanel, Project, Key, Recipe) {
        let project = Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            composition_engine: Some(papyrus_terrain::monument_composition::VERSION.into()),
            installation: Some(mapper_studio::installation::Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: mapper_studio::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            monument_assets: Some(mapper_studio::AssetIdentity {
                catalog_sha256: "b".repeat(64),
                bindings_sha256: "c".repeat(64),
            }),
            monument_rules: Some(mapper_studio::monument_source::Recipe {
                engine: mapper_studio::monument_source::VERSION.into(),
                source_sha256: "d".repeat(64),
                coastal_profile: None,
                tier_search: None,
                earthwork: None,
                coastal_normals: None,
                coastal_navigation: None,
                coastal_search: None,
                canonical_yaw: None,
                land_alignment: None,
                surface_modifiers: None,
                stage_rules: None,
                dungeon_source: None, offshore_source: None,
            }),
            roads: Some(mapper_studio::infrastructure::Recipe::new("e".repeat(64))),
            ..Default::default()
        };
        project.validate().unwrap();
        let key = Key {
            project: Arc::new(project.clone()),
            root: "test installation".into(),
            catalog: "b".repeat(64),
            bindings: "c".repeat(64),
        };
        let recipe = Recipe {
            engine: rail_generation::VERSION.into(),
            source_sha256: "f".repeat(64),
            mesh_sha256: "a".repeat(64),
            settings: rail_generation::initial_settings(project.master_seed),
        };
        let panel = RailPanel {
            ready: Some((key.clone(), recipe.clone())),
            ..Default::default()
        };
        (panel, project, key, recipe)
    }
    fn frame(
        ctx: &egui::Context,
        panel: &mut RailPanel,
        project: &mut Project,
        history: &mut History,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let before = project.clone();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(250., 900.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                crate::panels::open_recipe_section(ui, "Rails");
                panel.show(ui, project, false);
            },
        );
        history.record(before, project);
        output.textures_delta.clear();
        output
    }
    fn click(
        ctx: &egui::Context,
        panel: &mut RailPanel,
        project: &mut Project,
        history: &mut History,
        label: &str,
    ) {
        let (row, label) = label.split_once('/').map_or((None, label), |(row, label)| (Some(row), label));
        let output = frame(ctx, panel, project, history, vec![]);
        let row_y = row.map(|row| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == row => Some(text.pos.y),
            _ => None,
        }).unwrap_or_else(|| panic!("Missing rail setting: {row}")));
        let pos = if label == "Rails" {
            crate::panels::recipe_checkbox_position(&output, label)
        } else { output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label
                    && row_y.is_none_or(|y| (t.pos.y - y).abs() < 4.) => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing rail control {label}")) };
        for pressed in [true, false] {
            frame(
                ctx,
                panel,
                project,
                history,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn enable_disable_and_undo_preserve_terrain_monuments_and_saved_settings() {
        let ctx = egui::Context::default();
        let (mut panel, mut project, key, recipe) = fixture();
        let before = project.clone();
        let mut history = History::default();
        click(
            &ctx,
            &mut panel,
            &mut project,
            &mut history,
            "Rails",
        );
        assert_eq!(project.rails, Some(recipe.clone()));
        assert_eq!(project.terrain_key(), before.terrain_key());
        assert_eq!(project.targets, before.targets);
        let mut active = key.clone();
        active.project = Arc::new(project.clone());
        panel.poll_key(Some(&active));
        assert!(panel.ready.is_some());
        history.undo(&mut project);
        assert_eq!(project, before);
        history.redo(&mut project);
        assert!(project.rails.is_some());
        project.rails.as_mut().unwrap().settings["ring"]["seed"] = 91.into();
        let edited = project.rails.clone();
        click(
            &ctx,
            &mut panel,
            &mut project,
            &mut history,
            "Rails",
        );
        assert!(project.rails.is_none());
        panel.poll_key(Some(&key));
        click(
            &ctx,
            &mut panel,
            &mut project,
            &mut history,
            "Rails",
        );
        assert_eq!(project.rails, edited);
        project.validate().unwrap();
    }
    #[test]
    fn stale_recipe_root_and_pin_results_cannot_enable_rails() {
        for change in ["recipe", "root", "pin"] {
            let (mut panel, project, key, _) = fixture();
            let mut changed = key;
            match change {
                "recipe" => Arc::make_mut(&mut changed.project).size = 5000,
                "root" => changed.root = "different installation".into(),
                _ => {
                    Arc::make_mut(&mut changed.project).rails = Some(Recipe {
                        engine: rail_generation::VERSION.into(),
                        source_sha256: "1".repeat(64),
                        mesh_sha256: "2".repeat(64),
                        settings: rail_generation::initial_settings(42),
                    });
                }
            }
            panel.poll_key(Some(&changed));
            assert!(panel.ready.is_none());
            let mut current = project.clone();
            assert!(panel.toggle(&mut current, true).is_err());
            assert_eq!(current, project);
        }
    }
    #[test]
    fn rail_controls_have_no_manual_preparation_action() {
        let ctx = egui::Context::default();
        let (mut panel, mut project, _, _) = fixture();
        let output = frame(
            &ctx,
            &mut panel,
            &mut project,
            &mut History::default(),
            vec![],
        );
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                assert!(!text.galley.job.text.contains("Prepare rail"));
                assert!(!text.galley.job.text.contains("Verify rail"));
                assert_ne!(text.galley.job.text, "Allow smaller maps");
            }
        }
    }
    #[test]
    fn same_frame_parent_changes_are_rejected_atomically() {
        let (mut panel, mut project, _, _) = fixture();
        project.roads.as_mut().unwrap().source_sha256 = "1".repeat(64);
        let before = project.clone();
        assert!(panel.toggle(&mut project, true).is_err());
        assert_eq!(project, before);
    }
    #[test]
    fn narrow_panel_wraps_controls_and_never_changes_loaded_rails_on_render() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.animation_time = 0.);
        let (mut panel, mut project, _, recipe) = fixture();
        project.rails = Some(recipe);
        let before = project.clone();
        let mut history = History::default();
        let mut revealed = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(250., 900.))),
                ..Default::default()
            },
            |ui| panel.show(ui, &mut project, true),
        );
        revealed.textures_delta.clear();
        let output = frame(&ctx, &mut panel, &mut project, &mut history, vec![]);
        assert_eq!(project, before);
        let mut labels = std::collections::BTreeSet::new();
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape {
                labels.insert(t.galley.job.text.as_str());
                assert!(
                    t.pos.x + t.galley.size().x <= 250.5,
                    "{} overflows the panel",
                    t.galley.job.text
                );
            }
        }
        for label in [
            "Ring rail",
            "Ring seed",
            "Approach seed",
            "Branch seed",
            "Siding seed",
            "Ring grade (%)",
            "Bed grade (%)",
            "Maximum cut/fill (m)",
        ] {
            assert!(labels.contains(label), "Missing {label}");
        }
        click(&ctx, &mut panel, &mut project, &mut history, "Ring rail/No");
        assert!(!project.rails.as_ref().unwrap().settings().unwrap().main_ring);
        click(&ctx, &mut panel, &mut project, &mut history, "Ring rail/?");
        let restored = project.rails.as_ref().unwrap();
        assert!(restored.settings.get("main_ring").is_none());
        assert!(restored.settings().unwrap().main_ring);
    }
}

#[cfg(test)]
#[path = "rail_panel/performance_tests.rs"]
mod performance_tests;
