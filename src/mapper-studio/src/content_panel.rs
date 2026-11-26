use eframe::egui;
use mapper_studio::content_package::{Binding, Imported, Manifest, TerrainRegion};
use mapper_studio::{Project, custom_monuments, monument_addons, rustedit_prefab};
use papyrus_terrain::content::{self, Composed, Vector};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

enum ContentResult {
    Map(Arc<Imported>),
    RustEditPrefab { name: String, source: rustedit_prefab::Source },
    Placement(Box<Project>, custom_monuments::Placement),
}

struct ImportTask {
    receiver: Receiver<Result<ContentResult, String>>,
    cancel: Arc<AtomicBool>,
    activity: &'static str,
}
impl ImportTask {
    fn start(path: PathBuf, ctx: egui::Context) -> Self {
        Self::run(ctx, "Reading content…", move |cancelled| {
            let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
            if extension.eq_ignore_ascii_case("prefab") {
                rustedit_prefab::Source::read(&path, cancelled).map(|source| ContentResult::RustEditPrefab {
                    name: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                    source,
                })
            } else if extension.eq_ignore_ascii_case("asset") {
                Err("RustEdit 2 monument files are not supported yet.".into())
            } else {
                Imported::read_file(&path, cancelled).map(|item| ContentResult::Map(Arc::new(item)))
            }
        })
    }

    fn run(
        ctx: egui::Context,
        activity: &'static str,
        work: impl FnOnce(&dyn Fn() -> bool) -> Result<ContentResult, String> + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let _ = sender.send(work(&|| signal.load(Ordering::Relaxed)));
            ctx.request_repaint();
        });
        Self { receiver, cancel, activity }
    }
}

impl Drop for ImportTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum LayoutResult {
    Versions(Vec<monument_addons::Version>),
    Preview(monument_addons::PrefabLayout),
}

struct OverlayTask {
    receiver: Receiver<Result<LayoutResult, String>>,
    cancel: Arc<AtomicBool>,
    importing: bool,
}

impl OverlayTask {
    fn start(
        ctx: &egui::Context,
        importing: bool,
        work: impl FnOnce(&AtomicBool) -> Result<LayoutResult, String> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let ctx = ctx.clone();
        std::thread::Builder::new().name("mapper-overlay-library".into()).spawn(move || {
            let _ = sender.send(work(&signal));
            ctx.request_repaint();
        }).map_err(|error| format!("Could not start overlay library worker. {error}"))?;
        Ok(Self { receiver, cancel, importing })
    }
}

impl Drop for OverlayTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct MonumentSettings {
    recipe_id: u64,
    name: String,
    path: String,
    recovery_anchor: Option<u32>,
    installation: Option<mapper_studio::installation::Installation>,
    task: Option<OverlayTask>,
    loaded_anchor: Option<u32>,
    versions: Vec<monument_addons::Version>,
    preview: Option<monument_addons::PrefabLayout>,
    message: String,
}

fn save_layout(addon: &monument_addons::AddOn, signal: &AtomicBool) -> Result<LayoutResult, String> {
    let cancelled = || signal.load(Ordering::Relaxed);
    if cancelled() {
        return Err("Layout import cancelled".into());
    }
    let library = monument_addons::Library::local()?;
    library.store(addon)?;
    library.versions_cancellable(addon.anchor_id, cancelled).map(LayoutResult::Versions)
}

