use super::*;
use mapper_studio::plugins::{
    Capability, Control, Loaded, Outcome, Parameter, RegisteredPackage, Registry, Report, Severity,
};
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
use std::time::Duration;

struct Inspection {
    register: bool,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Loaded, String>>,
}
impl Drop for Inspection {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
struct Run {
    before: Project,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Outcome, String>>,
}
impl Drop for Run {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub(super) struct PluginPanel {
    registry: Option<Result<Registry, String>>,
    package: usize,
    plugin: Option<Arc<Loaded>>,
    inspection: Option<Inspection>,
    running: Option<Run>,
    selected: usize,
    parameters: String,
    values: Map<String, Value>,
    message: String,
    report: Option<(Project, Report)>,
    pub focus_requested: bool,
}
impl Default for PluginPanel {
    fn default() -> Self {
        Self {
            registry: None,
            package: 0,
            plugin: None,
            inspection: None,
            running: None,
            selected: 0,
            parameters: "{}".into(),
            values: Map::new(),
            message: String::new(),
            report: None,
            focus_requested: false,
        }
    }
}
impl PluginPanel {
    fn inspect(
        &mut self,
        ctx: &egui::Context,
        register: bool,
        load: impl FnOnce(&AtomicBool) -> Result<Loaded, String> + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(load(&signal));
            ctx.request_repaint();
        });
        self.inspection = Some(Inspection {
            register,
            cancel,
            receiver,
        });
        self.message.clear();
    }

    fn selected_package(&self) -> Option<&RegisteredPackage> {
        self.registry.as_ref()?.as_ref().ok()?.packages().get(self.package)
    }

    fn inspect_selected(&mut self, ctx: &egui::Context) {
        self.set_plugin(None);
        self.message.clear();
        if let Some(package) = self.selected_package().cloned() {
            self.inspect(ctx, false, move |cancel| {
                package.inspect(|| cancel.load(Ordering::Relaxed))
            });
        }
    }

    fn set_plugin(&mut self, plugin: Option<Loaded>) {
        self.plugin = plugin.map(Arc::new);
        self.selected = 0;
        self.values.clear();
        self.parameters = "{}".into();
        self.report = None;
    }

