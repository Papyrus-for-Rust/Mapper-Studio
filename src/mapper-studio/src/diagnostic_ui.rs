//! Bound routine layout work while retaining the complete worker diagnostic.
use eframe::egui;
use mapper_studio::{
    Project, installation::Installation, native_profile::Profile, support_snapshot::Snapshot,
};
use std::borrow::Cow;

#[derive(Clone)]
pub(super) struct SupportEnvironment {
    pub version: String,
    pub installation: Option<Installation>,
    pub profile: Option<Profile>,
}

impl SupportEnvironment {
    pub fn snapshot(&self, recipe: Option<&Project>) -> Snapshot {
        let (installation, profile) = match recipe {
            Some(recipe) => (recipe.installation.as_ref(), recipe.native_profile.as_ref()),
            None => (self.installation.as_ref(), self.profile.as_ref()),
        };
        Snapshot::capture(&self.version, installation, profile)
    }
}

fn support_environment_id() -> egui::Id {
    egui::Id::new("mapper-support-environment")
}

pub(super) fn set_support_environment(ctx: &egui::Context, environment: SupportEnvironment) {
    ctx.data_mut(|data| data.insert_temp(support_environment_id(), environment));
}

pub(super) fn copy_error(ui: &mut egui::Ui, error: &str, recipe: Option<&Project>) {
    let ctx = ui.ctx().clone();
    if error.is_empty()
        || !ctx.data(|data| {
            data.get_temp::<SupportEnvironment>(support_environment_id())
                .is_some()
        })
    {
        return;
    }

    if ui
        .button("Copy error message")
        .on_hover_text("Copies the full error and a support code. Review both before sharing.")
        .clicked()
        && let Some(environment) =
            ctx.data(|data| data.get_temp::<SupportEnvironment>(support_environment_id()))
    {
        let message = match environment.snapshot(recipe).encode() {
            Ok(code) => format!("{error}\n\nMapper support code\n{code}"),
            Err(_) => error.to_owned(),
        };
        ctx.copy_text(message);
    }
}

pub(super) fn support_controls(ui: &mut egui::Ui, preview: &str, code: &str) {
    ui.label("Review the details before sharing your support code.");
    ui.small("Anyone with the code can read these details.");
    egui::ScrollArea::vertical()
        .id_salt("support-preview")
        .max_height(230.)
        .show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(preview).monospace()).selectable(true));
        });
    if ui.button("Copy support code").clicked() {
        ui.ctx().copy_text(code.to_owned());
    }
    ui.collapsing("Show support code", |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(code).monospace())
                .wrap()
                .selectable(true),
        );
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecipeSection {
    Roads,
    Rails,
}

enum RoutingIssue {
    RoadConnections,
    RoadApproach,
    RailGrading,
    RailGradeLimit,
    RailEntrances,
    RailSearch,
}

pub(super) fn recipe_section(text: &str) -> Option<RecipeSection> {
    match routing_issue(&excerpt(text, 240))? {
        RoutingIssue::RoadConnections | RoutingIssue::RoadApproach => Some(RecipeSection::Roads),
        RoutingIssue::RailGrading
        | RoutingIssue::RailGradeLimit
        | RoutingIssue::RailEntrances
        | RoutingIssue::RailSearch => Some(RecipeSection::Rails),
    }
}

fn routing_issue(prefix: &str) -> Option<RoutingIssue> {
    if prefix.contains("Bounded complete road layouts exhausted:")
        || prefix.contains("Ordered paved roads left ")
    {
        Some(RoutingIssue::RoadConnections)
    } else if prefix.contains("Bounded local alignment retries exhausted:") {
        Some(RoutingIssue::RoadApproach)
    } else if prefix.contains("Rail entrance recovery reached the shared generation allowance")
        || prefix.contains("Required rail ring was not resolved")
        || prefix.contains("Rail approach search stopped")
        || prefix.contains("Connected rail height constraints failed: WorkLimit")
    {
        Some(RoutingIssue::RailSearch)
    } else if prefix.contains("Rail recovery could not preserve")
        || prefix.contains("Rail entrances could not be connected")
        || prefix.contains("Rail entrance recovery repeated a rejected site")
        || prefix.contains("Connected rail fixed heights conflict")
    {
        Some(RoutingIssue::RailEntrances)
    } else if prefix.contains("Connected rail fit could not satisfy the requested continuous grade")
        || prefix.contains("Connected rail height constraints failed: Infeasible")
    {
        Some(RoutingIssue::RailGradeLimit)
    } else if prefix.contains("Rail bed exceeds cumulative cut/fill allowance") {
        Some(RoutingIssue::RailGrading)
    } else {
        None
    }
}

