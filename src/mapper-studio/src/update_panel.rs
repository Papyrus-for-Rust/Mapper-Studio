use eframe::egui;
use mapper_studio::updates::{Channel, Prepared, Release};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

#[derive(Default)]
pub(super) struct Panel {
    channel: Option<Channel>,
    open: bool,
    task: Option<Task>,
    release: Option<Release>,
    prepared: Option<Prepared>,
    message: String,
}

enum ResultState {
    Checked(Option<Release>),
    Downloaded(Option<Prepared>),
}

struct Task {
    result: Receiver<Result<ResultState, String>>,
    stop: Arc<AtomicBool>,
    progress: &'static str,
}

impl Drop for Task {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Panel {
    pub fn new() -> Self {
        Self {
            channel: Channel::installed(),
            ..Default::default()
        }
    }

    pub fn version(&self) -> Option<&str> {
        self.channel
            .as_ref()
            .map(|channel| channel.version.as_str())
    }

    pub fn awaiting_restart(&self) -> bool {
        self.prepared.is_some()
    }

    pub fn check(&mut self, ctx: &egui::Context, manual: bool) {
        self.open |= manual;
        if self.task.is_some() || self.prepared.is_some() || self.release.is_some() {
            return;
        }
        let Some(channel) = self.channel.clone() else {
            self.message = "This build has no update channel.".into();
            return;
        };
        self.start(ctx, "Checking GitHub…", move |cancelled| {
            channel.check(cancelled).map(ResultState::Checked)
        });
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        progress: &'static str,
        work: impl FnOnce(&dyn Fn() -> bool) -> Result<ResultState, String> + Send + 'static,
    ) {
        self.message.clear();
        let (sender, result) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("mapper-updates".into())
            .spawn(move || {
                let _ = sender.send(work(&|| signal.load(Ordering::Relaxed)));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.task = Some(Task {
                    result,
                    stop,
                    progress,
                })
            }
            Err(error) => self.message = error.to_string(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, project: Option<&Path>, can_restart: bool) {
        if let Some(task) = &self.task {
            let received = match task.result.try_recv() {
                Ok(value) => Some(value),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Update worker stopped.".into())),
            };
            if let Some(received) = received {
                self.task = None;
                match received {
                    Ok(ResultState::Checked(Some(release))) => {
                        self.open = true;
                        self.release = Some(release);
                    }
                    Ok(ResultState::Checked(None)) => self.message = "Mapper is up to date.".into(),
                    Ok(ResultState::Downloaded(prepared)) => self.prepared = prepared,
                    Err(error) => self.message = error,
                }
            }
        }
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Mapper updates")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(330.)
            .show(ctx, |ui| {
                if let Some(task) = &self.task {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(task.progress);
                    });
                    if ui.button("Cancel").clicked() {
                        self.task = None;
                        self.message = "Update cancelled.".into();
                    }
                } else if let Some(prepared) = &mut self.prepared {
                    ui.label(format!("Mapper {} is ready.", prepared.version()));
                    if ui
                        .add_enabled(can_restart, egui::Button::new("Restart Mapper"))
                        .on_disabled_hover_text(
                            "Save your work and finish generation before restarting.",
                        )
                        .clicked()
                    {
                        match prepared.restart(project) {
                            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                            Err(error) => self.message = error,
                        }
                    }
                } else if let Some(release) = &self.release {
                    ui.label(format!("Mapper {} is available.", release.version));
                    if ui.button("Download update").clicked()
                        && let Some(channel) = self.channel.clone()
                    {
                        let release = release.clone();
                        self.start(ctx, "Downloading and verifying…", move |cancelled| {
                            channel
                                .prepare(&release, cancelled)
                                .map(ResultState::Downloaded)
                        });
                    }
                } else if self.channel.is_some() && ui.button("Check now").clicked() {
                    self.check(ctx, true);
                }
                if !self.message.is_empty() {
                    ui.label(&self.message);
                }
            });
        if !open {
            self.open = false;
            self.task = None;
            self.release = None;
            self.prepared = None;
            self.message.clear();
        }
    }
}
