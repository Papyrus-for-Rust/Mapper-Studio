use eframe::egui;
use papyrus_terrain::terrain_graph::{
    CellularMode, DistanceMode, MaskMode, NoiseMode, Operation, Port,
};

pub(super) const CATALOG: &[(&str, Operation)] = &[
    ("Native height", Operation::NativeHeight),
    ("Constant height", Operation::Height { metres: 25. }),
    (
        "Heightmap",
        Operation::Heightmap {
            position: None,
            asset: 0,
            center: [0.; 2],
            size: [1000.; 2],
            low: -30.,
            high: 250.,
        },
    ),
    ("Constant mask", Operation::Mask { coverage: 0.5 }),
    (
        "Ellipse",
        Operation::Ellipse {
            position: None,
            center: [0.; 2],
            radii: [650., 430.],
            rotation: 0.,
            feather: 0.25,
        },
    ),
    (
        "Gradient",
        Operation::Gradient {
            position: None,
            start: [-500., 0.],
            end: [500., 0.],
        },
    ),
    (
        "Noise",
        Operation::Noise {
            position: None,
            wavelength: 500.,
            octaves: 3,
            persistence: 0.5,
            mode: NoiseMode::Fractal,
            seed: None,
        },
    ),
    (
        "Cellular",
        Operation::Cellular {
            position: None,
            wavelength: 350.,
            jitter: 0.8,
            mode: CellularMode::Borders,
            seed: None,
        },
    ),
    (
        "Rings",
        Operation::Rings {
            position: None,
            center: [0.; 2],
            spacing: 200.,
            width: 60.,
            feather: 25.,
        },
    ),
    ("World position", Operation::WorldPosition),
    (
        "Transform position",
        Operation::TransformPosition {
            position: None,
            offset: [0.; 2],
            scale: [1.; 2],
            rotation: 0.,
        },
    ),
    (
        "Warp position",
        Operation::WarpPosition {
            position: None,
            x_mask: 0,
            z_mask: 0,
            amount: [100.; 2],
        },
    ),
    (
        "Mirror position",
        Operation::MirrorPosition {
            position: None,
            center: [0.; 2],
            axes: [true, false],
        },
    ),
    (
        "Circle distance",
        Operation::CircleDistance {
            position: None,
            center: [0.; 2],
            radius: 500.,
        },
    ),
    (
        "Box distance",
        Operation::BoxDistance {
            position: None,
            center: [0.; 2],
            half_size: [600., 400.],
            rotation: 0.,
        },
    ),
    (
        "Capsule distance",
        Operation::CapsuleDistance {
            position: None,
            start: [-500., 0.],
            end: [500., 0.],
            radius: 180.,
        },
    ),
    (
        "Combine distances",
        Operation::CombineDistance {
            left: 0,
            right: 0,
            mode: DistanceMode::Union,
            smoothing: 60.,
        },
    ),
    (
        "Offset distance",
        Operation::OffsetDistance {
            distance: 0,
            metres: 100.,
        },
    ),
    (
        "Distance mask",
        Operation::DistanceMask {
            distance: 0,
            feather: 80.,
        },
    ),
    (
        "Combine masks",
        Operation::Combine {
            left: 0,
            right: 0,
            mode: MaskMode::Multiply,
        },
    ),
    ("Invert mask", Operation::Invert { input: 0 }),
    (
        "Remap mask",
        Operation::Remap {
            input: 0,
            low: 0.15,
            high: 0.85,
        },
    ),
    (
        "Elevation mask",
        Operation::ElevationMask {
            height: 0,
            low: 20.,
            high: 90.,
        },
    ),
    (
        "Blend heights",
        Operation::Blend {
            low: 0,
            high: 0,
            mask: 0,
        },
    ),
    (
        "Raise terrain",
        Operation::Raise {
            height: 0,
            mask: 0,
            metres: 20.,
        },
    ),
    (
        "Terrace",
        Operation::Terrace {
            height: 0,
            step: 16.,
            softness: 0.2,
        },
    ),
];