pub(super) fn excerpt(text: &str, characters: usize) -> Cow<'_, str> {
    match text.char_indices().nth(characters) {
        Some((end, _)) => Cow::Owned(format!("{}…", &text[..end])),
        None => Cow::Borrowed(text),
    }
}

pub(super) fn summary(text: &str) -> Cow<'_, str> {
    let prefix = text
        .char_indices()
        .nth(240)
        .map_or(text, |(end, _)| &text[..end]);
    if let Some(issue) = routing_issue(prefix) {
        return Cow::Borrowed(match issue {
            RoutingIssue::RoadConnections => {
                "Some monument road connections could not be built. Check Advanced routing or try moving a monument."
            }
            RoutingIssue::RoadApproach => {
                "A road approach could not be fitted. Check Advanced routing or try moving the monument."
            }
            RoutingIssue::RailGrading => {
                "Surface rails need more terrain grading. Check Maximum cut/fill or try another terrain seed."
            }
            RoutingIssue::RailGradeLimit => {
                "Surface rails couldn’t fit within the bed grade limit. Check Bed grade or try another terrain seed."
            }
            RoutingIssue::RailEntrances => {
                "Surface rails couldn’t connect all required entrances. Try another terrain seed or review Rail settings."
            }
            RoutingIssue::RailSearch => {
                "No usable surface rail layout was found. Try another terrain seed or review Rail settings."
            }
        });
    }
    if prefix.contains("Invalid coarse map surface") {
        return Cow::Borrowed(
            "The terrain preview could not be built. Open the technical report for the rejected input or limit.",
        );
    }
    if let Some(end) = prefix.find(['\n', '\r', '{']) {
        if prefix[..end].trim().is_empty() {
            return Cow::Borrowed("Generation details are available.");
        }
        return Cow::Borrowed(prefix[..end].trim_end());
    }
    if prefix.len() == text.len() {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(format!("{prefix}…"))
    }
}

pub(super) fn queue_summary(text: &str) -> Cow<'_, str> {
    let prefix = excerpt(text, 240);
    Cow::Borrowed(match routing_issue(&prefix) {
        Some(RoutingIssue::RoadConnections) => "Roads could not connect",
        Some(RoutingIssue::RoadApproach) => "Road approach did not fit",
        Some(RoutingIssue::RailGrading) => "Rails need more grading",
        Some(RoutingIssue::RailGradeLimit) => "Rail bed too steep",
        Some(RoutingIssue::RailEntrances) => "Rail entrances disconnected",
        Some(RoutingIssue::RailSearch) => "No usable rail layout",
        None => return summary(text),
    })
}

pub(super) fn details(ui: &mut egui::Ui, text: &str) {
    copy_details(ui, text);
    report_preview(ui, text);
}

pub(super) fn copy_details(ui: &mut egui::Ui, text: &str) {
    if ui.button("Copy full details").clicked() {
        ui.ctx().copy_text(text.to_owned());
    }
}

pub(super) fn report_preview(ui: &mut egui::Ui, text: &str) {
    let preview = excerpt(text, 4096);
    if preview.as_ref() != text {
        ui.weak("Preview shortened.");
    }
    egui::ScrollArea::vertical()
        .max_height(180.)
        .show(ui, |ui| {
            ui.add(egui::Label::new(preview.as_ref()).wrap());
        });
}

