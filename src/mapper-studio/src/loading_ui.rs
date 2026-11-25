use eframe::egui;
use mapper_studio::loading_progress::{self, Snapshot, Unit};
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Clone, Copy)]
pub(super) enum Status<'a> {
    Ready,
    Blocked(&'a str),
    Preparing(&'a Arc<AtomicBool>),
    Generating {
        stage: &'a str,
        fraction: Option<f32>,
        batch: Option<(usize, usize)>,
    },
}

pub(super) fn show(ui: &mut egui::Ui, status: Status<'_>) {
    if let Status::Preparing(token) = status {
        let jobs = loading_progress::active_for(token);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
        panel(ui, &jobs, status);
    } else {
        panel(ui, &[], status);
    }
}

fn panel(ui: &mut egui::Ui, setup: &[Snapshot], status: Status<'_>) {
    match status {
        Status::Ready => {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8., 8.), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.5, if ui.visuals().dark_mode {
                egui::Color32::from_rgb(117, 190, 151)
            } else {
                egui::Color32::from_rgb(50, 115, 83)
            });
            ui.weak("Ready");
        }
        Status::Blocked(error) => {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(crate::diagnostic_ui::summary(error).as_ref())
                        .color(ui.visuals().warn_fg_color),
                )
                .truncate(),
            )
            .on_hover_text(error);
        }
        Status::Preparing(_) => {
            if let Some((job, workers)) = setup.split_first() {
                setup_line(ui, job, workers);
            } else {
                progress_line(ui, "Preparing Rust installation", None);
            }
        }
        Status::Generating {
            stage,
            fraction,
            batch,
            ..
        } => {
            if let Some((current, total)) = batch.filter(|(_, total)| *total > 1) {
                progress_line(ui, &format!("Map {current} of {total} · {stage}"), fraction);
            } else {
                progress_line(ui, stage, fraction);
            }
        }
    }
}

fn setup_line(ui: &mut egui::Ui, job: &Snapshot, workers: &[Snapshot]) {
    let stage = if job.cancelling {
        "Cancelling setup…"
    } else {
        &job.stage
    };
    let fraction = if workers.is_empty() && !job.cancelling {
        job.fraction()
    } else {
        None
    };
    let bar = fraction.filter(|_| ui.available_width() >= 225.);
    let spinner = fraction.is_none() && ui.available_width() >= 55.;
    let mut available = ui.available_rect_before_wrap();
    if bar.is_some() {
        available.max.x -= 76. + ui.spacing().item_spacing.x;
    } else if spinner {
        available.max.x -= 12. + ui.spacing().item_spacing.x;
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(available), |ui| {
        ui.push_id(job.id, |ui| {
            let button = egui::Button::new(stage).truncate();
            let (response, _) = egui::containers::menu::MenuButton::from_button(button)
                .ui(ui, |ui| {
                    ui.set_width(280.);
                    if workers.is_empty() {
                        details(ui, job);
                    } else {
                        for worker in workers {
                            ui.strong(&worker.label);
                            if worker.stage != worker.label {
                                ui.label(&worker.stage);
                            }
                            details(ui, worker);
                        }
                    }
                    cancel_button(ui, job);
                });
            response.on_hover_ui(|ui| {
                ui.label(stage);
                ui.weak("View setup details");
            });
        });
    });
    // The byte or item total describes the current operation only.
    if let Some(value) = bar {
        ui.add(
            egui::ProgressBar::new(value)
                .desired_width(76.)
                .desired_height(10.),
        );
    } else if spinner {
        ui.add(egui::Spinner::new().size(12.));
    }
}

fn progress_line(ui: &mut egui::Ui, caption: &str, fraction: Option<f32>) {
    let fraction = fraction
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0., 1.));
    if fraction.is_none() {
        ui.add(egui::Spinner::new().size(12.));
    }
    let bar = fraction.filter(|_| ui.available_width() >= 135.);
    let mut available = ui.available_rect_before_wrap();
    if bar.is_some() {
        available.max.x -= 76. + ui.spacing().item_spacing.x;
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(available), |ui| {
        ui.add(egui::Label::new(caption).truncate())
            .on_hover_text(caption);
    });
    if let Some(value) = bar {
        ui.add(
            egui::ProgressBar::new(value)
                .desired_width(76.)
                .desired_height(16.)
                .show_percentage(),
        );
    }
}

fn cancel_button(ui: &mut egui::Ui, job: &Snapshot) {
    if job.can_cancel
        && ui
            .add_enabled(
                !job.cancelling,
                egui::Button::new(if job.cancelling {
                    "Cancelling…"
                } else {
                    "Cancel"
                }),
            )
            .on_hover_ui(|ui| {
                ui.label(format!("Cancel {}", job.label));
            })
            .clicked()
    {
        loading_progress::cancel(job.id);
    }
}

