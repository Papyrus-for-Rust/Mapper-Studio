use eframe::egui;
use mapper_studio::{
    Project,
    erosion_compute::{self, Recommendation},
    generation_resources::Limits,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

#[derive(Default)]
pub(super) struct Comparison {
    input: Option<(u32, Limits)>,
    task: Option<Task>,
    result: Option<Result<Recommendation, String>>,
}

#[cfg(test)]
#[test]
fn changing_the_map_size_cancels_pending_comparison_work() {
    let mut comparison = Comparison::default();
    let limits = Limits::default();
    comparison.sync(4000, limits);
    let (send, result) = mpsc::channel();
    let signal = Arc::new(AtomicBool::new(false));
    comparison.task = Some(Task {
        cancel: signal.clone(),
        result,
    });
    comparison.sync(3000, limits);
    assert!(signal.load(Ordering::Relaxed));
    assert!(send.send(Ok(None)).is_err());
    assert!(comparison.result.is_none());
}

struct Task {
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Option<Recommendation>, String>>,
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Comparison {
    pub(super) fn cancel(&mut self) {
        self.task = None;
    }

    pub(super) fn sync(&mut self, size: u32, limits: Limits) {
        let input = Some((size, limits));
        if self.input != input {
            self.cancel();
            self.result = None;
            self.input = input;
        }
        let Some(task) = &self.task else { return };
        let report = match task.result.try_recv() {
            Ok(report) => report,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Comparison stopped".into()),
        };
        self.cancel();
        self.result = report.transpose().filter(|result| match result {
            Ok(report) => {
                report.cpu_workers == limits.effective_workers()
                    && report.gpu_memory_limit_mib == limits.gpu_memory_mib
                    && report.terrain_memory_limit_mib == limits.terrain_memory_mib
            }
            Err(_) => true,
        });
    }

    pub(super) fn action(&mut self, ui: &mut egui::Ui, busy: bool) {
        if self.task.is_some() {
            if ui
                .button("Cancel")
                .on_hover_text("Cancel erosion comparison")
                .clicked()
            {
                self.cancel();
            }
        } else if ui
            .add_enabled(!busy, egui::Button::new("Run benchmark"))
            .on_hover_text("Measure CPU and GPU erosion for this map size.")
            .on_disabled_hover_text("Finish the current generation to compare erosion.")
            .clicked()
        {
            self.start(ui.ctx());
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some((size, _)) = self.input else { return };
        self.result = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let (sender, result) = mpsc::channel();
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("mapper-erosion-comparison".into())
            .spawn(move || {
                let report = erosion_compute::recommend(size, || signal.load(Ordering::Relaxed));
                let _ = sender.send(report);
                ctx.request_repaint();
            }) {
            Ok(_) => self.task = Some(Task { cancel, result }),
            Err(error) => self.result = Some(Err(error.to_string())),
        }
    }

    pub(super) fn result(&self, ui: &mut egui::Ui, project: &mut Project) {
        if self.task.is_some() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.));
                ui.weak("Comparing CPU and GPU…");
            });
        }
        match &self.result {
            Some(Ok(report)) => {
                let gpu = report.gpu.as_ref().filter(|_| report.prefer_gpu);
                let backend = if gpu.is_some() { "GPU" } else { "CPU" };
                let response = if project.gpu_erosion.as_ref() == gpu {
                    ui.weak(format!("{backend} recommended"))
                } else {
                    let response = ui.button(format!("Use recommended {backend}"));
                    if response.clicked() {
                        project.gpu_erosion = gpu.cloned();
                    }
                    response
                };
                response.on_hover_ui(|ui| {
                    ui.set_max_width(265.);
                    ui.label(&report.reason);
                    if let (Some(cpu), Some(gpu)) =
                        (report.cpu_estimated_seconds, report.gpu_estimated_seconds)
                    {
                        ui.label(format!(
                            "Estimated {} iterations\nCPU {cpu:.2} s · GPU {gpu:.2} s",
                            report.estimate_iterations,
                        ));
                    }
                });
            }
            Some(Err(error)) => {
                ui.weak("Comparison failed").on_hover_text(error);
            }
            None => {}
        }
    }

    pub(super) fn required_gpu_memory(&self, limits: Limits) -> Option<u64> {
        match &self.result {
            Some(Ok(report)) if report.required_gpu_memory_mib > limits.gpu_memory_mib as u64 => {
                Some(report.required_gpu_memory_mib)
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn complete_test(&mut self, gpu: erosion_compute::Identity, limits: Limits) {
        let (send, result) = mpsc::channel();
        self.task = Some(Task {
            cancel: Arc::new(AtomicBool::new(false)),
            result,
        });
        send.send(Ok(Some(Recommendation {
            resolution: 4097,
            cpu_workers: limits.effective_workers(),
            gpu_memory_limit_mib: limits.gpu_memory_mib,
            terrain_memory_limit_mib: limits.terrain_memory_mib,
            required_gpu_memory_mib: 897,
            gpu: Some(gpu),
            cpu_sample_seconds: Some(3.8),
            gpu_sample_seconds: Some(0.5),
            cpu_estimated_seconds: Some(30.),
            gpu_estimated_seconds: Some(2.3),
            sample_iterations: 64,
            estimate_iterations: 512,
            prefer_gpu: true,
            reason: "GPU was faster at the requested erosion resolution".into(),
        })))
        .unwrap();
    }
}