fn preview_layout(ui: &mut egui::Ui, layout: &monument_addons::PrefabLayout) {
    let children = &layout.addon.children;
    ui.label(format!("{} · {} objects", layout.addon.name, children.len()));
    ui.weak(if layout.authored_anchor {
        "Using the stock monument in the source file as the pivot."
    } else {
        "This prefab has no matching stock monument. Use a .map to read the original position and rotation."
    });
    let (_, painter) = ui.allocate_painter(
        egui::vec2(ui.available_width(), 132.),
        egui::Sense::hover(),
    );
    let rect = painter.clip_rect();
    let (min, max) = children.iter().fold((egui::Vec2::ZERO, egui::Vec2::ZERO), |(min, max), child| {
        let point = egui::vec2(child.offset[0], -child.offset[2]);
        (min.min(point), max.max(point))
    });
    let extent = (max - min).max(egui::Vec2::splat(1.));
    let frame = rect.shrink(10.);
    let scale = (frame.width() / extent.x).min(frame.height() / extent.y).max(0.);
    let project = |point: egui::Vec2| frame.center() + (point - (min + max) * 0.5) * scale;
    let theme = ui.visuals();
    painter.rect_filled(rect, 5., theme.extreme_bg_color);
    for child in children.iter() {
        painter.circle_filled(
            project(egui::vec2(child.offset[0], -child.offset[2])),
            1.6,
            theme.hyperlink_color,
        );
    }
    let origin = project(egui::Vec2::ZERO);
    let stroke = egui::Stroke::new(1.5, theme.text_color());
    painter.hline(origin.x - 5. ..=origin.x + 5., origin.y, stroke);
    painter.vline(origin.x, origin.y - 5. ..=origin.y + 5., stroke);
    if layout.has_unapplied_metadata {
        ui.weak("Saving the layout keeps the placed objects. RustEdit metadata is omitted.");
    }
}