pub(super) fn name(operation: Operation) -> &'static str {
    match operation {
        Operation::Group { .. } => return "Linked group",
        Operation::GroupInput { .. } => return "Group input",
        _ => {}
    }
    let kind = std::mem::discriminant(&operation);
    CATALOG
        .iter()
        .find(|(_, candidate)| std::mem::discriminant(candidate) == kind)
        .map(|(label, _)| *label)
        .expect("every operation is in the catalog")
}

pub(super) fn port_name(port: Port) -> &'static str {
    match port {
        Port::Position => "Position",
        Port::Height => "Height",
        Port::Mask => "Mask",
        Port::Distance => "Distance",
    }
}

pub(super) fn parameters(ui: &mut egui::Ui, operation: &mut Operation) {
    use Operation::*;
    egui::Grid::new("node-parameters")
        .num_columns(2)
        .show(ui, |ui| match operation {
            Height { metres } => measure(ui, "Height", metres, " m", 1.),
            Heightmap {
                center,
                size,
                low,
                high,
                ..
            } => {
                pair(ui, "Center", center, " m", 1.);
                pair(ui, "Size", size, " m", 1.);
                measure(ui, "Black height", low, " m", 1.);
                measure(ui, "White height", high, " m", 1.);
            }
            Mask { coverage } => unit(ui, "Coverage", coverage),
            TransformPosition {
                offset,
                scale,
                rotation,
                ..
            } => {
                pair(ui, "Offset", offset, " m", 1.);
                pair(ui, "Scale", scale, "", 0.05);
                measure(ui, "Rotation", rotation, "°", 1.);
            }
            WarpPosition { amount, .. } => pair(ui, "Amount", amount, " m", 1.),
            MirrorPosition { center, axes, .. } => {
                pair(ui, "Center", center, " m", 1.);
                ui.label("Axes");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut axes[0], "X");
                    ui.checkbox(&mut axes[1], "Z");
                });
                ui.end_row();
            }
            Ellipse {
                center,
                radii,
                rotation,
                feather,
                ..
            } => {
                pair(ui, "Center", center, " m", 1.);
                pair(ui, "Radii", radii, " m", 1.);
                measure(ui, "Rotation", rotation, "°", 1.);
                unit(ui, "Feather", feather);
            }
            Gradient { start, end, .. } => {
                pair(ui, "Start", start, " m", 1.);
                pair(ui, "End", end, " m", 1.);
            }
            Noise {
                wavelength,
                octaves,
                persistence,
                mode,
                seed,
                ..
            } => {
                choice(
                    ui,
                    "Mode",
                    mode,
                    &[
                        (NoiseMode::Fractal, "Fractal"),
                        (NoiseMode::Ridged, "Ridged"),
                        (NoiseMode::Billow, "Billow"),
                    ],
                );
                measure(ui, "Wavelength", wavelength, " m", 1.);
                let label = ui.label("Octaves");
                ui.add(egui::DragValue::new(octaves).range(1..=8))
                    .labelled_by(label.id);
                ui.end_row();
                unit(ui, "Persistence", persistence);
                seed_field(ui, seed);
            }
            Cellular {
                wavelength,
                jitter,
                mode,
                seed,
                ..
            } => {
                choice(
                    ui,
                    "Mode",
                    mode,
                    &[
                        (CellularMode::Centers, "Centers"),
                        (CellularMode::Borders, "Borders"),
                        (CellularMode::Value, "Value"),
                    ],
                );
                measure(ui, "Wavelength", wavelength, " m", 1.);
                unit(ui, "Jitter", jitter);
                seed_field(ui, seed);
            }
            Rings {
                center,
                spacing,
                width,
                feather,
                ..
            } => {
                pair(ui, "Center", center, " m", 1.);
                measure(ui, "Spacing", spacing, " m", 1.);
                measure(ui, "Width", width, " m", 1.);
                measure(ui, "Feather", feather, " m", 1.);
            }
            CircleDistance { center, radius, .. } => {
                pair(ui, "Center", center, " m", 1.);
                measure(ui, "Radius", radius, " m", 1.);
            }
            BoxDistance {
                center,
                half_size,
                rotation,
                ..
            } => {
                pair(ui, "Center", center, " m", 1.);
                pair(ui, "Half size", half_size, " m", 1.);
                measure(ui, "Rotation", rotation, "°", 1.);
            }
            CapsuleDistance {
                start, end, radius, ..
            } => {
                pair(ui, "Start", start, " m", 1.);
                pair(ui, "End", end, " m", 1.);
                measure(ui, "Radius", radius, " m", 1.);
            }
            CombineDistance {
                mode, smoothing, ..
            } => {
                choice(
                    ui,
                    "Mode",
                    mode,
                    &[
                        (DistanceMode::Union, "Union"),
                        (DistanceMode::Intersect, "Intersect"),
                        (DistanceMode::Subtract, "Subtract"),
                    ],
                );
                measure(ui, "Smoothing", smoothing, " m", 1.);
            }
            OffsetDistance { metres, .. } => measure(ui, "Offset", metres, " m", 1.),
            DistanceMask { feather, .. } => measure(ui, "Feather width", feather, " m", 1.),
            Combine { mode, .. } => choice(
                ui,
                "Mode",
                mode,
                &[
                    (MaskMode::Union, "Union"),
                    (MaskMode::Intersect, "Intersect"),
                    (MaskMode::Subtract, "Subtract"),
                    (MaskMode::Multiply, "Multiply"),
                ],
            ),
            Remap { low, high, .. } => {
                unit(ui, "Low", low);
                unit(ui, "High", high);
            }
            ElevationMask { low, high, .. } => {
                measure(ui, "Low", low, " m", 1.);
                measure(ui, "High", high, " m", 1.);
            }
            Raise { metres, .. } => measure(ui, "Amount", metres, " m", 1.),
            Terrace { step, softness, .. } => {
                measure(ui, "Step", step, " m", 1.);
                unit(ui, "Softness", softness);
            }
            NativeHeight
            | WorldPosition
            | Invert { .. }
            | Blend { .. }
            | Group { .. }
            | GroupInput { .. } => {}
        });
}