pub(super) fn message(ui: &mut egui::Ui, text: &str, id: &str) {
    let short = summary(text);
    ui.label(short.as_ref());
    if short.as_ref() != text {
        egui::CollapsingHeader::new("Details")
            .id_salt(id)
            .show(ui, |ui| details(ui, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }
    #[test]
    fn recovered_rail_diagnostics_point_to_the_matching_controls() {
        for (report, expected) in [
            (
                "Composition unavailable: Connected rail fit could not satisfy the requested continuous grade within its limits: {\"maximum_grade\":0.15}",
                "Surface rails couldn’t fit within the bed grade limit. Check Bed grade or try another terrain seed.",
            ),
            (
                "Composition unavailable: Connected rail fixed heights conflict with terrain/earthwork bounds at track 2, control 0\nRail recovery details: {\"attempts\":[]}",
                "Surface rails couldn’t connect all required entrances. Try another terrain seed or review Rail settings.",
            ),
            (
                "Composition unavailable: Rail entrance recovery reached the shared generation allowance. Connected rail fixed heights conflict with terrain/earthwork bounds",
                "No usable surface rail layout was found. Try another terrain seed or review Rail settings.",
            ),
        ] {
            assert_eq!(summary(report), expected);
            assert_eq!(recipe_section(report), Some(RecipeSection::Rails));
        }
    }
    #[test]
    fn summaries_keep_short_messages_and_bound_unicode_and_nested_reports() {
        assert_eq!(
            summary("Road generation rejected a steep approach"),
            "Road generation rejected a steep approach"
        );
        let text = format!(
            "Composition unavailable: Bounded complete road layouts exhausted: {{\"report\":\"{}\"}}",
            "雪".repeat(100_000)
        );
        assert!(!summary(&text).contains("report"));
        assert!(summary(&text).len() < 160);
        assert!(
            summary("Composition unavailable: Rail bed exceeds cumulative cut/fill allowance")
                .starts_with("Surface rails need more terrain grading")
        );
        assert!(
            summary(
                "Generation stopped: Preview failed: Invalid coarse map surface inputs or limits"
            )
            .starts_with("The terrain preview could not be built.")
        );
        let unicode = "雪".repeat(100_000);
        assert_eq!(summary(&unicode).chars().count(), 241);
        assert_eq!(excerpt(&unicode, 4096).chars().count(), 4097);
        assert_eq!(summary("Failure: {\"technical\":true}"), "Failure:");
        assert_eq!(
            summary("{\"technical\":true}"),
            "Generation details are available."
        );
    }
    #[test]
    fn large_failure_details_render_with_bounded_text_in_both_themes() {
        for dark in [true, false] {
            for width in [320, 640] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                let text = format!(
                    "Composition unavailable: Bounded complete road layouts exhausted: {{\"report\":\"{}\"}}END_OF_FULL_REPORT",
                    "road: 3, triangle: 99, minimum: 0.0240318, limit: 0.0200000; ".repeat(10_000)
                );
                let mut textures = snapshot::Textures::default();
                let mut final_output = None;
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width as f32, 360.),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            egui::CentralPanel::default().show(ui, |ui| {
                                ui.label(summary(&text).as_ref());
                                details(ui, &text);
                            });
                        },
                    );
                    textures.update(&output.textures_delta);
                    output.textures_delta.clear();
                    final_output = Some(output);
                }
                let output = final_output.unwrap();
                let visible = output
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(visible.contains("Some monument road connections could not be built"));
                assert!(visible.contains("Copy full details"));
                assert!(!visible.contains("END_OF_FULL_REPORT"));
                assert!(visible.chars().count() < 5000);
                assert!(text.ends_with("END_OF_FULL_REPORT"));
                if let Some(directory) = std::env::var_os("MAPPER_FAILURE_UI_SNAPSHOTS") {
                    let image = textures.raster(&ctx, output, [width, 360]);
                    mapper_studio::image_export::write_png(
                        &std::path::PathBuf::from(directory).join(format!(
                            "failure-{}-{width}.png",
                            if dark { "dark" } else { "light" }
                        )),
                        &image,
                        || false,
                    )
                    .unwrap();
                }
            }
        }
    }
}
