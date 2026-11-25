use super::*;
use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

enum CacheReset {
    Waiting { root: PathBuf, requested: Instant },
    Running(JoinHandle<Result<mapper_studio::client_cache::Reset, String>>),
}

#[derive(Default)]
pub struct Panel {
    names: HashMap<u64, String>,
    reset: Option<CacheReset>,
    pub message: String,
}

impl Drop for Panel {
    fn drop(&mut self) {
        if let Some(CacheReset::Running(worker)) = self.reset.take() {
            // Let an in-flight directory move or rollback finish on shutdown.
            let _ = worker.join();
        }
    }
}

pub enum Action {Select(PathBuf),Forget,Reset}
impl Panel {
    pub fn resetting(&self) -> bool {
        self.reset.is_some()
    }

    pub fn request_reset(&mut self, root: PathBuf, ctx: &egui::Context) {
        self.reset = Some(CacheReset::Waiting { root, requested: Instant::now() });
        self.message.clear();
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    /// Returns true once the reset finishes so the installation can be loaded again.
    pub fn poll_reset(&mut self, ctx: &egui::Context) -> bool {
        match self.reset.as_ref() {
            None => return false,
            Some(CacheReset::Waiting { root, requested }) => {
                if requested.elapsed() >= Duration::from_millis(150)
                    && mapper_studio::loading_progress::active().is_empty()
                {
                    let root = root.clone();
                    self.reset = Some(CacheReset::Running(std::thread::spawn(move || {
                        mapper_studio::installation::clear_verification_cache();
                        mapper_studio::client_cache::reset_in(&root)
                    })));
                }
            }
            Some(CacheReset::Running(worker)) if worker.is_finished() => {
                if let Some(CacheReset::Running(worker)) = self.reset.take() {
                    self.message = match worker.join() {
                        Ok(Ok(report)) if report.directories == 0 => "Asset cache is already empty.".into(),
                        Ok(Ok(report)) => format!("Archived {} asset cache groups.", report.directories),
                        Ok(Err(error)) => error,
                        Err(_) => "Asset cache reset worker stopped.".into(),
                    };
                    return true;
                }
            }
            Some(CacheReset::Running(_)) => {}
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        false
    }

    pub fn controls(&mut self,ui:&mut egui::Ui,prefs:&mut mapper_studio::preferences::Preferences,busy:bool)->(bool,Option<Action>){
        let mut changed=false;let mut action=None;
        let entries=prefs.installations.clone();let mut remove=None;
        egui::ScrollArea::vertical().id_salt("rust-installations").max_height(210.).show(ui,|ui|{
            for entry in &entries {ui.push_id(entry.id,|ui|{
                ui.horizontal(|ui|{
                    let name=self.names.entry(entry.id).or_insert_with(||entry.name.clone());
                    let edit=ui.add(egui::TextEdit::singleline(name).char_limit(80).desired_width((ui.available_width()-160.).max(95.)));
                    if edit.lost_focus() {let clean=name.trim().to_owned();
                        if !clean.is_empty()&&!clean.chars().any(char::is_control) && let Some(saved)=prefs.installations.iter_mut().find(|e|e.id==entry.id) {
                            if saved.name!=clean {saved.name=clean.clone();changed=true;}*name=clean;
                        }else{*name=entry.name.clone();}
                    }
                    let active=prefs.selected_installation==Some(entry.id);
                    if ui.add_enabled(!busy&&!active,egui::Button::new(if active{"Selected"}else{"Use"})).clicked()
                        && let Ok(path)=prefs.select_installation(entry.id){changed=true;action=Some(Action::Select(path));}
                    if ui.add_enabled(!busy,egui::Button::new("Remove")).on_hover_text("Forget this registration. The Rust folder and its game files are kept.").clicked(){remove=Some(entry.id);}
                });
                ui.add(egui::Label::new(egui::RichText::new(entry.path.display().to_string()).small()).truncate()).on_hover_text(entry.path.display().to_string());
                ui.separator();
            });}
        });
        if let Some(id)=remove {if prefs.forget_installation(id){action=Some(Action::Forget);}self.names.remove(&id);changed=true;}
        ui.horizontal_wrapped(|ui|{
            if ui.add_enabled(!busy,egui::Button::new("Add Rust folder…")).clicked()
                && let Some(path)=rfd::FileDialog::new().set_title("Add Rust client or dedicated-server folder").pick_folder(){
                let select=prefs.rust_folder.is_none();
                match prefs.remember_installation(path,select){Ok(id)=>{changed=true;if select {action=Some(Action::Select(prefs.select_installation(id).unwrap()));}},Err(e)=>self.message=e}
            }
            if ui.add_enabled(!busy&&self.reset.is_none(),egui::Button::new("Reset cache")).on_hover_text("Rebuild extracted data shared by all Rust installations. Archives previous snapshots and preserves maps, recipes, icons, terrain variants and saved source pins.").clicked(){action=Some(Action::Reset);}
        });
        if prefs.installations.is_empty(){ui.small("Add regular Rust, staging, or a retained older build.");}
        if let Some(reset) = &self.reset {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small(match reset {
                    CacheReset::Waiting { .. } => "Waiting for asset jobs…",
                    CacheReset::Running(_) => "Resetting asset cache…",
                });
            });
        }
        if !self.message.is_empty(){ui.small(&self.message);}
        (changed,action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_restore_waits_for_cache_archive_to_finish() {
        let ctx = egui::Context::default();
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("papyrus-prefab-cache");
        std::fs::create_dir(&cache).unwrap();
        let root = directory.path().to_owned();
        let rust = root.join("rust");
        let mut app = crate::tests::fixture(&ctx);
        app.restore_source = true;
        app.preferences.rust_folder = Some(rust.clone());
        let (release, wait) = std::sync::mpsc::channel();
        app.installation_library.reset = Some(CacheReset::Running(std::thread::spawn(move || {
            wait.recv().map_err(|error| error.to_string())?;
            mapper_studio::client_cache::reset_in(&root)
        })));

        app.tick_preferences(&ctx);
        assert!(app.installation_library.resetting());
        assert!(app.restore_source && !app.source.inspecting());
        assert!(cache.is_dir());
        release.send(()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while app.installation_library.resetting() && Instant::now() < deadline {
            app.tick_preferences(&ctx);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!app.installation_library.resetting());
        assert!(!app.restore_source && app.source.inspecting());
        assert_eq!(app.source.selected_root().as_deref(), Some(rust.as_path()));
        assert!(!cache.exists());
        assert!(app.installation_library.message.contains("Archived 1 asset cache"));
    }
}