fn details(ui: &mut egui::Ui, job: &Snapshot) {
    if let Some(fraction) = job.fraction() {
        let total = job.total.unwrap_or(0);
        let count = match job.unit {
            Unit::Bytes => format!(
                "{:.1} / {:.1} MiB",
                job.completed as f64 / 1048576.,
                total as f64 / 1048576.
            ),
            Unit::Items => format!("{} / {} items", job.completed, total),
        };
        ui.add(egui::ProgressBar::new(fraction).text(count));
    }
    if !job.item.is_empty() {
        ui.add(egui::Label::new(&job.item).truncate())
            .on_hover_text(&job.item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }
    #[test]
    fn blocked_generation_is_visible_and_long_errors_fit_the_status_row() {
        let issue = "Choose the matching Rust installation in Settings. ".repeat(40);
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            for width in [320., 1050.] {
                let mut remaining = Vec::new();
                for (status, expected) in [
                    (Status::Ready, "Ready".into()),
                    (
                        Status::Blocked(&issue),
                        crate::diagnostic_ui::summary(&issue),
                    ),
                ] {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 150.),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            egui::Panel::bottom("readiness-row")
                                .exact_size(36.)
                                .show(ui, |ui| {
                                    let available = ui.available_rect_before_wrap();
                                    ui.horizontal(|ui| show(ui, status));
                                    assert!(ui.min_rect().right() <= available.right() + 1.);
                                });
                            remaining.push(ui.available_rect_before_wrap());
                        },
                    );
                    assert!(output.shapes.iter().any(|shape| {
                        matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == expected.as_ref())
                    }));
                    output.textures_delta.clear();
                }
                assert_eq!(remaining[0], remaining[1]);
            }
        }
    }
    #[test]
    fn background_jobs_leave_ready_and_viewport_space_unchanged() {
        let ctx = egui::Context::default();
        let mut remaining = Vec::new();
        let mut positions = Vec::new();
        for background in [false, true, false] {
            let _job = background.then(|| loading_progress::begin("Reading textures", None));
            let mut output = ctx.run_ui(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::Panel::bottom("test-generation-actions")
                        .exact_size(44.)
                        .resizable(false)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| show(ui, Status::Ready));
                        });
                    remaining.push(ui.available_rect_before_wrap());
                });
            });
            output.textures_delta.clear();
            let labels: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.job.text.as_str(), text.pos)),
                _ => None,
            }).collect();
            assert_eq!(labels.len(), 1, "{labels:?}");
            assert_eq!(labels[0].0, "Ready");
            positions.push(labels[0].1);
        }
        assert_eq!(remaining[0], remaining[1]);
        assert_eq!(remaining[1], remaining[2]);
        assert_eq!(positions[0], positions[1]);
        assert_eq!(positions[1], positions[2]);
    }

    #[test]
    fn active_composition_progress_uses_the_footer_without_a_duplicate_menu() {
        let token = Arc::new(AtomicBool::new(false));
        let ctx = egui::Context::default();
        let progress = loading_progress::begin("Composing map", Some(token.clone()));
        progress.reporter().stage("Simulating erosion");
        progress.reporter().update("", 3, Some(4), Unit::Items);
        let (jobs, primary) = loading_progress::active_with_priority(Some(&token));
        let job = &jobs[primary.unwrap()];
        let mut output = ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| show(
                    ui,
                    Status::Generating {
                        stage: &job.stage,
                        fraction: job.fraction(),
                        batch: None,
                    },
                ));
            });
        });
        output.textures_delta.clear();
        let labels = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        }).collect::<Vec<_>>();
        assert!(labels.contains(&"Simulating erosion"));
        assert!(labels.contains(&"75%"));
        assert!(!labels.contains(&"Setup details"));
    }

    #[test]
    fn background_work_keeps_the_generation_stage_and_blocker_visible() {
        let token = Arc::new(AtomicBool::new(false));
        let optional = Snapshot {
            id: 1,
            label: "Terrain colors".into(),
            stage: "Reading map palette".into(),
            item: String::new(),
            completed: 0,
            total: None,
            unit: Unit::Items,
            elapsed: Default::default(),
            can_cancel: true,
            cancelling: false,
        };
        let setup = Snapshot {
            id: 2,
            label: "Rust installation".into(),
            stage: "Checking Rust files".into(),
            item: "assetscenes.bundle".into(),
            completed: 35,
            total: Some(100),
            ..optional.clone()
        };
        for (width, dark) in [(320., false), (320., true), (1050., false), (1050., true)] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let mut textures = snapshot::Textures::default();
            for (status, job, expected) in [
                (Status::Ready, None, "Ready"),
                (
                    Status::Blocked("Choose the matching Rust installation"),
                    None,
                    "Choose the matching Rust installation",
                ),
                (
                    Status::Preparing(&token),
                    None,
                    "Preparing Rust installation",
                ),
                (Status::Preparing(&token), Some(&setup), "Checking Rust files"),
                (
                    Status::Generating {
                        stage: "Composing map",
                        fraction: Some(0.5),
                        batch: Some((2, 4)),
                    },
                    None,
                    "Map 2 of 4 · Composing map",
                ),
            ] {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 180.),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        egui::Panel::bottom("test-action-bar")
                            .exact_size(36.)
                            .show(ui, |ui| {
                                let jobs = job.map(std::slice::from_ref).unwrap_or_default();
                                let right = ui.max_rect().right();
                                ui.horizontal(|ui| {
                                    if width < 400. && job.is_some() {
                                        ui.add_space(160.);
                                    }
                                    panel(ui, jobs, status);
                                });
                                assert!(ui.min_rect().right() <= right + 1.);
                            });
                    },
                );
                textures.update(&output.textures_delta);
                output.textures_delta.clear();
                let labels: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert!(labels.contains(&expected), "{labels:?}");
                assert!(!labels.contains(&"Setup details"), "{labels:?}");
                assert!(!labels.contains(&"35%"), "{labels:?}");
                assert!(!labels.contains(&"assetscenes.bundle"), "{labels:?}");
                assert!(!labels.contains(&"Reading map palette"), "{labels:?}");
                assert!(!labels.contains(&"Cancel loading"), "{labels:?}");
                if let Some(directory) = std::env::var_os("MAPPER_SETUP_SNAPSHOTS")
                    && job.is_some()
                {
                    let image = textures.raster(&ctx, output, [width as usize, 180]);
                    let theme = if dark { "dark" } else { "light" };
                    let path = std::path::PathBuf::from(directory)
                        .join(format!("setup-{width}-{theme}.png"));
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
            }
        }
    }

    #[test]
    fn setup_details_show_parallel_workers_and_cancel_the_owner() {
        let token = Arc::new(AtomicBool::new(false));
        let parent = loading_progress::begin("Rust installation", Some(token.clone()));
        parent.reporter().stage("Preparing generation data");
        let mut jobs = loading_progress::active_for(&token);
        let primary = jobs[0].clone();
        jobs.extend([
            Snapshot {
                id: primary.id + 1,
                label: "Monument data".into(),
                stage: "Preparing no-build zones".into(),
                completed: 9,
                total: Some(24),
                ..primary.clone()
            },
            Snapshot {
                id: primary.id + 2,
                label: "Paths and scenery".into(),
                stage: "Preparing roads and dirt trails".into(),
                ..primary
            },
        ]);
        let ctx = egui::Context::default();
        crate::workbench_style::install(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.);
        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(520., 360.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::bottom("cancel-task-footer")
                        .exact_size(36.)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                panel(ui, &jobs, Status::Preparing(&token))
                            });
                        });
                },
            );
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let click = |name, output: &egui::FullOutput| {
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == name => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Missing {name} control"));
            let event = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            render(vec![egui::Event::PointerMoved(pos), event(true)]);
            render(vec![event(false)]);
        };
        render(vec![]);
        let closed = render(vec![]);
        click("Preparing generation data", &closed);
        let open = render(vec![]);
        let labels: Vec<_> = open.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        }).collect();
        assert!(labels.contains(&"Monument data"), "{labels:?}");
        assert!(labels.contains(&"Preparing no-build zones"), "{labels:?}");
        assert!(labels.contains(&"Paths and scenery"), "{labels:?}");
        assert!(labels.contains(&"Preparing roads and dirt trails"), "{labels:?}");
        assert_eq!(
            labels.iter().filter(|&&label| label == "Preparing generation data").count(),
            1,
        );
        assert!(!labels.contains(&"37%"), "{labels:?}");
        assert_eq!(labels.iter().filter(|&&label| label == "Cancel").count(), 1);
        click("Cancel", &open);
        assert!(token.load(std::sync::atomic::Ordering::Relaxed));
        if let Some(directory) = std::env::var_os("MAPPER_SETUP_SNAPSHOTS") {
            for (output, name) in [(open, "setup-details.png"), (closed, "setup-parallel.png")] {
                let image = textures.borrow().raster(&ctx, output, [520, 360]);
                let path = std::path::PathBuf::from(&directory).join(name);
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        }
    }
}