    pub fn cancel(&mut self) {
        if let Some(task) = &self.running {
            task.cancel.store(true, Ordering::Relaxed);
            self.message = "Cancelling plugin action…".into();
        }
    }
    /// Main applies the return value inside its normal one-frame undo transaction.
    pub fn poll(&mut self, project: &Project) -> Option<Project> {
        if let Some(task) = &self.inspection {
            let result = match task.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Plugin inspection ended unexpectedly".into()))
                }
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let cancelled = task.cancel.load(Ordering::Relaxed);
                let register = task.register;
                self.inspection = None;
                if !cancelled {
                    let result = result.and_then(|plugin| {
                        if register {
                            let registry = self.registry.as_mut().ok_or("Plugin list unavailable")?;
                            self.package = registry.as_mut().map_err(|e| e.as_str())?
                                .register(&plugin)?;
                        }
                        Ok(plugin)
                    });
                    match result {
                        Ok(plugin) => {
                            self.set_plugin(Some(plugin));
                            self.message.clear();
                        }
                        Err(error) => self.message = format!("Could not load plugin: {error}"),
                    }
                }
            }
        }
        let task = self.running.as_ref()?;
        let result = match task.receiver.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Plugin worker ended unexpectedly".into()),
        };
        let cancelled = task.cancel.load(Ordering::Relaxed);
        let recipe_changed = &task.before != project;
        self.running = None;
        if cancelled {
            self.message = "Plugin action cancelled.".into();
            return None;
        }
        if recipe_changed {
            self.message = "Plugin result discarded because the recipe changed.".into();
            return None;
        }
        match result {
            Ok(Outcome::Recipe(next)) => {
                self.message = if next.as_ref() == project {
                    "Plugin completed with no recipe changes."
                } else {
                    "Plugin applied. Undo restores the previous recipe."
                }
                .into();
                Some(*next)
            }
            Ok(Outcome::Report(report)) => {
                self.message.clear();
                self.report = Some((project.clone(), report));
                None
            }
            Err(error) => {
                self.message = format!("Plugin failed: {error}");
                None
            }
        }
    }
    fn packages(&mut self, ui: &mut egui::Ui, settings: Option<&Path>) {
        if self.registry.is_none() {
            let settings = settings.map(Path::to_path_buf)
                .map_or_else(mapper_studio::preferences::Preferences::path, Ok);
            self.registry = Some(settings.and_then(|path| Registry::load(path.with_file_name("plugins.json"))));
            self.inspect_selected(ui.ctx());
        }
        let Some(registry) = &mut self.registry else { return };
        let registry = match registry {
            Ok(registry) => registry,
            Err(error) => {
                ui.add(egui::Label::new("Could not open the plugin list.").truncate())
                    .on_hover_text(error.as_str());
                if ui.button("Retry").clicked() {
                    self.registry = None;
                }
                return;
            }
        };
        let busy = self.running.is_some() || self.inspection.is_some();
        let mut selection = self.package;
        let selected = registry.packages().get(selection);
        if let Some(package) = selected {
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("installed-plugins")
                    .width(ui.available_width())
                    .truncate()
                    .selected_text(&package.name)
                    .show_ui(ui, |ui| {
                        for (index, package) in registry.packages().iter().enumerate() {
                            ui.selectable_value(&mut selection, index, &package.name)
                                .on_hover_text(&package.id);
                        }
                    });
            });
        }
        let mut add = false;
        let mut remove = false;
        ui.horizontal_wrapped(|ui| {
            add = ui.add_enabled(!busy, egui::Button::new("Add plugin…"))
                .on_hover_text("Choose a plugin manifest from a source you trust.")
                .clicked();
            if selected.is_some() {
                remove = ui.add_enabled(!busy, egui::Button::new("Remove"))
                    .on_hover_text("Remove this entry from Mapper’s plugin list. Its files stay on disk.")
                    .clicked();
            }
        });
        if remove {
            match registry.remove(self.package) {
                Ok(()) => {
                    self.package = self.package.min(registry.packages().len().saturating_sub(1));
                    self.inspect_selected(ui.ctx());
                }
                Err(error) => self.message = format!("Could not remove plugin: {error}"),
            }
        } else if selection != self.package {
            self.package = selection;
            self.inspect_selected(ui.ctx());
        } else if add
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Mapper plugin manifest", &["json"])
                .pick_file()
        {
            self.inspect(ui.ctx(), true, move |cancel| {
                Loaded::load_cancellable(&path, || cancel.load(Ordering::Relaxed))
            });
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, project: &Project, idle: bool, settings: Option<&Path>) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong("Installed packages");
            self.packages(ui, settings);
            if self.inspection.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking plugin…");
                    if ui.button("Cancel").clicked() {
                        self.inspection = None;
                    }
                });
            }
            if let Some(plugin) = &self.plugin {
                ui.separator();
                ui.collapsing("Package details", |ui| {
                    ui.label(&plugin.manifest.description);
                    ui.label(format!(
                        "Version {} · API {}",
                        plugin.manifest.version, plugin.manifest.api_version
                    ));
                    ui.label(&plugin.manifest.id);
                    ui.small(plugin.manifest_path().display().to_string());
                    ui.small(format!("Manifest SHA-256 {}", plugin.fingerprint()));
                });

                ui.add_space(6.);
                ui.strong("Action");
                let selected = self.selected;
                ui.add_enabled_ui(self.running.is_none() && self.inspection.is_none(), |ui| {
                    let action_response = egui::ComboBox::from_id_salt("plugin-action")
                        .selected_text(&plugin.manifest.actions[self.selected].title)
                        .width(ui.available_width())
                        .truncate()
                        .show_ui(ui, |ui| {
                            for (index, action) in plugin.manifest.actions.iter().enumerate() {
                                ui.selectable_value(&mut self.selected, index, &action.title);
                            }
                        });
                    if self.selected != selected {
                        self.values.clear();
                        self.message.clear();
                        self.report = None;
                    }
                    let action = &plugin.manifest.actions[self.selected];
                    action_response.response.on_hover_text(&action.description);
                    ui.add_space(6.);
                    if action.parameters.as_ref().is_some_and(|fields| !fields.is_empty()) {
                        ui.strong("Inputs");
                    }
                    if let Some(fields) = &action.parameters {
                        ui.push_id((&plugin.manifest.id, &action.id), |ui| {
                            render_controls(ui, fields, &mut self.values)
                        });
                    } else {
                        ui.label("Parameters (JSON)");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.parameters)
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .char_limit(65536),
                        );
                    }
                    ui.add_space(6.);
                    let kind = action.kind();
                    let run_label = if kind == Capability::Inspect {
                        "Run check"
                    } else {
                        "Run plugin action"
                    };
                    if ui.add_enabled(idle, egui::Button::new(egui::RichText::new(run_label).color(egui::Color32::WHITE))
                        .fill(super::workbench_style::PRIMARY_ACTION))
                        .on_hover_text("This plugin runs with your account’s file and network access.")
                        .clicked()
                    {
                        let parameters = if action.parameters.is_some() {
                            Ok(Value::Object(self.values.clone()))
                        } else {
                            serde_json::from_str(&self.parameters)
                        };
                        match parameters {
                            Ok(parameters) => {
                                let plugin = plugin.clone();
                                let action = action.id.clone();
                                let before = project.clone();
                                let request = before.clone();
                                let (sender, receiver) = mpsc::channel();
                                let cancel = Arc::new(AtomicBool::new(false));
                                let signal = cancel.clone();
                                let ctx = ui.ctx().clone();
                                std::thread::spawn(move || {
                                    let result = plugin.invoke(
                                        &action,
                                        kind,
                                        &request,
                                        parameters,
                                        Duration::from_secs(10),
                                        &signal,
                                    );
                                    let _ = sender.send(result);
                                    ctx.request_repaint();
                                });
                                self.running = Some(Run {
                                    before,
                                    cancel,
                                    receiver,
                                });
                                self.message = "Running plugin action…".into();
                                self.report = None;
                            }
                            Err(error) => {
                                self.message = format!("Parameters must be valid JSON. {error}")
                            }
                        }
                    }
                });
            } else if self.inspection.is_none()
                && self.selected_package().is_some()
                && ui.button("Check package again").clicked()
            {
                self.inspect_selected(ui.ctx());
            }
            if let Some(task) = &self.running {
                let cancelling = task.cancel.load(Ordering::Relaxed);
                ui.spinner();
                if ui.add_enabled(!cancelling, egui::Button::new("Cancel plugin action")).clicked() {
                    self.cancel();
                }
            }
            if !self.message.is_empty() {
                ui.add_space(8.);
                ui.label(&self.message);
            }
            if let Some((basis, report)) = &self.report
                && basis == project
            {
                ui.separator();
                ui.strong("Results");
                ui.label(&report.summary);
                for finding in &report.findings {
                    let visuals = ui.visuals();
                    let (label, color) = match finding.severity {
                        Severity::Info => ("Info", visuals.text_color()),
                        Severity::Warning => ("Warning", visuals.warn_fg_color),
                        Severity::Error => ("Error", visuals.error_fg_color),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(color, label);
                        ui.label(&finding.message);
                    });
                }
            }
        });
    }
}