pub(super) struct ContentPanel {
    item: Option<Arc<Imported>>,
    rustedit_prefab: Option<(String, rustedit_prefab::Source)>,
    task: Option<ImportTask>,
    placement: Option<(Box<Project>, custom_monuments::Placement)>,
    name: String,
    addon: bool,
    anchor: usize,
    pivot: [f32; 3],
    authoring_yaw: f32,
    destination: [f32; 3],
    yaw: f32,
    region: TerrainRegion,
    include_terrain: bool,
    preview: Option<Composed>,
    preview_binding: Option<Binding>,
    preview_destination: [f32; 4],
    message: String,
    pub focus_requested: bool,
    monument_settings: Option<MonumentSettings>,
}
impl Default for ContentPanel {
    fn default() -> Self {
        Self {
            item: None,
            rustedit_prefab: None,
            task: None,
            placement: None,
            name: String::new(),
            addon: false,
            anchor: 0,
            pivot: [0.; 3],
            authoring_yaw: 0.,
            destination: [0.; 3],
            yaw: 0.,
            region: TerrainRegion {
                center: [0.; 2],
                size: [200.; 2],
                fade: 20.,
                height: true,
                splat: true,
                topology: true,
                biome: false,
                alpha: false,
            },
            include_terrain: false,
            preview: None,
            preview_binding: None,
            preview_destination: [0.; 4],
            message: String::new(),
            focus_requested: false,
            monument_settings: None,
        }
    }
}
impl ContentPanel {
    pub fn choose_file(&mut self, ctx: &egui::Context) {
        if self.task.is_some() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Rust content", &["map", "mapperasset", "prefab"])
            .pick_file()
        {
            self.start(path, ctx.clone());
        }
    }
    fn start(&mut self, path: PathBuf, ctx: egui::Context) {
        self.task = Some(ImportTask::start(path, ctx));
        self.focus_requested = true;
        self.message.clear();
    }
    pub fn poll(&mut self) {
        let Some(task) = &self.task else {
            return;
        };
        let result = match task.receiver.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Disconnected) => {
                Some(Err("Content worker ended unexpectedly".into()))
            }
            Err(TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            let cancelled = task.cancel.load(Ordering::Relaxed);
            self.task = None;
            if cancelled {
                self.message = "Cancelled.".into();
                return;
            }
            match result {
                Ok(ContentResult::Map(item)) => {
                    *self = Self { monument_settings: self.monument_settings.take(), ..Self::default() };
                    self.name = item.manifest.name.clone();
                    match &item.manifest.binding {
                        Binding::AddOn { anchor_index } => {
                            self.addon = true;
                            self.anchor = *anchor_index;
                        }
                        Binding::Standalone {
                            pivot,
                            authoring_yaw,
                            terrain,
                        } => {
                            self.addon = false;
                            self.pivot = *pivot;
                            self.authoring_yaw = *authoring_yaw;
                            self.include_terrain = terrain.is_some();
                            if let Some(region) = terrain {
                                self.region = region.clone();
                            }
                        }
                    }
                    self.item = Some(item);
                    self.preview = None;
                    self.preview_binding = None;
                    self.focus_requested = true;
                }
                Ok(ContentResult::RustEditPrefab { name, source }) => {
                    *self = Self {
                        rustedit_prefab: Some((name, source)),
                        monument_settings: self.monument_settings.take(),
                        focus_requested: true,
                        ..Self::default()
                    };
                }
                Ok(ContentResult::Placement(expected, placement)) => {
                    self.placement = Some((expected, placement));
                }
                Err(e) => self.message = e,
            }
        }
    }
    fn binding(&self) -> Binding {
        if self.addon {
            Binding::AddOn {
                anchor_index: self.anchor,
            }
        } else {
            Binding::Standalone {
                pivot: self.pivot,
                authoring_yaw: self.authoring_yaw,
                terrain: self.include_terrain.then(|| self.region.clone()),
            }
        }
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        if let Some((expected, placement)) = self.placement.take() {
            self.message = if *expected == *project {
                match custom_monuments::add_placement(project, placement) {
                    Ok(()) => "Added to Monuments.".into(),
                    Err(error) => error,
                }
            } else {
                "Recipe changed. Select Add to recipe again.".into()
            };
        }
        egui::ScrollArea::vertical().show(ui,|ui|{
            ui.horizontal_wrapped(|ui| {
                ui.heading("Custom content");
                if ui.add_enabled(self.task.is_none(), egui::Button::new(
                    egui::RichText::new("Import content…").color(egui::Color32::WHITE),
                ).fill(super::workbench_style::PRIMARY_ACTION)).clicked() {
                    self.choose_file(ui.ctx());
                }
            });
            ui.separator();
            if let Some(task)=&self.task{
                ui.horizontal(|ui|{ui.spinner();ui.label(task.activity);});
                if ui.button("Cancel").clicked(){task.cancel.store(true,Ordering::Relaxed);}
            }
            if !self.message.is_empty() {
                ui.label(&self.message);
            }
            if !project.monument_addons.is_empty() {
                ui.separator();
                ui.strong("Embedded layouts");
                let mut remove = None;
                for (index, addon) in project.monument_addons.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(&addon.name).on_hover_text(format!(
                                "Anchor ID {} · {} objects", addon.anchor_id, addon.children.len()
                            ));
                            if ui.button("Move to library").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
                if let Some(index) = remove {
                    self.message = match monument_addons::Library::local()
                        .and_then(|library| library.store(&project.monument_addons[index]))
                    {
                        Ok(_) => {
                            project.monument_addons.remove(index);
                            "Layout moved to the library.".into()
                        }
                        Err(error) => error,
                    };
                }
            }
            if let Some((name, source)) = &self.rustedit_prefab {
                ui.separator();
                ui.strong(name);
                ui.label(format!("{} objects · {} terrain images", source.prefab.prefabs.len(), source.terrain.len()));
                ui.weak("Inspection only. Placing RustEdit prefabs is still in development.");
                if !source.terrain.is_empty() {
                    ui.collapsing("Terrain images", |ui| {
                        for image in &source.terrain {
                            let [width, height] = image.dimensions;
                            ui.label(format!("{} · {width} × {height}", image.suffix));
                        }
                    });
                }
                return;
            }
            let Some(item)=&self.item else{
                ui.weak("Open a .map, .mapperasset, or RustEdit 1 .prefab.");
                return;
            };
            ui.separator();
            ui.label(format!("{} m source map", item.map.size))
                .on_hover_text(format!("Prefabs {} · Paths {}", item.map.prefabs.len(), item.map.paths.len()));
            ui.label("Package name");ui.text_edit_singleline(&mut self.name);
            egui::ComboBox::from_id_salt("content-kind").selected_text(if self.addon{"Monument layout"}else{"Custom monument"}).show_ui(ui,|ui|{
                ui.selectable_value(&mut self.addon,true,"Monument layout");ui.selectable_value(&mut self.addon,false,"Custom monument");
            });
            if self.addon{
                ui.label("Anchor source index");ui.add(egui::DragValue::new(&mut self.anchor).range(0..=item.map.prefabs.len().saturating_sub(1)));
                if let Some(p)=item.map.prefabs.get(self.anchor){ui.label(format!("Asset ID {} · {}",p.id,p.category));ui.small(format!("Origin ({:.1}, {:.1}, {:.1}) · yaw {:.1}°",p.position.x,p.position.y,p.position.z,p.rotation.y));}
                if ui.button("Save layout to library").on_hover_text(
                    "Choose this version later in the monument's settings."
                ).clicked() {
                    self.message = match monument_addons::import(item, self.name.clone(), self.anchor)
                        .and_then(|addon| monument_addons::Library::local()?.store(&addon))
                    {
                        Ok(_) => "Layout saved to the library.".into(),
                        Err(error) => error,
                    };
                }
            }else{
                ui.label("Authored pivot (X, Y, Z)");
                ui.horizontal_wrapped(|ui|{for value in &mut self.pivot{ui.add(egui::DragValue::new(value).range(-65536. ..=65536.));}});
                ui.horizontal(|ui|{ui.label("Authored yaw");ui.add(egui::DragValue::new(&mut self.authoring_yaw).range(-360. ..=360.));});
                ui.checkbox(&mut self.include_terrain,"Terrain region");
                if self.include_terrain{
                    ui.label("Source center X / Z");ui.horizontal_wrapped(|ui|{for value in &mut self.region.center{ui.add(egui::DragValue::new(value));}});
                    ui.label("Width / depth (m)");ui.horizontal_wrapped(|ui|{for value in &mut self.region.size{ui.add(egui::DragValue::new(value).range(1. ..=item.map.size as f32));}});
                    ui.horizontal(|ui|{ui.label("Fade (m)");ui.add(egui::DragValue::new(&mut self.region.fade).range(0. ..=item.map.size as f32));});
                    ui.horizontal_wrapped(|ui| {
                        for (name, value) in [
                            ("Height", &mut self.region.height),
                            ("Splat", &mut self.region.splat),
                            ("Topology", &mut self.region.topology),
                            ("Biome", &mut self.region.biome),
                            ("Terrain holes", &mut self.region.alpha),
                        ] {
                            ui.checkbox(value, name);
                        }
                    });
                }
            }
            ui.collapsing("Source layers",|ui|{for layer in &item.map.layers{ui.label(format!("{} · {} bytes",layer.name,layer.data.len()));}});
            let extra=item.map.unknown_fields+item.map.prefabs.iter().map(|p|p.unknown_fields).sum::<usize>()+item.map.paths.iter().map(|p|p.unknown_fields).sum::<usize>();
            if extra>0{ui.colored_label(ui.visuals().warn_fg_color,format!("{extra} extra fields retained in source; their gameplay meaning is unverified."));}
            ui.separator();ui.strong("Placement");ui.label("Destination X / Y / Z");
            ui.horizontal_wrapped(|ui|{for value in &mut self.destination{ui.add(egui::DragValue::new(value).range(-65536. ..=65536.));}});
            ui.horizontal(|ui|{ui.label("Destination yaw");ui.add(egui::DragValue::new(&mut self.yaw).range(-360. ..=360.));});
            if ui.button("Preview placement").clicked(){
                let binding=self.binding();let position=Vector{x:self.destination[0],y:self.destination[1],z:self.destination[2]};
                match item.terrain_stamp(&binding)
                    .and_then(|_| content::compose(&item.map,binding.composition(),position,self.yaw)) {
                    Ok(preview)=>{self.preview=Some(preview);self.preview_binding=Some(binding);self.preview_destination=[self.destination[0],self.destination[1],self.destination[2],self.yaw];self.message.clear();},
                    Err(e)=>{self.preview=None;self.message=e;}
                }
            }
            let current=self.preview_binding.as_ref()==Some(&self.binding())&&self.preview_destination==[self.destination[0],self.destination[1],self.destination[2],self.yaw];
            if current&&let Some(preview)=&self.preview{
                ui.label(format!("{} child objects · {} paths",preview.prefabs.len(),preview.paths.len()));
                for p in preview.prefabs.iter().take(8){ui.small(format!("{}: ({:.1}, {:.1}, {:.1}) yaw {:.1}°",p.id,p.position.x,p.position.y,p.position.z,p.rotation.y));}
                if preview.prefabs.len()>8{ui.small("First eight objects shown; all objects are retained in the package.");}
            }
            ui.separator();
            if !self.addon && self.include_terrain {
                let ready = project.installation.is_some() && project.composition_engine.is_some()
                    && mapper_studio::native_preview::is_native(&project.preview_engine);
                if ui.add_enabled(ready && self.task.is_none(), egui::Button::new("Add to recipe"))
                    .on_disabled_hover_text("Choose a Rust installation and prepare map generation first.")
                    .clicked()
                {
                    let manifest = Manifest {
                        schema: 1,
                        name: self.name.trim().to_owned(),
                        source_sha256: item.manifest.source_sha256.clone(),
                        binding: self.binding(),
                    };
                    let mut expected = project.clone();
                    let item = Arc::clone(item);
                    let position = self.destination;
                    let yaw = self.yaw.rem_euclid(360.);
                    self.message.clear();
                    self.task = Some(ImportTask::run(ui.ctx().clone(), "Adding monument…", move |cancel| {
                        custom_monuments::add_package_with_manifest(&mut expected, &item, &manifest, position, yaw, cancel)?;
                        let placement = expected.custom_monuments.pop().expect("placement was added");
                        Ok(ContentResult::Placement(Box::new(expected), placement))
                    }));
                }
            }
            if ui.button("Save portable package…").clicked(){
                let manifest=Manifest{schema:1,name:self.name.clone(),source_sha256:item.manifest.source_sha256.clone(),binding:self.binding()};
                match item.validate(&manifest){
                    Err(e)=>self.message=e,
                    Ok(())=>if let Some(path)=rfd::FileDialog::new().add_filter("Mapper content package",&["mapperasset"]).set_file_name("custom-content.mapperasset").save_file(){self.message=match item.save(&manifest,&path){Ok(())=>format!("Package saved: {}",path.display()),Err(e)=>format!("Save failed: {e}")};},
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_can_clear_only_the_blocked_choice_without_installation_ids() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut panel = ContentPanel::default();
        let mut project = Project::default();
        let anchor = 4_163_222_222;
        project.monument_overlay_choices.insert(anchor, "a".repeat(64));
        panel.open_overlay_recovery(&project, crate::FIRST_RECIPE, anchor, String::new(), "Monument layout".into(),
            "Version unavailable".into());
        let (sender, receiver) = std::sync::mpsc::channel();
        let settings = panel.monument_settings.as_mut().unwrap();
        settings.loaded_anchor = Some(anchor);
        settings.task = Some(OverlayTask {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            importing: false,
        });
        let render = |panel: &mut ContentPanel, project: &mut Project, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                events,
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800., 600.))),
                ..Default::default()
            }, |_| panel.show_monument_settings(&ctx, project, None));
            output.textures_delta.clear();
            output
        };
        let click = |panel: &mut ContentPanel, project: &mut Project, label: &str| {
            let output = render(panel, project, vec![]);
            let point = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            }).unwrap_or_else(|| panic!("Missing control {label}"));
            for pressed in [true, false] {
                render(panel, project, vec![egui::Event::PointerMoved(point), egui::Event::PointerButton {
                    pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                }]);
            }
        };
        render(&mut panel, &mut project, vec![]);
        click(&mut panel, &mut project, "Loading versions…");
        click(&mut panel, &mut project, "Stock monument");
        sender.send(Ok(LayoutResult::Versions(Vec::new()))).unwrap();
        render(&mut panel, &mut project, vec![]);
        assert!(project.monument_overlay_choices.is_empty());
        assert_eq!(panel.monument_settings_recipe(), Some(crate::FIRST_RECIPE));
    }
}

