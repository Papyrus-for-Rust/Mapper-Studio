use super::*;
use mapper_studio::{installation::Installation,prefab_assets::Catalog,monument_icons};
use std::cell::RefCell;
use std::{collections::{BTreeMap,HashMap},sync::{Arc,atomic::{AtomicBool,Ordering},mpsc::{self,Receiver,TryRecvError}}};
struct Task {cancel:Arc<AtomicBool>,receiver:Receiver<Result<Catalog,String>>}
impl Drop for Task {fn drop(&mut self){self.cancel.store(true,Ordering::Relaxed);}}
pub const ICON_SIDE:f32=mapper_studio::map_labels::MARKER_SIDE;
pub const PLACED_RADIUS: f32 = 3.;
pub const REQUEST_RADIUS:f32=5.;
const PICK_PADDING:f32=2.;
const MAX_CACHED_PREFAB_ICONS: usize = 4096;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ArtworkKey<'a> {
    Builtin(&'static str),
    Custom(&'a str),
}
struct Artwork {texture:egui::TextureHandle,alpha:Vec<u8>}
impl Artwork {
    fn hit(&self,delta:Vec2)->bool {
        if delta.x.abs()>ICON_SIDE/2. || delta.y.abs()>ICON_SIDE/2. {return false;}
        // Match the linearly filtered, clamp-to-edge icon quad. Transparent
        // padding is not part of the monument's selectable marker.
        let size=monument_icons::SIZE;
        let at=|v:f32|((v/ICON_SIDE+0.5)*size as f32-0.5).clamp(0.,(size-1) as f32);
        let x=at(delta.x);let y=at(delta.y);let x0=x.floor() as usize;let y0=y.floor() as usize;
        let x1=(x0+1).min(size-1);let y1=(y0+1).min(size-1);let fx=x-x0 as f32;let fy=y-y0 as f32;
        let top=self.alpha[y0*size+x0] as f32*(1.-fx)+self.alpha[y0*size+x1] as f32*fx;
        let bottom=self.alpha[y1*size+x0] as f32*(1.-fx)+self.alpha[y1*size+x1] as f32*fx;
        top*(1.-fy)+bottom*fy>=8.
    }
}

#[derive(Default)]
pub struct Panel {
    source:Option<Installation>,catalog:Option<Arc<Catalog>>,task:Option<Task>,attempted:bool,
    builtin_keys: RefCell<HashMap<String, Option<&'static str>>>,
    builtin_textures: HashMap<&'static str, Result<Artwork, String>>,
    textures:HashMap<String,Result<Artwork,String>>,
    search:String,pub message:String,
    pub labels:map_label_panel::Panel,
}
impl Panel {
    pub fn for_export(&self) -> Self {
        Self::with_export_catalog(self.catalog.clone())
    }
    pub fn with_export_catalog(catalog: Option<Arc<Catalog>>) -> Self {
        Self {
            source: catalog.as_ref().map(|c| c.installation.clone()),
            catalog,
            attempted: true,
            ..Default::default()
        }
    }
    pub fn export_texture_bytes(
        &self,
        monuments: &[super::map_monuments::Monument],
        preferences: &mapper_studio::preferences::Preferences,
    ) -> usize {
        monuments
            .iter()
            .filter_map(|m| self.artwork_key(m.prefab.as_deref(), preferences))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            * monument_icons::SIZE
            * monument_icons::SIZE
            * 4
    }
    #[cfg(test)]
    pub(super) fn seed_names(&mut self,catalog:Arc<Catalog>){self.source=Some(catalog.installation.clone());self.set_catalog(Some(catalog));}
    fn set_catalog(&mut self, catalog: Option<Arc<Catalog>>) {
        self.catalog = catalog;
        self.builtin_keys.get_mut().clear();
    }
    pub fn tick(&mut self,project:&Project,root:Option<PathBuf>,catalog:Option<Arc<Catalog>>,ctx:&egui::Context) {
        if self.source!=project.installation {self.task=None;self.set_catalog(None);self.attempted=false;self.source=project.installation.clone();self.message.clear();}
        if let Some(task)=&self.task {
            let result=match task.receiver.try_recv(){Ok(r)=>Some(r),Err(TryRecvError::Empty)=>None,Err(_)=>Some(Err("Monument names worker stopped".into()))};
            if let Some(result)=result {self.task=None;match result {Ok(c)=>{self.set_catalog(Some(Arc::new(c)));self.message.clear();},Err(e)=>self.message=e}}
        }
        if self.catalog.is_none() && let Some(c)=catalog.as_ref().filter(|c|c.entries.values().any(|a|a.display_name.is_some())) {self.set_catalog(Some(c.clone()));}
        if self.catalog.is_none() && catalog.is_some() && !self.attempted && self.task.is_none()
            && let Some((root,source))=root.zip(self.source.clone()) {
            self.attempted=true;let cancel=Arc::new(AtomicBool::new(false));let signal=cancel.clone();let context=ctx.clone();let(tx,receiver)=mpsc::channel();
            std::thread::spawn(move||{let _loading=mapper_studio::loading_progress::begin("Monument display names",Some(signal.clone()));
                let result=mapper_studio::no_build::prepare_catalog(&root,&source,false,||signal.load(Ordering::Relaxed));let _=tx.send(result);context.request_repaint();});
            self.task=Some(Task{cancel,receiver});
        }
        if self.task.is_some(){ctx.request_repaint_after(std::time::Duration::from_millis(100));}
    }
    pub fn name(&self,path:Option<&str>,fallback:&str)->String {
        path.and_then(|path| self.display_name(path)).unwrap_or(fallback).to_owned()
    }
    pub(super) fn name_source(&self) -> Option<&Arc<Catalog>> {
        self.catalog.as_ref()
    }
    fn display_name(&self, path: &str) -> Option<&str> {
        self.catalog.as_ref()?.entries.get(path)?.display_name.as_deref()
    }
    fn artwork_key<'a>(&self, path: Option<&str>, preferences: &'a mapper_studio::preferences::Preferences) -> Option<ArtworkKey<'a>> {
        let path = path?;
        if let Some(hash) = preferences.monument_icons.get(path) {
            return Some(ArtworkKey::Custom(hash));
        }
        let mut keys = self.builtin_keys.borrow_mut();
        if let Some(key) = keys.get(path) {
            return key.map(ArtworkKey::Builtin);
        }
        let stem = path.rsplit('/').next()?.trim_end_matches(".prefab");
        let key = monument_icons::default_key_for_prefab(self.display_name(path).unwrap_or(stem), path);
        if keys.len() < MAX_CACHED_PREFAB_ICONS {
            keys.insert(path.to_owned(), key);
        }
        key.map(ArtworkKey::Builtin)
    }
    fn texture(&mut self, ctx: &egui::Context, settings: Option<&std::path::Path>, key: ArtworkKey<'_>) -> Option<egui::TextureId> {
        let upload = |name: &str, rgba: Vec<u8>| Artwork {
            texture: ctx.load_texture(
                format!("monument-icon-{name}"),
                egui::ColorImage::from_rgba_unmultiplied([monument_icons::SIZE; 2], &rgba),
                egui::TextureOptions::LINEAR,
            ),
            alpha: rgba.chunks_exact(4).map(|pixel| pixel[3]).collect(),
        };
        let artwork = match key {
            ArtworkKey::Builtin(name) => self.builtin_textures.entry(name)
                .or_insert_with(|| monument_icons::default_rgba(name).map(|rgba| upload(name, rgba))),
            ArtworkKey::Custom(hash) => {
                if !self.textures.contains_key(hash) {
                    let pixels = settings.ok_or("Settings storage unavailable".into())
                        .and_then(monument_icons::directory)
                        .and_then(|folder| monument_icons::load(&folder, hash));
                    self.textures.insert(hash.into(), pixels.map(|rgba| upload(hash, rgba)));
                }
                self.textures.get_mut(hash)?
            }
        };
        artwork.as_ref().ok().map(|art| art.texture.id())
    }
    fn cached_artwork(&self, key: ArtworkKey<'_>) -> Option<&Artwork> {
        match key {
            ArtworkKey::Builtin(name) => self.builtin_textures.get(name),
            ArtworkKey::Custom(hash) => self.textures.get(hash),
        }
        .and_then(|art| art.as_ref().ok())
    }
    pub fn prepare_hit(&mut self,ctx:&egui::Context,path:Option<&str>,preferences:&mapper_studio::preferences::Preferences,settings:Option<&std::path::Path>,text:map_label_panel::Text<'_>) {
        if preferences.monument_markers==mapper_studio::map_labels::Mode::None {return;}
        if preferences.monument_markers==mapper_studio::map_labels::Mode::Text {self.labels.prepare(ctx,text.source,text.name);return;}
        if let Some(key)=self.artwork_key(path,preferences){self.texture(ctx,settings,key);}
    }
    pub fn marker_hit(&self,point:Pos2,center:Pos2,path:Option<&str>,preferences:&mapper_studio::preferences::Preferences,requested:bool,text:map_label_panel::Text<'_>)->bool {
        let delta=point-center;
        // The requested center dot is drawn independently of its artwork.
        if requested && delta.length()<=REQUEST_RADIUS+PICK_PADDING {return true;}
        if preferences.monument_markers==mapper_studio::map_labels::Mode::None {return false;}
        if preferences.monument_markers==mapper_studio::map_labels::Mode::Text {return self.labels.hit(point,center,text.source,text.name);}
        if let Some(icon)=self.artwork_key(path,preferences).and_then(|key|self.cached_artwork(key)) {
            return icon.hit(delta);
        }
        !requested && delta.length() <= PLACED_RADIUS + PICK_PADDING
    }
    pub fn paint(&mut self,painter:&egui::Painter,p:Pos2,path:Option<&str>,preferences:&mapper_studio::preferences::Preferences,settings:Option<&std::path::Path>,text:map_label_panel::Text<'_>)->bool {
        if preferences.monument_markers==mapper_studio::map_labels::Mode::None {return true;}
        if preferences.monument_markers==mapper_studio::map_labels::Mode::Text {self.labels.paint(painter,p,text.source,text.name);return true;}
        self.paint_icon(painter,p,path,preferences,settings)
    }
    fn paint_icon(&mut self,painter:&egui::Painter,p:Pos2,path:Option<&str>,preferences:&mapper_studio::preferences::Preferences,settings:Option<&std::path::Path>)->bool {
        let Some(key)=self.artwork_key(path,preferences) else{return false;};
        let Some(id)=self.texture(painter.ctx(),settings,key) else{return false;};
        painter.image(id,Rect::from_center_size(p,Vec2::splat(ICON_SIDE)),Rect::from_min_max(Pos2::ZERO,egui::pos2(1.,1.)),Color32::WHITE);true
    }
    pub fn thumbnail(&mut self,ctx:&egui::Context,path:Option<&str>,preferences:&mapper_studio::preferences::Preferences,settings:Option<&std::path::Path>)->Option<egui::TextureId> {
        let key=self.artwork_key(path,preferences)?;
        self.texture(ctx,settings,key)
    }
    pub(super) fn monument_appearance(
        &mut self,
        ui: &mut egui::Ui,
        path: &str,
        preferences: &mut mapper_studio::preferences::Preferences,
        settings: Option<&std::path::Path>,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            if let Some(id) = self.thumbnail(ui.ctx(), Some(path), preferences, settings) {
                ui.add(egui::Image::new((id, Vec2::splat(32.))));
            }
            let custom = preferences.monument_icons.contains_key(path);
            if ui.add_enabled(settings.is_some() && (custom || preferences.monument_icons.len() < 256),
                egui::Button::new("Choose icon…")).clicked()
                && let Some(file) = rfd::FileDialog::new().add_filter("PNG icon", &["png"]).pick_file()
            {
                let result = settings.ok_or("Settings storage unavailable".into())
                    .and_then(monument_icons::directory)
                    .and_then(|folder| monument_icons::import(&folder, &file));
                match result {
                    Ok(hash) => {
                        self.textures.remove(&hash);
                        preferences.monument_icons.insert(path.into(), hash);
                        self.message.clear();
                        changed = true;
                    }
                    Err(error) => self.message = error,
                }
            }
            if ui.add_enabled(custom, egui::Button::new("Restore default")).clicked() {
                preferences.monument_icons.remove(path);
                self.message.clear();
                changed = true;
            }
        });
        if !self.message.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &self.message);
        }
        changed
    }

    pub fn settings(&mut self,ui:&mut egui::Ui,project:&Project,catalog:Option<&Catalog>,preferences:&mut mapper_studio::preferences::Preferences,settings:Option<&std::path::Path>)->bool {
        let mut changed=false;
        self.textures.retain(|hash,_|preferences.monument_icons.values().any(|v|v==hash));
        ui.horizontal(|ui|{ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search monuments").desired_width((ui.available_width()-95.).max(95.)));
            if ui.add_enabled(!preferences.monument_icons.is_empty(),egui::Button::new("Restore all")).on_hover_text("Remove every custom icon override. Your original image files are kept.").clicked(){preferences.monument_icons.clear();self.textures.clear();changed=true;}});
        let mut rows=BTreeMap::<String,String>::new();
        for catalog in [catalog,self.catalog.as_deref()].into_iter().flatten(){for (path,asset) in &catalog.entries {
            let fallback=path.rsplit('/').next().unwrap_or(path).trim_end_matches(".prefab").replace('_'," ");
            rows.insert(path.clone(),asset.display_name.clone().unwrap_or(fallback));
        }}
        for target in &project.targets {if let Some(path)=&target.prefab_path {rows.entry(path.clone()).or_insert_with(||self.name(Some(path),&target.name));}}
        for path in preferences.monument_icons.keys(){rows.entry(path.clone()).or_insert_with(||path.rsplit('/').next().unwrap_or(path).trim_end_matches(".prefab").replace('_'," "));}
        let mut rows:Vec<_>=rows.into_iter().collect();rows.sort_by(|a,b|a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        let search=self.search.to_lowercase();
        if rows.is_empty() {
            ui.weak("Choose a Rust installation to load its monument list.");
        } else if !rows.iter().any(|(p,n)| p.to_lowercase().contains(&search) || n.to_lowercase().contains(&search)) {
            ui.weak("No monuments match your search.");
        }
        egui::ScrollArea::vertical().id_salt("monument-icon-list").max_height(250.).show(ui,|ui|{
            for (path,name) in rows.iter().filter(|(p,n)|p.to_lowercase().contains(&search)||n.to_lowercase().contains(&search)) {
                ui.push_id(path,|ui|{ui.horizontal(|ui|{
                    let (rect,_)=ui.allocate_exact_size(Vec2::splat(32.),Sense::hover());
                    if !self.paint_icon(ui.painter(),rect.center(),Some(path),preferences,settings){ui.painter().circle_stroke(rect.center(),6.,Stroke::new(1.5,ui.visuals().text_color()));}
                    let text_width=(ui.available_width()-180.).max(65.);
                    ui.allocate_ui_with_layout(Vec2::new(text_width,38.),egui::Layout::top_down(egui::Align::LEFT),|ui|{
                        ui.add(egui::Label::new(name).truncate()).on_hover_text(name);
                        ui.add(egui::Label::new(egui::RichText::new(path.rsplit('/').next().unwrap_or(path).trim_end_matches(".prefab")).small()).truncate()).on_hover_text(path);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                        if ui.add_enabled(preferences.monument_icons.contains_key(path),egui::Button::new("Restore")).clicked(){preferences.monument_icons.remove(path);changed=true;}
                        if ui.add_enabled(settings.is_some() && (preferences.monument_icons.contains_key(path) || preferences.monument_icons.len()<256),egui::Button::new("Choose…")).on_hover_text("PNG with transparency, up to 1024 × 1024 px and 4 MiB. A local copy is saved; the original can be moved later.").clicked()
                            && let Some(file)=rfd::FileDialog::new().add_filter("PNG icon",&["png"]).pick_file() {
                            let result=settings.ok_or("Settings storage unavailable".into()).and_then(monument_icons::directory).and_then(|folder|monument_icons::import(&folder,&file));
                            match result {Ok(hash)=>{self.textures.remove(&hash);preferences.monument_icons.insert(path.clone(),hash);self.message.clear();changed=true;},Err(e)=>self.message=e}
                        }
                    });
                });
                if let Some(hash)=preferences.monument_icons.get(path) && let Some(Err(error))=self.textures.get(hash){ui.colored_label(ui.visuals().error_fg_color,format!("Icon unavailable: {error}"));}
                ui.separator();});
            }
        });
        if self.task.is_some(){ui.small("Loading installation display names…");}
        if !self.message.is_empty(){ui.small(&self.message);}
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PATH:&str="assets/test.prefab";

    #[test]
    fn artwork_choices_cache_hits_and_misses_until_catalog_changes() {
        let ctx = egui::Context::default();
        let mut panel = Panel::default();
        let mut preferences = mapper_studio::preferences::Preferences::default();
        assert!(panel.artwork_key(Some(PATH), &preferences).is_none());
        assert_eq!(panel.builtin_keys.borrow().get(PATH), Some(&None));

        let mut catalog = Catalog {
            sha256: "b".repeat(64),
            installation: Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: mapper_studio::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            },
            entries: Default::default(),
            sources: Vec::new(),
        };
        catalog.entries.insert(PATH.into(), mapper_studio::prefab_assets::Asset {
            priority: None, bounds: None, yaw: None, modifier_size: None, modifier_offset: None,
            texture_guids: Default::default(), splat_mask: None, modifier: None,
            modifier_absent: false, water_geometry: None, no_build_geometry: None,
            display_name: Some("Outpost".into()),
        });
        panel.seed_names(Arc::new(catalog));
        let first = panel.thumbnail(&ctx, Some(PATH), &preferences, None).unwrap();
        assert_eq!(panel.builtin_keys.borrow().get(PATH), Some(&Some("outpost")));
        ctx.tex_manager().write().take_delta().clear();
        for _ in 0..32 {
            assert_eq!(panel.thumbnail(&ctx, Some(PATH), &preferences, None), Some(first));
        }
        let mut delta = ctx.tex_manager().write().take_delta();
        let reused_without_uploads = delta.is_empty();
        delta.clear();
        assert!(reused_without_uploads);

        let override_hash = "a".repeat(64);
        preferences.monument_icons.insert(PATH.into(), override_hash.clone());
        assert!(matches!(panel.artwork_key(Some(PATH), &preferences), Some(ArtworkKey::Custom(key)) if key == override_hash));
        preferences.monument_icons.clear();
        assert_eq!(panel.thumbnail(&ctx, Some(PATH), &preferences, None), Some(first));
        panel.set_catalog(None);
        assert!(panel.artwork_key(Some(PATH), &preferences).is_none());
    }

    #[test]
    fn fallback_marker_hits_follow_the_placed_and_requested_dots() {
        let panel=Panel::default();let preferences=mapper_studio::preferences::Preferences::default();
        let center=egui::pos2(100.,100.);
        let hit=|x,y,requested|panel.marker_hit(center+egui::vec2(x,y),center,Some(PATH),&preferences,requested,Default::default());
        assert!(hit(3.,4.,false));assert!(!hit(4.,4.,false));
        assert!(hit(4.,4.,true));assert!(!hit(6.,6.,true));assert!(!hit(8.,0.,true));
    }

    #[test]
    fn custom_icon_hit_uses_loaded_alpha_and_requested_center_dot() {
        let ctx=egui::Context::default();let mut panel=Panel::default();
        let mut preferences=mapper_studio::preferences::Preferences::default();
        let temp=tempfile::tempdir().unwrap();let source=temp.path().join("source.png");
        let mut rgba=vec![255;64*64*4];
        for y in 0..64 {for x in 32..64 {rgba[(y*64+x)*4+3]=0;}}
        let mut encoder=png::Encoder::new(std::fs::File::create(&source).unwrap(),64,64);
        encoder.set_color(png::ColorType::Rgba);encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&rgba).unwrap();
        let settings=temp.path().join("settings.json");let folder=monument_icons::directory(&settings).unwrap();
        let hash=monument_icons::import(&folder,&source).unwrap();preferences.monument_icons.insert(PATH.into(),hash);
        // Load before picking on the very first frame, just as paint would.
        panel.prepare_hit(&ctx,Some(PATH),&preferences,Some(&settings),Default::default());
        let center=egui::pos2(100.,100.);
        let hit=|x,y,requested|panel.marker_hit(center+egui::vec2(x,y),center,Some(PATH),&preferences,requested,Default::default());
        assert!(hit(-13.,-13.,false));assert!(!hit(11.,11.,false));assert!(!hit(-15.,0.,false));
        assert!(hit(-13.,-13.,true));assert!(!hit(11.,11.,true));
        assert!(!hit(4.,0.,false));assert!(hit(4.,0.,true));
        let painter=ctx.layer_painter(egui::LayerId::background());
        assert!(panel.paint(&painter,center,Some(PATH),&preferences,Some(&settings),Default::default()));
        preferences.monument_icons.clear();
        assert!(!panel.marker_hit(center+egui::vec2(-13.,-13.),center,Some(PATH),&preferences,false,Default::default()));
    }

    #[test]
    fn unreadable_custom_icon_uses_the_drawn_fallback_marker_for_picking() {
        let ctx=egui::Context::default();let mut panel=Panel::default();
        let mut preferences=mapper_studio::preferences::Preferences::default();preferences.monument_icons.insert(PATH.into(),"a".repeat(64));
        panel.prepare_hit(&ctx,Some(PATH),&preferences,None,Default::default());
        assert!(panel.marker_hit(Pos2::ZERO,Pos2::ZERO,Some(PATH),&preferences,false,Default::default()));
        assert!(!panel.marker_hit(egui::pos2(13.,13.),Pos2::ZERO,Some(PATH),&preferences,false,Default::default()));
        assert!(!panel.paint(&ctx.layer_painter(egui::LayerId::background()),Pos2::ZERO,Some(PATH),&preferences,None,Default::default()));
    }
}