fn render_controls(ui: &mut egui::Ui, fields: &[Parameter], values: &mut Map<String, Value>) {
    for field in fields {
        let value = values
            .entry(&field.id)
            .or_insert_with(|| field.control.default_value());
        ui.push_id(&field.id, |ui| match &field.control {
            Control::Boolean { default } => {
                let mut enabled = value.as_bool().unwrap_or(*default);
                if ui.checkbox(&mut enabled, &field.label).changed() {
                    *value = Value::from(enabled);
                }
            }
            Control::Integer { default, min, max } => {
                ui.label(&field.label);
                let mut number = value.as_i64().map_or(*default, |n| n as i32);
                if ui
                    .add(egui::DragValue::new(&mut number).range(*min..=*max))
                    .changed()
                {
                    *value = Value::from(number);
                }
            }
            Control::Number { default, min, max } => {
                ui.label(&field.label);
                let mut number = value.as_f64().unwrap_or(*default);
                if ui
                    .add(
                        egui::DragValue::new(&mut number)
                            .range(*min..=*max)
                            .speed(0.1),
                    )
                    .changed()
                {
                    *value = Value::from(number);
                }
            }
            Control::Text { default, max_chars } => {
                ui.label(&field.label);
                let mut text = value.as_str().unwrap_or(default).to_owned();
                if ui
                    .add(egui::TextEdit::singleline(&mut text).char_limit(*max_chars))
                    .changed()
                {
                    *value = Value::from(text);
                }
            }
            Control::Choice { default, options } => {
                ui.label(&field.label);
                let current = value.as_str().unwrap_or(default);
                if let Some(option) = egui::ComboBox::from_id_salt("choice")
                    .selected_text(current)
                    .truncate()
                    .show_ui(ui, |ui| {
                        let mut picked = None;
                        for option in options {
                            if ui
                                .selectable_label(current == option.as_str(), option)
                                .clicked()
                            {
                                picked = Some(option);
                            }
                        }
                        picked
                    })
                    .inner
                    .flatten()
                {
                    *value = Value::from(option.clone());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_action_renders_native_controls_and_edits_its_arguments() {
        mod snapshot {
            include!("../examples/support/egui_snapshot.rs");
        }
        use sha2::{Digest, Sha256};

        fn center(shape: &egui::Shape, text: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| center(shape, text)),
                _ => None,
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mapper-plugin.json");
        std::fs::write(root.path().join("plugin.exe"), b"inspection fixture").unwrap();
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({
            "format":"papyrus-mapper-plugin", "api_version":1, "id":"test.controls",
            "name":"Map preferences", "version":"1", "description":"Recipe preferences",
            "executable":"plugin.exe",
            "executable_sha256":format!("{:x}", Sha256::digest(b"inspection fixture")),
            "capabilities":["recipe.transform","recipe.inspect"],
            "actions":[{
                "id":"preferences","title":"Set preferences","description":"Apply map preferences.",
                "parameters":[
                    {"id":"enabled","label":"Generate monuments","control":{"type":"boolean","default":true}},
                    {"id":"count","label":"Monument limit","control":{"type":"integer","default":2,"min":0,"max":8}},
                    {"id":"weight","label":"Placement weight","control":{"type":"number","default":0.5,"min":0,"max":1}},
                    {"id":"name","label":"Recipe note","control":{"type":"text","default":"Coastal","max_chars":32}},
                    {"id":"mode","label":"Placement mode","control":{"type":"choice","default":"Balanced","options":["Balanced","Sparse"]}}
                ]
            },{
                "id":"check","title":"Check recipe","description":"Review the recipe.",
                "capability":"recipe.inspect","parameters":[]
            }]
        })).unwrap()).unwrap();
        let plugin = Arc::new(Loaded::load(&path).unwrap());
        for (theme, width) in [(egui::Theme::Dark, 340), (egui::Theme::Light, 540)] {
            let ctx = egui::Context::default();
            crate::workbench_style::install(&ctx);
            ctx.set_theme(theme);
            ctx.global_style_mut(|style| style.animation_time = 0.);
            let mut registry = Registry::load(root.path().join("plugins.json")).unwrap();
            registry.register(&plugin).unwrap();
            let mut panel = PluginPanel {
                registry: Some(Ok(registry)),
                plugin: Some(plugin.clone()),
                ..Default::default()
            };
            let mut textures = snapshot::Textures::default();
            let recipe = Project::default();
            let mut frame = |panel: &mut PluginPanel, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            egui::vec2(width as f32, 480.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| panel.show(ui, &recipe, true, None));
                    },
                );
                textures.update(&output.textures_delta);
                output.textures_delta.clear();
                output
            };
            frame(&mut panel, vec![]);
            let first = frame(&mut panel, vec![]);
            let position = first
                .shapes
                .iter()
                .find_map(|s| center(&s.shape, "Generate monuments"))
                .unwrap();
            assert!(
                first
                    .shapes
                    .iter()
                    .all(|s| center(&s.shape, "Parameters (JSON)").is_none())
            );
            assert_eq!(panel.values["enabled"], true);
            for pressed in [true, false] {
                frame(
                    &mut panel,
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            assert_eq!(panel.values["enabled"], false);
            let final_frame = frame(&mut panel, vec![]);
            panel.selected = 1;
            let (sender, receiver) = mpsc::channel();
            sender
                .send(Ok(Outcome::Report(Report {
                    summary: "Monument policy check".into(),
                    findings: vec![mapper_studio::plugins::Finding {
                        severity: Severity::Warning,
                        message: "Review monument preferences.".into(),
                    }],
                })))
                .unwrap();
            panel.running = Some(Run {
                before: recipe.clone(),
                cancel: Arc::new(AtomicBool::new(false)),
                receiver,
            });
            assert!(panel.poll(&recipe).is_none());
            let report_frame = frame(&mut panel, vec![]);
            assert!(
                report_frame
                    .shapes
                    .iter()
                    .any(|s| center(&s.shape, "Run check").is_some())
            );
            assert!(
                report_frame
                    .shapes
                    .iter()
                    .any(|s| center(&s.shape, "Monument policy check").is_some())
            );
            if let Some(path) = std::env::var_os("MAPPER_PLUGIN_PANEL_SNAPSHOTS") {
                for (name, output) in [("controls", final_frame), ("report", report_frame)] {
                    let path =
                        std::path::PathBuf::from(&path).join(format!("plugin-{name}-{width}.png"));
                    let image = textures.raster(&ctx, output, [width, 480]);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
            }
        }
    }

    #[test]
    fn changed_recipe_discards_plugin_output_and_cancel_keeps_recipe() {
        let mut project = Project::default();
        let before = project.clone();
        let mut next = before.clone();
        next.shuffle_monuments();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(Outcome::Recipe(Box::new(next.clone()))))
            .unwrap();
        let mut panel = PluginPanel {
            running: Some(Run {
                before: before.clone(),
                cancel: Arc::new(AtomicBool::new(false)),
                receiver,
            }),
            ..Default::default()
        };
        project.targets[0].x += 1.;
        assert!(panel.poll(&project).is_none());
        assert!(panel.message.contains("discarded"));
        let (sender, receiver) = mpsc::channel();
        let signal = Arc::new(AtomicBool::new(false));
        panel.running = Some(Run {
            before,
            cancel: signal.clone(),
            receiver,
        });
        panel.cancel();
        assert!(signal.load(Ordering::Relaxed));
        assert!(panel.running.is_some());
        assert!(panel.poll(&project).is_none());
        assert!(panel.running.is_some());
        sender.send(Ok(Outcome::Recipe(Box::new(next)))).unwrap();
        assert!(panel.poll(&project).is_none());
        assert!(panel.running.is_none());
        assert_eq!(panel.message, "Plugin action cancelled.");
    }
}