impl ContentPanel {
    pub(super) fn monument_settings_recipe(&self) -> Option<u64> {
        self.monument_settings.as_ref().map(|settings| settings.recipe_id)
    }

    pub(super) fn open_monument_settings(&mut self, project: &Project, recipe_id: u64, path: String, name: String) {
        self.monument_settings = Some(MonumentSettings {
            recipe_id,
            name,
            path,
            recovery_anchor: None,
            installation: project.installation.clone(),
            task: None,
            loaded_anchor: None,
            versions: Vec::new(),
            preview: None,
            message: String::new(),
        });
    }

    pub(super) fn open_overlay_recovery(&mut self, project: &Project, recipe_id: u64, anchor: u32, path: String, name: String, message: String) {
        self.open_monument_settings(project, recipe_id, path, name);
        if let Some(settings) = &mut self.monument_settings {
            settings.recovery_anchor = Some(anchor);
            settings.message = message;
        }
    }

    pub(super) fn show_monument_settings(
        &mut self,
        ctx: &egui::Context,
        project: &mut Project,
        identities: Option<Arc<mapper_studio::client_preparation::ReadyIdentities>>,
    ) {
        let Some(settings) = self.monument_settings.as_mut() else { return; };
        if settings.installation != project.installation {
            self.monument_settings = None;
            return;
        }
        let available_anchor = identities.as_ref().and_then(|ids| ids.prefab_id(&settings.path));
        let anchor = settings.recovery_anchor.or(available_anchor);
        if settings.loaded_anchor != anchor {
            settings.loaded_anchor = anchor;
            settings.versions.clear();
            settings.preview = None;
            settings.task = None;
            if let Some(anchor) = anchor {
                let embedded: Vec<_> = project.monument_addons.iter()
                    .filter(|addon| addon.anchor_id == anchor).cloned().collect();
                match OverlayTask::start(ctx, false, move |signal| {
                    let library = monument_addons::Library::local()?;
                    let cancelled = || signal.load(Ordering::Relaxed);
                    for addon in &embedded {
                        if cancelled() { return Err("Layout library scan cancelled".into()); }
                        library.store(addon)?;
                    }
                    library.versions_cancellable(anchor, cancelled).map(LayoutResult::Versions)
                }) {
                    Ok(task) => settings.task = Some(task),
                    Err(error) => settings.message = error,
                }
            }
        }
        if let Some(task) = &settings.task {
            let result = match task.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err("Layout library worker stopped".into())),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let importing = task.importing;
                settings.task = None;
                match result {
                    Ok(LayoutResult::Versions(versions)) => {
                        settings.versions = versions;
                        if importing {
                            settings.preview = None;
                            settings.message = "Layout saved to the library".into();
                        }
                    }
                    Ok(LayoutResult::Preview(preview)) => settings.preview = Some(preview),
                    Err(error) => settings.message = error,
                }
            }
        }
        let mut open = true;
        egui::Window::new(&settings.name)
            .id(egui::Id::new("monument-settings"))
            .collapsible(false)
            .default_width(360.)
            .open(&mut open)
            .show(ctx, |ui| {
                let layout_label = ui.strong("Layout");
                if let Some(anchor) = anchor {
                    let current = project.monument_overlay_choices.get(&anchor).map(String::as_str);
                    let embedded = project.monument_addons.iter().any(|addon| addon.anchor_id == anchor);
                    let mut chosen = current;
                    let version_label = |index: usize| {
                        let version = &settings.versions[index];
                        let duplicate = (index > 0 && settings.versions[index - 1].name == version.name)
                            || settings.versions.get(index + 1).is_some_and(|next| next.name == version.name);
                        if duplicate { format!("{} · {}", version.name, &version.key[..8]) }
                        else { version.name.clone() }
                    };
                    let label = match current {
                        Some(key) => settings.versions.iter().position(|version| version.key == key)
                            .map_or_else(|| {
                                if settings.task.as_ref().is_some_and(|task| !task.importing) {
                                    "Loading versions…".into()
                                } else {
                                    "Missing library version".into()
                                }
                            }, version_label),
                        None if embedded => "Embedded layouts".into(),
                        None => "Stock monument".into(),
                    };
                    let mut selected = false;
                    egui::ComboBox::from_id_salt(("overlay-version", anchor))
                        .selected_text(label).truncate().width(ui.available_width()).show_ui(ui, |ui| {
                        if ui.add_enabled(!embedded || settings.task.is_none(),
                            egui::Button::selectable(current.is_none() && !embedded, "Stock monument")).clicked() {
                            chosen = None;
                            selected = true;
                        }
                        for (index, version) in settings.versions.iter().enumerate() {
                            if ui.selectable_label(current == Some(&version.key), version_label(index))
                                .on_hover_text(format!("{} objects", version.objects)).clicked() {
                                chosen = Some(&version.key);
                                selected = true;
                            }
                        }
                    }).response.labelled_by(layout_label.id)
                        .on_hover_text("This recipe applies the selected layout to every placement of this monument.");
                    if selected && (chosen != current || embedded) {
                        let chosen = chosen.map(str::to_owned);
                        let result = (|| {
                            if let Some(key) = &chosen {
                                let addon = monument_addons::Library::local()?.load(anchor, key)?;
                                identities.as_ref().ok_or("Prepare this monument’s Rust installation first.")?
                                    .addons.check(std::slice::from_ref(&addon))?;
                            }
                            if embedded {
                                let library = monument_addons::Library::local()?;
                                library.save_embedded(project, anchor)?;
                            }
                            project.choose_monument_overlay(anchor, chosen)
                        })();
                        settings.message = result.err().unwrap_or_default();
                    }
                }
                ui.horizontal(|ui| {
                    let add = ui.add_enabled(available_anchor.is_some() && settings.task.is_none(), egui::Button::new("Import layout…"))
                        .on_hover_text("Export a RustEdit .map containing this stock monument and the objects you want to attach. Mapper aligns the objects to the stock monument.")
                        .on_disabled_hover_text(if available_anchor.is_none() {
                            "Prepare this monument’s Rust installation first."
                        } else {
                            "The layout library is busy."
                        });
                    if add.clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("RustEdit layout map", &["map"])
                            .add_filter("Mapper layout package", &["mapperasset"])
                            .add_filter("RustEdit 1 prefab", &["prefab"])
                            .pick_file()
                        && let (Some(anchor), Some(ids)) = (available_anchor, identities.as_ref())
                    {
                        settings.message.clear();
                        settings.preview = None;
                        let ids = Arc::clone(ids);
                        match OverlayTask::start(ctx, true, move |signal| {
                            let cancelled = || signal.load(Ordering::Relaxed);
                            if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("prefab")) {
                                let name = path.file_stem().and_then(|name| name.to_str())
                                    .unwrap_or("RustEdit layout").to_owned();
                                let source = rustedit_prefab::Source::read(&path, cancelled)?;
                                let preview = monument_addons::import_rustedit_layout(source, name, anchor)?;
                                ids.addons.check(std::slice::from_ref(&preview.addon))?;
                                return Ok(LayoutResult::Preview(preview));
                            }
                            let item = Imported::read_file(&path, cancelled)?;
                            let addon = monument_addons::import_for_anchor(&item, anchor)?;
                            ids.addons.check(std::slice::from_ref(&addon))?;
                            save_layout(&addon, signal)
                        }) {
                            Ok(task) => settings.task = Some(task),
                            Err(error) => settings.message = error,
                        }
                    }
                    if let Some(task) = &settings.task {
                        ui.spinner();
                        if task.importing {
                            if ui.button("Cancel import").clicked() {
                                settings.task = None;
                                settings.loaded_anchor = None;
                            }
                        } else if anchor.is_none_or(|anchor| !project.monument_overlay_choices.contains_key(&anchor)) {
                            ui.weak("Loading versions…");
                        }
                    }
                });
                if let Some(preview) = &settings.preview {
                    ui.separator();
                    preview_layout(ui, preview);
                    let discard = ui.horizontal(|ui| {
                        let idle = settings.task.is_none();
                        let label = if preview.authored_anchor {
                            "Save layout"
                        } else {
                            "Save using prefab origin"
                        };
                        let save = ui.add_enabled(idle && available_anchor == Some(preview.addon.anchor_id),
                            egui::Button::new(label)).clicked();
                        let clear = ui.add_enabled(idle, egui::Button::new("Discard preview")).clicked();
                        if save {
                            let addon = preview.addon.clone();
                            settings.message.clear();
                            match OverlayTask::start(ctx, true, move |signal| save_layout(&addon, signal)) {
                                Ok(task) => settings.task = Some(task),
                                Err(error) => settings.message = error,
                            }
                        }
                        !save && clear
                    }).inner;
                    if discard {
                        settings.preview = None;
                    }
                }
                if !settings.message.is_empty() { ui.label(&settings.message); }
            });
        if !open { self.monument_settings = None; }
    }
}
