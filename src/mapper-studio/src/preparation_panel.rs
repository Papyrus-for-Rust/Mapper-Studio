use super::preview::Preview;
use eframe::egui;
use mapper_studio::{MonumentTarget, Project, placement_preview};
use papyrus_terrain::{
    placement::Position,
    preparation::{self, Assessment, Footprint},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

#[derive(Clone, PartialEq)]
struct Context {
    recipe: Project,
    id: u64,
    x: f32,
    z: f32,
    yaw: f32,
    fade: f32,
    maximum_change: f32,
    generated: bool,
    bounds: [f32; 2],
    catalog_sha256: Option<String>,
}
struct Task {
    context: Context,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Assessment>, String>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub(super) struct PreparationPanel {
    fade: f32,
    maximum_change: f32,
    generated: bool,
    task: Option<Task>,
    result: Option<(Context, Assessment)>,
    message: String,
    context: Option<Context>,
}
impl Default for PreparationPanel {
    fn default() -> Self {
        Self {
            fade: 24.,
            maximum_change: 20.,
            generated: false,
            task: None,
            result: None,
            message: String::new(),
            context: None,
        }
    }
}
impl PreparationPanel {
    pub fn tick(&mut self) {
        if self.task.is_some()
            && let Some(context) = self.context.clone()
        {
            self.poll(&context);
        }
    }
    fn poll(&mut self, context: &Context) {
        let Some(task) = &self.task else {
            return;
        };
        if task.context != *context {
            task.cancel.store(true, Ordering::Relaxed);
        }
        let response = match task.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Assessment worker stopped".into()),
        };
        let task = self.task.take().unwrap();
        if task.context != *context || task.cancel.load(Ordering::Relaxed) {
            self.message =
                "Assessment discarded; inputs changed or cancellation was requested.".into();
            return;
        }
        match response {
            Ok(Some(report)) => {
                self.result = Some((context.clone(), report));
                self.message = "Assessment complete; terrain unchanged.".into();
            }
            Ok(None) => self.message = "Assessment cancelled; terrain unchanged.".into(),
            Err(error) => self.message = error,
        }
    }
    #[allow(clippy::too_many_arguments)] // Session analysis inputs are separate from the saved recipe.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        preview: Option<&Preview>,
        target: Option<&MonumentTarget>,
        position: Option<&Position>,
        idle: bool,
        assets: Option<&mapper_studio::prefab_assets::Catalog>,
    ) {
        let Some(target) = target else {
            ui.small("Select a monument target.");
            return;
        };
        let catalog = placement_preview::project_templates(project, assets);
        let Some(template) = catalog
            .iter()
            .find(|e| Some(&e.path) == target.prefab_path.as_ref())
        else {
            ui.small("No supported footprint estimate for this target.");
            return;
        };
        let authored = assets
            .and_then(|catalog| {
                target
                    .prefab_path
                    .as_ref()
                    .and_then(|p| catalog.entries.get(p))
            })
            .and_then(|asset| asset.bounds.as_ref());
        let extents = authored.map_or([template.entry.extent_x, template.entry.extent_z], |b| {
            b.horizontal_extent()
        });
        ui.small(if authored.is_some() {
            "Read-only assessment using source-verified authored bounds."
        } else {
            "Read-only assessment using estimated monument bounds."
        });
        ui.small(format!(
            "Horizontal half-extents: {:.1} × {:.1} m",
            extents[0], extents[1]
        ));
        let size_allowed = authored.is_none_or(|b| project.size >= b.minimum_world_size);
        if let Some(bounds) = authored
            && !size_allowed
        {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "Authored minimum world size: {} m",
                    bounds.minimum_world_size
                ),
            );
        }
        ui.add_enabled(
            position.is_some(),
            egui::Checkbox::new(&mut self.generated, "Assess generated position"),
        );
        ui.horizontal(|ui| {
            ui.label("Blend (m)");
            ui.add(egui::DragValue::new(&mut self.fade).range(1. ..=128.));
        });
        ui.horizontal(|ui| {
            ui.label("Cut/fill limit (m)");
            ui.add(egui::DragValue::new(&mut self.maximum_change).range(0.1..=100.));
        });
        let chosen = position.filter(|_| self.generated);
        let (x, z, yaw) = chosen.map_or(
            (
                target.x,
                target.z,
                target.yaw.unwrap_or(template.entry.minimum_yaw),
            ),
            |p| (p.x, p.z, p.yaw),
        );
        let context = Context {
            recipe: project.clone(),
            id: target.instance_id,
            x,
            z,
            yaw,
            fade: self.fade,
            maximum_change: self.maximum_change,
            generated: chosen.is_some(),
            bounds: extents,
            catalog_sha256: assets
                .filter(|_| authored.is_some())
                .map(|c| c.sha256.clone()),
        };
        if self.context.as_ref() != Some(&context) {
            self.message.clear();
            self.context = Some(context.clone());
        }
        self.poll(&context);
        let layers = preview
            .filter(|p| p.matches_project(project))
            .and_then(|p| p.native_layers.as_ref());
        let ready = idle && layers.is_some() && self.task.is_none() && size_allowed;
        ui.small(if context.generated {
            "Location: generated center"
        } else {
            "Location: preferred center"
        });
        if ui
            .add_enabled(ready, egui::Button::new("Assess cut and fill"))
            .clicked()
        {
            let layers = layers.unwrap().clone();
            let (tx, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            let ctx = ui.ctx().clone();
            let footprint = Footprint {
                x,
                z,
                yaw,
                extent_x: extents[0],
                extent_z: extents[1],
                fade: self.fade,
                maximum_change: self.maximum_change,
            };
            std::thread::spawn(move || {
                let report = preparation::assess(
                    &layers.height,
                    layers.height_resolution,
                    layers.size as usize,
                    footprint,
                    || signal.load(Ordering::Relaxed),
                );
                let _ = tx.send(report);
                ctx.request_repaint();
            });
            self.task = Some(Task {
                context: context.clone(),
                cancel,
                receiver,
            });
            self.message = "Assessing full-resolution terrain…".into();
        }
        if layers.is_none() {
            ui.small("Build matching installed Rust terrain to assess this footprint.");
        }
        if let Some(task) = &self.task {
            ui.spinner();
            if ui.button("Cancel assessment").clicked() {
                task.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some((saved, report)) = &self.result {
            if saved == &context && layers.is_some() {
                ui.label(if report.within_budget {
                    "Within this earthwork budget"
                } else {
                    "Exceeds earthwork or coastal limits"
                });
                ui.label(format!(
                    "Plane {:.1} m · Ground {:.1}–{:.1} m",
                    report.plane_height, report.minimum_height, report.maximum_height
                ));
                ui.label(format!(
                    "Cut up to {:.1} m · Fill up to {:.1} m",
                    report.maximum_cut, report.maximum_fill
                ));
                ui.small(format!(
                    "Estimated cut {:.0} m³ · fill {:.0} m³",
                    report.cut_volume, report.fill_volume
                ));
                ui.small(format!(
                    "{} low core samples · {} submerged blend samples",
                    report.low_core_samples, report.submerged_blend_samples
                ));
                ui.small("Proposed plane uses current center elevation. Verified prefab stamps, placement rules and game checks are still required.");
            } else {
                ui.small("Assessment is stale; assess the current inputs again.");
            }
        }
        if !self.message.is_empty() {
            ui.small(&self.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> Context {
        Context {
            recipe: Project::default(),
            id: 1,
            x: 0.,
            z: 0.,
            yaw: 0.,
            fade: 24.,
            maximum_change: 20.,
            generated: false,
            bounds: [150., 150.],
            catalog_sha256: None,
        }
    }
    #[test]
    fn cancelled_or_changed_context_cannot_publish_an_assessment() {
        for cancelled in [false, true] {
            let mut panel = PreparationPanel::default();
            let original = context();
            let (tx, receiver) = mpsc::channel();
            panel.task = Some(Task {
                context: original.clone(),
                cancel: Arc::new(AtomicBool::new(cancelled)),
                receiver,
            });
            tx.send(Err("old error".into())).unwrap();
            let mut current = original;
            if !cancelled {
                current.x = 1.;
            }
            panel.poll(&current);
            assert!(panel.task.is_none());
            assert!(panel.result.is_none());
            assert!(panel.message.contains("discarded"));
        }
    }
}