fn measure(ui: &mut egui::Ui, name: &str, value: &mut f32, suffix: &str, speed: f64) {
    let label = ui.label(name);
    ui.add(egui::DragValue::new(value).suffix(suffix).speed(speed))
        .labelled_by(label.id);
    ui.end_row();
}

fn unit(ui: &mut egui::Ui, name: &str, value: &mut f32) {
    let label = ui.label(name);
    ui.add(egui::DragValue::new(value).range(0. ..=1.).speed(0.01))
        .labelled_by(label.id);
    ui.end_row();
}

fn pair(ui: &mut egui::Ui, name: &str, values: &mut [f32; 2], suffix: &str, speed: f64) {
    ui.label(name);
    ui.horizontal(|ui| {
        for (value, axis) in values.iter_mut().zip(["X", "Z"]) {
            let label = ui.weak(axis);
            ui.add(egui::DragValue::new(value).suffix(suffix).speed(speed))
                .labelled_by(label.id);
        }
    });
    ui.end_row();
}

fn choice<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    choices: &[(T, &str)],
) {
    let label = ui.label(label);
    let selected = choices
        .iter()
        .find(|(variant, _)| *variant == *value)
        .map_or("", |(_, name)| *name);
    egui::ComboBox::from_id_salt(label.id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for &(variant, name) in choices {
                ui.selectable_value(value, variant, name);
            }
        })
        .response
        .labelled_by(label.id);
    ui.end_row();
}

fn seed_field(ui: &mut egui::Ui, value: &mut Option<u32>) {
    let label = ui.label("Seed");
    let mut fixed = value.is_some();
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut fixed, "Fixed")
            .labelled_by(label.id)
            .changed()
        {
            *value = fixed.then_some(1);
        }
        if let Some(seed) = value {
            ui.add(egui::DragValue::new(seed)).labelled_by(label.id);
        }
    });
    ui.end_row();
}
