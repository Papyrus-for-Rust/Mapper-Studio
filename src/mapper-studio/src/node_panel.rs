//! A document-local graph draft. Only validated edits enter the generation recipe.
mod controls;
mod heightmap;

use eframe::egui::{self, AtomExt as _, Color32, Pos2, Rect, Sense, Stroke, Vec2, vec2};
use mapper_studio::Project;
use papyrus_terrain::terrain_graph::{
    Graph, MAX_HEIGHTMAP_PIXELS, MAX_NODES, Node, NodeGroup, Operation, Port, VERSION,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const CARD: Vec2 = vec2(154., 115.);
const CARD_RADIUS: u8 = 4;
const STEP: Vec2 = vec2(222., 142.);
const MIN_LABEL_ZOOM: f32 = 0.25;
const INSPECTOR_WIDTH: f32 = 248.;
const INSPECTOR_SPLITTER: f32 = 7.;

const EXAMPLES: &[(&str, &str)] = &[
    (
        "Ridged highlands",
        include_str!("../../../docs/examples/terrain-graphs/ridged-highlands.json"),
    ),
    (
        "Archipelago",
        include_str!("../../../docs/examples/terrain-graphs/archipelago.json"),
    ),
    (
        "Terraced caldera",
        include_str!("../../../docs/examples/terrain-graphs/terraced-caldera.json"),
    ),
    (
        "Warped faults",
        include_str!("../../../docs/examples/terrain-graphs/warped-faults.json"),
    ),
    (
        "Tidal lagoon",
        include_str!("../../../docs/examples/terrain-graphs/tidal-lagoon.json"),
    ),
];

#[derive(Default)]
pub(super) struct Panel {
    applied: Option<Arc<Graph>>,
    draft: Option<Graph>,
    removal_pending: bool,
    selected: Option<u32>,
    marked: BTreeSet<u32>,
    offsets: BTreeMap<u32, Vec2>,
    search: String,
    node_filter: String,
    inspector_width: Option<f32>,
    issue: Option<String>,
    validation: Option<DraftValidation>,
    layout: Option<GraphLayout>,
    reset_scroll: bool,
    zoom: Option<f32>,
    zoom_offset: Option<Vec2>,
    fitted_extent: Option<Vec2>,
    fit: bool,
    locate: Option<u32>,
    reveal_definition: Option<u32>,
}

struct DraftValidation {
    graph: Graph,
    size: u32,
    issue: Option<String>,
    node_issue: Option<NodeIssue>,
}

struct NodeIssue {
    id: u32,
    input: Option<usize>,
    summary: String,
}

impl NodeIssue {
    fn find(graph: &Graph) -> Option<Self> {
        let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (node.id, node)).collect();
        for node in graph.nodes.iter().find(|node| node.id == graph.output).into_iter()
            .chain(graph.nodes.iter().filter(|node| node.id != graph.output))
        {
            for (slot, input) in node.op.inputs().into_iter().enumerate() {
                let Some(input) = input else { continue };
                let problem = match input.source {
                    None if !input.optional => "Connect",
                    Some(source) if nodes.get(&source).is_none_or(|node| node.op.port() != input.port) => "Replace",
                    _ => continue,
                };
                let name = input_label(graph, node.op, slot, input.name);
                return Some(Self {
                    id: node.id,
                    input: Some(slot),
                    summary: format!("{problem} {name} on {}", operation_name(graph, node.op)),
                });
            }
            if let Operation::Heightmap { asset, .. } = node.op
                && !graph.heightmaps.contains_key(&asset)
            {
                return Some(Self {
                    id: node.id,
                    input: None,
                    summary: "Choose an image for the heightmap".into(),
                });
            }
        }
        None
    }
}

struct GraphLayout {
    // Only the wiring and frame membership affect automatic placement.
    nodes: Vec<(u32, [Option<u32>; 3])>,
    groups: Vec<Vec<u32>>,
    positions: BTreeMap<u32, Vec2>,
}

impl GraphLayout {
    fn nodes(graph: &Graph) -> impl Iterator<Item = (u32, [Option<u32>; 3])> + '_ {
        graph.nodes.iter().map(|node| {
            (
                node.id,
                node.op
                    .inputs()
                    .map(|port| port.and_then(|input| input.source)),
            )
        })
    }

    fn new(graph: &Graph) -> Self {
        Self {
            nodes: Self::nodes(graph).collect(),
            groups: graph
                .groups
                .iter()
                .map(|group| group.nodes.clone())
                .collect(),
            positions: arrange(graph),
        }
    }

    fn matches(&self, graph: &Graph) -> bool {
        Self::nodes(graph).eq(self.nodes.iter().copied())
            && graph
                .groups
                .iter()
                .map(|group| &group.nodes)
                .eq(&self.groups)
    }
}

#[derive(Clone, Copy)]
struct OutputDrag {
    node: u32,
    port: Port,
}

impl Panel {
    pub(super) fn show(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        let same_graph = match (&self.applied, &project.terrain_graph) {
            (Some(applied), Some(current)) => Arc::ptr_eq(applied, current),
            (None, None) => true,
            _ => false,
        };
        if !same_graph {
            let changed = self.applied != project.terrain_graph;
            self.applied = project.terrain_graph.clone();
            if changed {
                self.replace(self.applied.as_deref().cloned());
            }
        }
        if self.removal_pending {
            let apply = ui.horizontal_wrapped(|ui| {
                let apply = ui.button("Apply removal")
                    .on_hover_text("Use the Recipe's terrain settings for future generations.")
                    .clicked();
                if ui.button("Keep editing").clicked() {
                    self.removal_pending = false;
                    ui.ctx().request_repaint();
                }
                ui.weak("Removal pending")
                    .on_hover_text("The Recipe still uses the applied graph.");
                apply
            }).inner;
            if apply {
                self.applied = None;
                project.terrain_graph = None;
                self.replace(None);
                ui.ctx().request_repaint();
            }
            return;
        }
        let mut issue = None;
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(self.draft.is_some(), |ui| {
                ui.menu_button("Add node", |ui| self.picker(ui))
                    .response.on_hover_text("Search for a node. Press Enter to add the only matching result.");
            });
            ui.menu_button("Examples", |ui| {
                for &(title, json) in EXAMPLES {
                    if ui.button(title).clicked() {
                        ui.close();
                        self.load(json.as_bytes(), project.size);
                    }
                }
            });
            ui.menu_button("Graph", |ui| {
                if let Some(graph) = &mut self.draft {
                    let group = graph.groups.iter().position(|group| {
                        self.selected
                            .is_some_and(|node| group.nodes.contains(&node))
                    });
                    let can_group = self.marked.len() > 1
                        && !graph
                            .groups
                            .iter()
                            .any(|group| group.nodes.iter().any(|node| self.marked.contains(node)));
                    if ui
                        .add_enabled(can_group, egui::Button::new("Group selected nodes"))
                        .on_hover_text("Shift-click node headers to select multiple nodes.")
                        .clicked()
                    {
                        graph.groups.push(NodeGroup {
                            name: "Node group".into(),
                            nodes: std::mem::take(&mut self.marked).into_iter().collect(),
                        });
                        ui.close();
                    }
                    if let Some(group) = group {
                        if ui
                            .button("Make linked group")
                            .on_hover_text("Turn this frame into a reusable node with one output.")
                            .clicked()
                        {
                            match graph.link_group(group, self.selected.unwrap_or_default()) {
                                Ok(id) => {
                                    self.selected = Some(id);
                                    self.marked.clear();
                                    self.offsets.retain(|id, _| {
                                        graph.nodes.iter().any(|node| node.id == *id)
                                    });
                                    self.issue = None;
                                }
                                Err(error) => self.issue = Some(error),
                            }
                            ui.close();
                            return;
                        }
                        if ui.button("Duplicate group").clicked() {
                            match graph.duplicate_group(group) {
                                Ok(index) => {
                                    self.selected = graph.groups[index].nodes.first().copied();
                                    self.marked.clear();
                                    self.issue = None;
                                }
                                Err(error) => self.issue = Some(error),
                            }
                            ui.close();
                        }
                        if ui.button("Dissolve group").clicked() {
                            graph.groups.remove(group);
                            ui.close();
                        }
                    }
                    ui.separator();
                }
                if ui
                    .add_enabled(
                        self.draft
                            .as_ref()
                            .is_none_or(|graph| graph.nodes.len() < MAX_NODES),
                        egui::Button::new("Import heightmap…"),
                    )
                    .clicked()
                {
                    ui.close();
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Grayscale heightmap", &["png"])
                        .pick_file()
                    {
                        match heightmap::read(&path)
                            .and_then(|image| self.add_heightmap(image, project.size))
                        {
                            Ok(()) => self.issue = None,
                            Err(error) => self.issue = Some(error),
                        }
                    }
                }
                if ui.button("Import JSON…").clicked() {
                    ui.close();
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Terrain graph", &["json"])
                        .pick_file()
                    {
                        match std::fs::File::open(path) {
                            Ok(file) => self.load(file, project.size),
                            Err(error) => self.issue = Some(error.to_string()),
                        }
                    }
                }
                if ui
                    .add_enabled(self.draft.is_some(), egui::Button::new("Auto layout"))
                    .clicked()
                {
                    ui.close();
                    self.offsets.clear();
                    self.reset_scroll = true;
                }
                if ui
                    .add_enabled(
                        self.draft.as_ref() != self.applied.as_deref(),
                        egui::Button::new("Revert draft"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.replace(self.applied.as_deref().cloned());
                }
                if ui
                    .add_enabled(
                        self.draft.is_some(),
                        egui::Button::new(if self.applied.is_some() { "Remove graph" } else { "Discard draft" }),
                    )
                    .on_hover_text(if self.applied.is_some() {
                        "Stage removal of the graph from the Recipe."
                    } else {
                        "Clear the unapplied graph."
                    })
                    .clicked()
                {
                    ui.close();
                    if self.applied.is_some() {
                        self.removal_pending = true;
                        ui.ctx().request_repaint();
                    } else {
                        self.replace(None);
                    }
                }
            });
            if self.removal_pending {
                return;
            }
            if self.draft.is_some() && ui.button("Fit")
                .on_hover_text("Fit all nodes in the canvas.").clicked()
            {
                self.fit = true;
            }
            let magnification = self.zoom.unwrap_or(1.) * 100.;
            let zoom_label = if magnification < 10. {
                format!("{magnification:.1}%")
            } else {
                format!("{magnification:.0}%")
            };
            if self.draft.is_some()
                && ui
                    .button(zoom_label)
                    .on_hover_text("Reset zoom. Use Ctrl + scroll over the graph to zoom.")
                    .clicked()
            {
                self.zoom = None;
                self.fitted_extent = None;
                self.reset_scroll = true;
            }
            let mut changed = self.draft.as_ref() != self.applied.as_deref();
            issue = self.draft_issue(project);
            let can_apply = changed && issue.is_none();
            let apply = if can_apply {
                egui::Button::new(egui::RichText::new("Apply to recipe").color(Color32::WHITE))
                    .fill(super::workbench_style::PRIMARY_ACTION)
            } else {
                egui::Button::new("Apply to recipe")
            };
            if ui
                .add_enabled(can_apply, apply)
                .on_hover_text("Use this draft the next time you generate from the Recipe.")
                .on_disabled_hover_text(if issue.is_some() {
                    "Resolve the graph issue before applying."
                } else {
                    "The recipe already uses this graph."
                })
                .clicked()
            {
                self.applied = self.draft.clone().map(Arc::new);
                project.terrain_graph = self.applied.clone();
                self.issue = None;
                changed = false;
            }
            if changed {
                ui.weak("Unapplied changes")
                    .on_hover_text("Generation still uses the current Recipe.");
            } else if self.applied.is_some() {
                ui.weak("In Recipe");
            }
        });
        if self.removal_pending {
            return;
        }
        let node_issue = issue.as_ref().filter(|_| project.supports_terrain_graph())
            .and_then(|_| self.validation.as_ref()?.node_issue.as_ref());
        let attention = node_issue.map(|issue| (issue.id, issue.input));
        let mut reveal_issue = false;
        if let Some(issue) = node_issue {
            let id = issue.id;
            let go = ui.horizontal(|ui| {
                let go = ui.small_button("Go to node").clicked();
                warning(ui, &issue.summary);
                go
            }).inner;
            if go {
                self.selected = Some(id);
                self.marked.clear();
                self.locate = Some(id);
                reveal_issue = true;
                ui.ctx().request_repaint();
            }
        } else if let Some(error) = &issue {
            warning(ui, error);
        }
        if let Some(error) = &self.issue {
            let dismiss = ui.horizontal(|ui| {
                let dismiss = ui.small_button("Dismiss")
                    .on_hover_text("Dismiss the last graph operation message.")
                    .clicked();
                warning(ui, error);
                dismiss
            }).inner;
            if dismiss {
                self.issue = None;
            }
        }
        if self.draft.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                if ui.button("Start from recipe").clicked() {
                    self.replace(Some(Graph {
                        version: VERSION,
                        output: 1,
                        heightmaps: Default::default(),
                        groups: Vec::new(),
                        definitions: Default::default(),
                        nodes: vec![Node {
                            id: 1,
                            op: Operation::NativeHeight,
                        }],
                    }));
                }
            });
            return;
        }
        let available = ui.available_size_before_wrap();
        if available.x >= 550. {
            let spacing = 2. * ui.spacing().item_spacing.x + INSPECTOR_SPLITTER;
            let max_inspector = (available.x - spacing - 260.).max(INSPECTOR_WIDTH);
            let inspector_width = self.inspector_width.unwrap_or(INSPECTOR_WIDTH)
                .clamp(INSPECTOR_WIDTH, max_inspector);
            ui.horizontal_top(|ui| {
                self.canvas(
                    ui,
                    vec2(available.x - inspector_width - spacing, available.y),
                    attention,
                );
                let (divider, handle) = ui.allocate_exact_size(
                    vec2(INSPECTOR_SPLITTER, available.y), Sense::click_and_drag(),
                );
                let handle = handle.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                    .on_hover_text("Drag or use the arrow keys to resize. Double-click to reset.");
                if handle.clicked() {
                    handle.request_focus();
                }
                handle.widget_info(|| egui::WidgetInfo::labeled(
                    egui::WidgetType::Other, ui.is_enabled(), "Resize node inspector",
                ));
                let active = handle.hovered() || handle.dragged() || handle.has_focus();
                let color = if active { ui.visuals().hyperlink_color } else { ui.visuals().window_stroke.color };
                ui.painter().vline(divider.center().x, divider.y_range(), Stroke::new(1., color));
                if handle.double_clicked() || handle.triple_clicked() {
                    self.inspector_width = None;
                    ui.ctx().request_repaint();
                } else {
                    let step = if handle.has_focus() {
                        ui.input_mut(|input| {
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                                20.
                            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                                -20.
                            } else {
                                0.
                            }
                        })
                    } else {
                        0.
                    };
                    let delta = handle.drag_delta().x + step;
                    if delta != 0. {
                        self.inspector_width = Some((inspector_width - delta).clamp(INSPECTOR_WIDTH, max_inspector));
                        ui.ctx().request_repaint();
                    }
                }
                ui.allocate_ui_with_layout(
                    vec2(inspector_width, available.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.inspector(ui, available.y, attention, reveal_issue);
                    },
                );
            });
        } else {
            let canvas_height = (available.y * 0.57).max(80.).min(available.y);
            self.canvas(ui, vec2(available.x, canvas_height), attention);
            ui.separator();
            self.inspector(ui, ui.available_height(), attention, reveal_issue);
        }
    }

    fn replace(&mut self, graph: Option<Graph>) {
        self.selected = graph.as_ref().map(|graph| graph.output);
        self.draft = graph;
        self.removal_pending = false;
        self.marked.clear();
        self.node_filter.clear();
        self.offsets.clear();
        self.issue = None;
        self.validation = None;
        self.layout = None;
        self.reset_scroll = true;
        self.zoom = None;
        self.zoom_offset = None;
        self.fitted_extent = None;
        self.fit = false;
        self.locate = None;
        self.reveal_definition = None;
    }

    fn draft_issue(&mut self, project: &Project) -> Option<String> {
        let graph = self.draft.as_ref()?;
        if !project.supports_terrain_graph() {
            return Some("Choose a supported Rust installation to apply this graph.".into());
        }
        if self
            .validation
            .as_ref()
            .is_none_or(|validation| validation.size != project.size || validation.graph != *graph)
        {
            let issue = graph.validate(project.size).err();
            self.validation = Some(DraftValidation {
                node_issue: issue.as_ref().and_then(|_| NodeIssue::find(graph)),
                issue,
                graph: graph.clone(),
                size: project.size,
            });
        }
        self.validation.as_ref()?.issue.clone()
    }

    fn load(&mut self, reader: impl std::io::Read, size: u32) {
        match Graph::read(reader).and_then(|graph| graph.validate(size).map(|()| graph)) {
            Ok(graph) => self.replace(Some(graph)),
            Err(error) => self.issue = Some(error),
        }
    }

    fn add_heightmap(
        &mut self,
        image: papyrus_terrain::terrain_graph::Heightmap,
        size: u32,
    ) -> Result<(), String> {
        let graph = self.draft.get_or_insert_with(|| Graph {
            version: VERSION,
            output: 0,
            nodes: Vec::new(),
            heightmaps: Default::default(),
            groups: Vec::new(),
            definitions: Default::default(),
        });
        if graph.nodes.len() == MAX_NODES {
            return Err("Node limit reached".into());
        }
        let pixels = graph
            .heightmaps
            .values()
            .map(|map| map.samples.len())
            .sum::<usize>();
        if pixels + image.samples.len() > MAX_HEIGHTMAP_PIXELS {
            return Err("Heightmaps exceed the recipe pixel limit".into());
        }
        let id = (1..).find(|id| !graph.heightmaps.contains_key(id)).unwrap();
        graph.heightmaps.insert(id, image);
        self.add(Operation::Heightmap {
            position: None,
            asset: id,
            center: [0.; 2],
            size: [size as f32; 2],
            low: -30.,
            high: 250.,
        });
        Ok(())
    }

    fn picker(&mut self, ui: &mut egui::Ui) {
        let limit_reached = self
            .draft
            .as_ref()
            .is_none_or(|graph| graph.nodes.len() >= MAX_NODES);
        if limit_reached {
            ui.weak("Node limit reached");
            return;
        }
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search nodes")
                .desired_width(166.),
        );
        focus_search_on_open(ui, &search);
        let submit = (search.has_focus() || search.lost_focus())
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let query = self.search.trim().to_ascii_lowercase();
        let mut chosen = None;
        let mut first = None;
        let mut matches = 0;
        egui::ScrollArea::vertical()
            .max_height(300.)
            .show(ui, |ui| {
                if let Some(graph) = &self.draft {
                    for (&id, definition) in &graph.definitions {
                        if definition.name.to_ascii_lowercase().contains(&query)
                            && let Ok(op) = definition.instance(id)
                        {
                            matches += 1;
                            first.get_or_insert(op);
                            if ui
                                .button(&definition.name)
                                .on_hover_text("Add linked group instance")
                                .clicked()
                            {
                                chosen = Some(op);
                            }
                        }
                    }
                }
                for &(title, op) in controls::CATALOG {
                    if matches!(op, Operation::Heightmap { .. })
                        && self
                            .draft
                            .as_ref()
                            .is_some_and(|graph| graph.heightmaps.is_empty())
                    {
                        continue;
                    }
                    if !title.to_ascii_lowercase().contains(&query) {
                        continue;
                    }
                    matches += 1;
                    first.get_or_insert(op);
                    if ui.button(title).clicked() {
                        chosen = Some(op);
                        break;
                    }
                }
                if matches == 0 {
                    ui.weak("No matching nodes");
                }
            });
        if chosen.is_none() && submit && !query.is_empty() && matches == 1 {
            chosen = first;
        }
        if let Some(op) = chosen {
            self.add(op);
            self.search.clear();
            ui.close();
        }
    }

    fn add(&mut self, mut op: Operation) {
        let Some(graph) = &mut self.draft else { return };
        if let Operation::Heightmap { asset, .. } = &mut op
            && *asset == 0
            && let Some(id) = graph.heightmaps.keys().next()
        {
            *asset = *id;
        }
        if graph.nodes.len() >= MAX_NODES {
            return;
        }
        let Some(id) =
            (1..=MAX_NODES as u32 + 1).find(|id| !graph.nodes.iter().any(|node| node.id == *id))
        else {
            return;
        };
        for (slot, input) in op.inputs().into_iter().enumerate() {
            if let Some(input) = input.filter(|input| !input.optional) {
                let source = graph
                    .nodes
                    .iter()
                    .rev()
                    .find(|node| node.op.port() == input.port)
                    .map(|node| node.id);
                op.set_input(slot, source);
            }
        }
        graph.nodes.push(Node { id, op });
        if graph.output == 0 && op.port() == Port::Height {
            graph.output = id;
        }
        self.selected = Some(id);
        self.locate = Some(id);
        self.marked.clear();
        self.issue = None;
    }

    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        height: f32,
        attention: Option<(u32, Option<usize>)>,
        reveal_issue: bool,
    ) {
        let Some(graph) = &mut self.draft else { return };
        let reveal_definition = self.reveal_definition.take();
        let mut output = None;
        let mut remove = None;
        let heading = ui.strong("Node properties");
        let located = ui.horizontal(|ui| {
            let chosen = find_node(ui, graph, self.selected, &mut self.node_filter);
            let locate = ui.add_enabled(self.selected.is_some(), egui::Button::new("Locate"))
                .on_hover_text("Bring the selected node into view.")
                .clicked();
            if locate { self.selected } else { chosen }
        });
        if let Some(id) = located.inner {
            self.selected = Some(id);
            self.locate = Some(id);
            ui.ctx().request_repaint();
        }
        egui::ScrollArea::vertical()
            .id_salt("node-inspector")
            .animated(ui.style().animation_time > 0.)
            .max_height((height - heading.rect.height() - located.response.rect.height()
                - 2. * ui.spacing().item_spacing.y).max(0.))
            .show(ui, |ui| {
                let Some(index) = graph
                    .nodes
                    .iter()
                    .position(|node| Some(node.id) == self.selected)
                else {
                    ui.weak("Select a node to edit its parameters.");
                    return;
                };
                let node = &graph.nodes[index];
                let id = node.id;
                let mut op = node.op;
                let mut marked = self.marked.contains(&id);
                if ui
                    .checkbox(&mut marked, "Include in group selection")
                    .changed()
                {
                    if marked {
                        self.marked.insert(id);
                    } else {
                        self.marked.remove(&id);
                    }
                }
                if let Some(group) = graph
                    .groups
                    .iter_mut()
                    .find(|group| group.nodes.contains(&id))
                {
                    ui.add(
                        egui::TextEdit::singleline(&mut group.name)
                            .hint_text("Group name")
                            .char_limit(48),
                    );
                }
                if graph.output == id {
                    ui.weak("Terrain output");
                }
                if let Operation::Heightmap { asset, .. } = &mut op {
                    let chooser = egui::ComboBox::from_id_salt(("heightmap-asset", id))
                        .width(ui.available_width())
                        .truncate()
                        .selected_text(
                            graph
                                .heightmaps
                                .get(asset)
                                .map_or("Choose image", |map| &map.label),
                        )
                        .show_ui(ui, |ui| {
                            for (&key, image) in &graph.heightmaps {
                                ui.selectable_value(asset, key, &image.label);
                            }
                        });
                    if reveal_issue && attention == Some((id, None)) {
                        chooser.response.scroll_to_me(None);
                    }
                }
                for (slot, input) in op.inputs().into_iter().enumerate() {
                    let Some(input) = input else { continue };
                    let selected = input
                        .source
                        .and_then(|source| graph.nodes.iter().find(|node| node.id == source))
                        .map(|node| node_label(graph, node))
                        .unwrap_or_else(|| match input.source {
                            Some(id) => format!("Missing #{id}"),
                            None if input.optional => "World coordinates".into(),
                            None => "Connect input".into(),
                        });
                    let mut text = egui::RichText::new(input_label(graph, op, slot, input.name));
                    if attention == Some((id, Some(slot))) {
                        text = text.color(ui.visuals().warn_fg_color);
                    }
                    let label = ui
                        .label(text)
                        .on_hover_text(controls::port_name(input.port));
                    let mut source = input.source;
                    let chooser = egui::ComboBox::from_id_salt((id, slot))
                        .width(ui.available_width())
                        .truncate()
                        .selected_text(&selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut source,
                                None,
                                if input.optional {
                                    "World coordinates"
                                } else {
                                    "Disconnect"
                                },
                            );
                            for from in graph
                                .nodes
                                .iter()
                                .filter(|node| node.id != id && node.op.port() == input.port)
                            {
                                ui.selectable_value(
                                    &mut source,
                                    Some(from.id),
                                    node_label(graph, from),
                                );
                            }
                        })
                        .response
                        .labelled_by(label.id)
                        .on_hover_text(selected);
                    if reveal_issue && attention == Some((id, Some(slot))) {
                        chooser.scroll_to_me(None);
                    }
                    op.set_input(slot, source);
                }
                ui.push_id(id, |ui| controls::parameters(ui, &mut op));
                if let Operation::Group { definition, .. } = op
                    && inspect_definition(ui, graph, definition, reveal_definition == Some(id))
                {
                    self.reveal_definition = Some(id);
                    ui.ctx().request_repaint();
                }
                ui.horizontal_wrapped(|ui| {
                    if op.port() == Port::Height
                        && id != graph.output
                        && ui.button("Set output").clicked()
                    {
                        output = Some(id);
                    }
                    if ui.button("Remove node").clicked() {
                        remove = Some(id);
                    }
                });
                graph.nodes[index].op = op;
            });
        if let Some(id) = output {
            graph.output = id;
        }
        if let Some(id) = remove {
            graph.nodes.retain(|node| node.id != id);
            graph.groups.retain_mut(|group| {
                group.nodes.retain(|&node| node != id);
                !group.nodes.is_empty()
            });
            graph.heightmaps.retain(|&asset, _| {
                graph.nodes.iter().chain(graph.definitions.values().flat_map(|group| &group.nodes)).any(
                    |node| matches!(node.op, Operation::Heightmap { asset: id, .. } if id == asset),
                )
            });
            for node in &mut graph.nodes {
                for (slot, input) in node.op.inputs().into_iter().enumerate() {
                    if input.is_some_and(|input| input.source == Some(id)) {
                        node.op.set_input(slot, None);
                    }
                }
            }
            self.offsets.remove(&id);
            self.marked.remove(&id);
            self.selected = graph.nodes.first().map(|node| node.id);
            if graph.output == id {
                graph.output = graph
                    .nodes
                    .iter()
                    .find(|node| node.op.port() == Port::Height)
                    .map_or(0, |node| node.id);
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui, size: Vec2, attention: Option<(u32, Option<usize>)>) {
        let Some(graph) = &mut self.draft else { return };
        if self
            .layout
            .as_ref()
            .is_none_or(|layout| !layout.matches(graph))
        {
            self.layout = Some(GraphLayout::new(graph));
        }
        let positions = &self.layout.as_ref().unwrap().positions;
        let offsets = &self.offsets;
        let shifted = |id: &u32, base: Vec2| {
            offsets
                .get(id)
                .map_or(base, |&offset| (base + offset).max(Vec2::splat(10.)))
        };
        let position = |id: &u32| positions.get(id).map(|&base| shifted(id, base));
        let mut content = Vec2::splat(1.);
        for (id, &base) in positions {
            content = content.max(shifted(id, base) + CARD + Vec2::splat(24.));
        }
        let fit_zoom = (size.max(Vec2::splat(1.)) / content).min_elem().min(1.);
        if std::mem::take(&mut self.fit) {
            self.zoom = Some(fit_zoom);
            self.fitted_extent = Some(content);
            self.reset_scroll = true;
            self.locate = None;
        }
        let locate = self.locate.take().and_then(|id| position(&id));
        let zoom = self.zoom.unwrap_or(1.);
        let fitted = self.fitted_extent;
        let padding_at = |scale| fitted.map_or(Vec2::ZERO, |bounds| {
            (size - bounds * scale).max(Vec2::ZERO) * 0.5
        });
        let padding = padding_at(zoom);
        let extent = (content * zoom + padding).max(size);
        let scrolling = egui::ScrollArea::both()
            .id_salt("node-canvas")
            .animated(ui.style().animation_time > 0.)
            .auto_shrink([false; 2])
            .max_width(size.x.max(1.))
            .max_height(size.y.max(1.));
        let offset = self.zoom_offset.take();
        let reset = std::mem::take(&mut self.reset_scroll);
        if reset || offset.is_some() {
            let mut state = egui::scroll_area::State::default();
            if !reset && locate.is_none() {
                state.offset = offset.unwrap_or_default();
            }
            state.store(ui.ctx(), ui.make_persistent_id(egui::IdSalt::new("node-canvas")));
        }
        let view = scrolling.show_viewport(ui, |ui, viewport| {
            let (rect, _) = ui.allocate_exact_size(extent, Sense::hover());
            let origin = rect.min + padding;
            if let Some(position) = locate {
                let card = Rect::from_min_size(origin + position * zoom, CARD * zoom);
                ui.scroll_to_rect(card, Some(egui::Align::Center));
            }
            if ui.rect_contains_pointer(rect.intersect(ui.clip_rect())) {
                let (pointer, delta) = ui.input(|input| (input.pointer.hover_pos(), input.zoom_delta()));
                if let Some(pointer) = pointer
                    && delta.is_finite()
                    && delta > 0.
                    && delta != 1.
                {
                    let next = (zoom * delta).clamp(fit_zoom.min(0.25).min(zoom), 2.);
                    if next != zoom {
                        let next_padding = padding_at(next);
                        let target = viewport.min.to_vec2()
                            + (pointer - origin) * (next / zoom - 1.)
                            + next_padding - padding;
                        let max = ((content * next + next_padding).max(size) - viewport.size()).max(Vec2::ZERO);
                        self.zoom = Some(next);
                        self.zoom_offset = Some(target.clamp(Vec2::ZERO, max));
                        ui.ctx().request_repaint();
                    }
                }
            }
            let painter = ui.painter_at(rect);
            grid(ui, &painter, rect, origin, zoom);
            let mut moves = Vec::new();
            for (index, group) in graph.groups.iter().enumerate() {
                let mut bounds = Rect::NOTHING;
                for position in group.nodes.iter().filter_map(position) {
                    let card = Rect::from_min_size(origin + position * zoom, CARD * zoom);
                    bounds = bounds.union(card);
                }
                if !bounds.is_finite() {
                    continue;
                }
                bounds = bounds.expand(8. * zoom);
                bounds.min.y -= 14. * zoom;
                let head = Rect::from_min_max(
                    bounds.min,
                    Pos2::new(bounds.max.x, bounds.min.y + 18. * zoom),
                );
                let mut font = egui::TextStyle::Small.resolve(ui.style());
                font.size *= zoom;
                painter.rect(
                    bounds,
                    5.,
                    ui.visuals().faint_bg_color,
                    ui.visuals().widgets.noninteractive.bg_stroke,
                    egui::StrokeKind::Inside,
                );
                if zoom >= MIN_LABEL_ZOOM {
                    painter.text(
                        head.left_center() + vec2(7. * zoom, 0.),
                        egui::Align2::LEFT_CENTER,
                        &group.name,
                        font,
                        ui.visuals().text_color(),
                    );
                }
                if ui
                    .interact(head, ui.id().with(("group", index)), Sense::drag())
                    .dragged()
                {
                    let delta = ui.input(|input| input.pointer.delta()) / zoom;
                    moves.extend(group.nodes.iter().map(|&id| (id, delta)));
                }
            }
            let mut wire = None;
            for node in &graph.nodes {
                let card =
                    Rect::from_min_size(origin + position(&node.id).unwrap() * zoom, CARD * zoom);
                for (slot, input) in node.op.inputs().into_iter().enumerate() {
                    let Some(input) = input else { continue };
                    if let Some(source) = input.source
                        && let Some(from) = position(&source)
                    {
                        let start = origin + (from + vec2(CARD.x, 44.)) * zoom;
                        let end = inlet(card, slot, zoom);
                        let focused = self.selected == Some(source) || self.selected == Some(node.id);
                        let width = if focused { 2.4 } else { 1.4 };
                        let width = width * (zoom / MIN_LABEL_ZOOM).clamp(0.2, 1.);
                        draw_wire(&painter, start, end, Stroke::new(width, port_color(ui, input.port)));
                    }
                }
            }
            for node in &graph.nodes {
                let card =
                    Rect::from_min_size(origin + position(&node.id).unwrap() * zoom, CARD * zoom);
                if !ui.is_rect_visible(card.expand(5.)) {
                    continue;
                }
                let selected = self.selected == Some(node.id) || self.marked.contains(&node.id);
                draw_node(
                    ui,
                    &painter,
                    graph,
                    node,
                    card,
                    zoom,
                    selected,
                    graph.output == node.id,
                );
                if let Some((_, input)) = attention.filter(|(id, _)| *id == node.id) {
                    let stroke = Stroke::new(1.5, ui.visuals().warn_fg_color);
                    painter.rect_stroke(card.expand(1.), CARD_RADIUS + 1, stroke, egui::StrokeKind::Outside);
                    if let Some(slot) = input {
                        painter.circle_stroke(inlet(card, slot, zoom), 6. * zoom, stroke);
                    }
                }
                let header = Rect::from_min_size(card.min, vec2(CARD.x, 29.) * zoom);
                let name = ui.interact(
                    header,
                    ui.id().with(("node", node.id)),
                    Sense::click_and_drag(),
                );
                let linked = matches!(node.op, Operation::Group { .. });
                let open_definition = linked && name.double_clicked()
                    && !ui.input(|input| input.modifiers.shift);
                if linked {
                    name.clone().on_hover_text("Double-click to edit the shared definition.");
                }
                if open_definition {
                    self.selected = Some(node.id);
                    self.marked.clear();
                    self.reveal_definition = Some(node.id);
                } else if name.clicked() || name.drag_started() {
                    if name.clicked() && ui.input(|input| input.modifiers.shift) {
                        if self.marked.is_empty() {
                            self.marked.extend(self.selected);
                        }
                        if !self.marked.insert(node.id) {
                            self.marked.remove(&node.id);
                        }
                    } else {
                        self.marked.clear();
                    }
                    self.selected = Some(node.id);
                }
                if name.dragged() {
                    let delta = ui.input(|input| input.pointer.delta()) / zoom;
                    moves.push((node.id, delta));
                }
                if zoom < MIN_LABEL_ZOOM {
                    continue;
                }
                let output_point = card.min + vec2(CARD.x, 44.) * zoom;
                let output = ui.interact(
                    Rect::from_center_size(output_point, Vec2::splat(15. * zoom)),
                    ui.id().with(("output", node.id)),
                    Sense::drag(),
                );
                output.dnd_set_drag_payload(OutputDrag {
                    node: node.id,
                    port: node.op.port(),
                });
                output.on_hover_text("Drag to a matching input");
                for (slot, input) in node.op.inputs().into_iter().enumerate() {
                    let Some(input) = input else { continue };
                    let point = inlet(card, slot, zoom);
                    let response = ui.interact(
                        Rect::from_center_size(point, Vec2::splat(17. * zoom)),
                        ui.id().with(("input", node.id, slot)),
                        Sense::hover(),
                    );
                    response
                        .clone()
                        .on_hover_text(controls::port_name(input.port));
                    if let Some(source) = egui::DragAndDrop::payload::<OutputDrag>(ui.ctx())
                        && source.node != node.id
                        && source.port == input.port
                        && response.contains_pointer()
                    {
                        painter.circle_stroke(
                            point,
                            6. * zoom,
                            Stroke::new(1., port_color(ui, source.port)),
                        );
                    }
                    if response.contains_pointer()
                        && let Some(source) = response.dnd_release_payload::<OutputDrag>()
                        && source.node != node.id
                        && source.port == input.port
                    {
                        wire = Some((node.id, slot, source.node));
                    }
                }
            }
            if let Some(source) = egui::DragAndDrop::payload::<OutputDrag>(ui.ctx())
                && let Some(position) = position(&source.node)
                && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
            {
                draw_wire(
                    &painter,
                    origin + (position + vec2(CARD.x, 44.)) * zoom,
                    pointer,
                    Stroke::new(2.4, port_color(ui, source.port)),
                );
            }
            if let Some((id, slot, source)) = wire {
                if let Some(target) = graph.nodes.iter_mut().find(|node| node.id == id) {
                    target.op.set_input(slot, Some(source));
                    self.selected = Some(id);
                }
            }
            moves
        });
        for (id, delta) in view.inner {
            let offset = self.offsets.entry(id).or_default();
            *offset = (*offset + delta).clamp(Vec2::splat(-6000.), Vec2::splat(6000.));
        }
    }
}

fn warning(ui: &mut egui::Ui, message: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(message).color(ui.visuals().warn_fg_color))
            .truncate(),
    )
    .on_hover_text(message);
}

fn operation_name(graph: &Graph, op: Operation) -> &str {
    match op {
        Operation::Group { definition, .. } => graph
            .definitions
            .get(&definition)
            .map_or_else(|| controls::name(op), |group| group.name.as_str()),
        _ => controls::name(op),
    }
}

fn input_label<'a>(
    graph: &'a Graph,
    op: Operation,
    slot: usize,
    fallback: &'static str,
) -> &'a str {
    if let Operation::Group { definition, .. } = op
        && let Some(input) = graph
            .definitions
            .get(&definition)
            .and_then(|group| group.inputs.get(slot))
    {
        return &input.name;
    }
    fallback
}

fn node_label(graph: &Graph, node: &Node) -> String {
    format!("{} · {}", operation_name(graph, node.op), node.id)
}

fn focus_search_on_open(ui: &egui::Ui, search: &egui::Response) {
    if ui.is_sizing_pass() {
        return;
    }
    let frame = search.ctx.cumulative_frame_nr();
    let seen = search.id.with("picker-frame");
    let previous = search.ctx.data_mut(|data| {
        let previous = data.get_temp::<u64>(seen);
        data.insert_temp(seen, frame);
        previous
    });
    if previous.is_none_or(|previous| frame.saturating_sub(previous) > 1) {
        search.request_focus();
    }
}

fn find_node(ui: &mut egui::Ui, graph: &Graph, selected: Option<u32>, filter: &mut String) -> Option<u32> {
    let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F);
    let shortcut_text = ui.ctx().format_shortcut(&shortcut);
    let find = ui.is_enabled() && ui.input_mut(|input| input.consume_shortcut(&shortcut));
    let label = graph.nodes.iter().find(|node| Some(node.id) == selected)
        .map_or_else(|| "Find node".into(), |node| node_label(graph, node));
    let icon = egui::Atom { size: Some(vec2(10., 10.)), ..Default::default() };
    let button = ui.add_sized(
        [174., ui.spacing().interact_size.y],
        egui::Button::new(egui::Atoms::new((label.atom_shrink(true), egui::Atom::grow(), icon))).truncate(),
    ).on_hover_text(format!("Find a node by name, ID, or group. ({shortcut_text})"));
    let center = button.rect.right_center() - vec2(ui.spacing().button_padding.x + 5., 0.);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![center + vec2(-4., -2.), center + vec2(4., -2.), center + vec2(0., 3.)],
        ui.style().interact(&button).fg_stroke.color,
        Stroke::NONE,
    ));
    let mut popup = egui::Popup::menu(&button)
        .width(button.rect.width())
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    if find {
        popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
    }
    popup.show(|ui| {
            let search = ui.add(egui::TextEdit::singleline(filter)
                .desired_width(ui.available_width()).hint_text("Find in graph").char_limit(96));
            focus_search_on_open(ui, &search);
            if find && !ui.is_sizing_pass() {
                search.request_focus();
            }
            let submit = (search.has_focus() || search.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let query = filter.trim().to_lowercase();
            let exact_id = query.strip_prefix('#').unwrap_or(&query).parse::<u32>().ok();
            let matching_groups: BTreeSet<_> = if query.is_empty() || exact_id.is_some() {
                BTreeSet::new()
            } else {
                graph.groups.iter().filter(|group| group.name.to_lowercase().contains(&query))
                    .flat_map(|group| group.nodes.iter().copied()).collect()
            };
            let matches = graph.nodes.iter().filter(|node| match exact_id {
                Some(id) => node.id == id,
                None => query.is_empty() || matching_groups.contains(&node.id)
                    || operation_name(graph, node.op).to_lowercase().contains(&query),
            });
            let mut first = None;
            let mut count = 0;
            let mut chosen = None;
            let height = (ui.ctx().content_rect().height() * 0.45).clamp(56., 234.);
            egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for node in matches {
                    first.get_or_insert(node.id);
                    count += 1;
                    let label = node_label(graph, node);
                    if ui.add(egui::Button::selectable(selected == Some(node.id), &label).truncate())
                        .on_hover_text(label).clicked()
                    {
                        chosen = Some(node.id);
                    }
                }
                if first.is_none() {
                    ui.weak("No matching nodes");
                }
            });
            if chosen.is_none() && submit && !query.is_empty() && count == 1 {
                chosen = first;
            }
            if chosen.is_some() {
                filter.clear();
                ui.close();
            } else if submit {
                search.request_focus();
            }
            chosen
        }).and_then(|popup| popup.inner)
}

fn inspect_definition(ui: &mut egui::Ui, graph: &mut Graph, definition: u32, reveal: bool) -> bool {
    let Some(group) = graph.definitions.get_mut(&definition) else {
        return false;
    };
    let header = egui::CollapsingHeader::new("Edit shared definition")
        .id_salt(("group-definition", definition))
        .open(reveal.then_some(true))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut group.name)
                    .hint_text("Group name")
                    .desired_width(ui.available_width().min(220.))
                    .char_limit(48),
            );
            for input in &mut group.inputs {
                ui.horizontal(|ui| {
                    let label = ui.label(controls::port_name(input.port));
                    ui.add(
                        egui::TextEdit::singleline(&mut input.name)
                            .hint_text("Input name")
                            .desired_width(ui.available_width().min(220.))
                            .char_limit(48),
                    ).labelled_by(label.id);
                });
            }
            let members = group.nodes.clone();
            for node in &mut group.nodes {
                if matches!(node.op, Operation::GroupInput { .. }) {
                    continue;
                }
                let title = format!("{} · {}", controls::name(node.op), node.id);
                egui::CollapsingHeader::new(title)
                    .id_salt(node.id)
                    .show(ui, |ui| {
                        for (slot, input) in node.op.inputs().into_iter().enumerate() {
                            let Some(input) = input else { continue };
                            let mut source = input.source;
                            let label = |op| match op {
                                Operation::GroupInput { slot, .. } => group
                                    .inputs
                                    .get(usize::from(slot))
                                    .map_or("Group input", |input| input.name.as_str()),
                                _ => controls::name(op),
                            };
                            let selected = members
                                .iter()
                                .find(|from| Some(from.id) == source)
                                .map_or_else(
                                    || {
                                        if input.optional {
                                            "World coordinates".into()
                                        } else {
                                            "Connect input".into()
                                        }
                                    },
                                    |from| format!("{} · {}", label(from.op), from.id),
                                );
                            let name = ui.label(input.name);
                            egui::ComboBox::from_id_salt(slot)
                                .selected_text(selected)
                                .show_ui(ui, |ui| {
                                    if input.optional {
                                        ui.selectable_value(&mut source, None, "World coordinates");
                                    }
                                    for from in members.iter().filter(|from| {
                                        from.id != node.id && from.op.port() == input.port
                                    }) {
                                        ui.selectable_value(
                                            &mut source,
                                            Some(from.id),
                                            format!("{} · {}", label(from.op), from.id),
                                        );
                                    }
                                })
                                .response
                                .labelled_by(name.id);
                            node.op.set_input(slot, source);
                        }
                        controls::parameters(ui, &mut node.op);
                    });
            }
        });
    let fully_open = header.fully_open();
    let header = header.header_response
        .on_hover_text("Edits apply wherever this definition is used.");
    if reveal && fully_open {
        header.scroll_to_me(Some(egui::Align::Min));
    }
    reveal && !fully_open
}

fn arrange(graph: &Graph) -> BTreeMap<u32, Vec2> {
    let mut levels = BTreeMap::<u32, usize>::new();
    let ids: BTreeSet<_> = graph.nodes.iter().map(|node| node.id).collect();
    for _ in 0..graph.nodes.len() {
        let prior = levels.len();
        for node in &graph.nodes {
            if levels.contains_key(&node.id) {
                continue;
            }
            let mut level = 0;
            let ready = node
                .op
                .inputs()
                .into_iter()
                .flatten()
                .filter_map(|port| port.source)
                .filter(|id| ids.contains(id))
                .all(|id| {
                    levels.get(&id).is_some_and(|from| {
                        level = level.max(from + 1);
                        true
                    })
                });
            if ready {
                levels.insert(node.id, level);
            }
        }
        if levels.len() == prior {
            break;
        }
    }
    let members: BTreeSet<_> = graph
        .groups
        .iter()
        .flat_map(|g| g.nodes.iter().copied())
        .collect();
    let ungrouped = graph
        .nodes
        .iter()
        .filter(|node| !members.contains(&node.id))
        .map(|node| node.id)
        .collect::<Vec<_>>();
    let mut result = BTreeMap::new();
    let mut first_row = 0;
    for band in
        std::iter::once(ungrouped.as_slice()).chain(graph.groups.iter().map(|g| g.nodes.as_slice()))
    {
        let mut rows = [0; MAX_NODES];
        for &id in band {
            let column = levels.get(&id).copied().unwrap_or(0);
            let row = first_row + rows[column];
            rows[column] += 1;
            let auto = vec2(column as f32 * STEP.x + 24., row as f32 * STEP.y + 23.);
            result.insert(id, auto);
        }
        first_row += rows.into_iter().max().unwrap_or(0);
    }
    result
}

fn inlet(card: Rect, slot: usize, zoom: f32) -> Pos2 {
    card.min + vec2(0., 63. + slot as f32 * 18.) * zoom
}

fn draw_wire(painter: &egui::Painter, start: Pos2, end: Pos2, stroke: Stroke) {
    let bend = ((end.x - start.x) * 0.52).max(24.);
    let points = [start, start + vec2(bend, 0.), end - vec2(bend, 0.), end];
    if !painter
        .clip_rect()
        .intersects(Rect::from_points(&points).expand(2.))
    {
        return;
    }
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
}

fn port_color(ui: &egui::Ui, port: Port) -> Color32 {
    match (ui.visuals().dark_mode, port) {
        (true, Port::Position) => Color32::from_rgb(143, 177, 224),
        (true, Port::Mask) => Color32::from_rgb(140, 192, 164),
        (true, Port::Height) => Color32::from_rgb(224, 180, 126),
        (true, Port::Distance) => Color32::from_rgb(211, 160, 219),
        (false, Port::Position) => Color32::from_rgb(52, 92, 151),
        (false, Port::Mask) => Color32::from_rgb(44, 113, 78),
        (false, Port::Height) => Color32::from_rgb(137, 82, 33),
        (false, Port::Distance) => Color32::from_rgb(119, 55, 131),
    }
}

fn grid(ui: &egui::Ui, painter: &egui::Painter, rect: Rect, origin: Pos2, zoom: f32) {
    let visible = rect.intersect(ui.clip_rect());
    painter.rect_filled(visible, 0., ui.visuals().extreme_bg_color);
    let mark = ui.visuals().weak_text_color().gamma_multiply(
        if zoom < MIN_LABEL_ZOOM { 0.16 } else { 0.25 },
    );
    let major = 88. * zoom;
    let fine = zoom >= 0.75;
    let step = if fine {
        major / 4.
    } else {
        major * 2_f32.powf((22. / major).log2().ceil().max(0.))
    };
    let first = ((visible.min - origin) / step).floor();
    let last = ((visible.max - origin) / step).ceil();
    for y in first.y as i32..last.y as i32 {
        for x in first.x as i32..last.x as i32 {
            let point = origin + vec2(x as f32, y as f32) * step;
            if !fine || x % 4 == 0 && y % 4 == 0 {
                let stroke = Stroke::new(1., mark);
                for arm in [vec2(2., 0.), vec2(0., 2.)] {
                    painter.line_segment([point - arm, point + arm], stroke);
                }
            } else {
                painter.circle_filled(point, 0.75, mark);
            }
        }
    }
}

fn draw_node(
    ui: &egui::Ui,
    painter: &egui::Painter,
    graph: &Graph,
    node: &Node,
    rect: Rect,
    zoom: f32,
    selected: bool,
    output: bool,
) {
    let visual = ui.visuals();
    let border = if selected {
        visual.selection.stroke
    } else {
        visual.widgets.inactive.bg_stroke
    };
    painter.rect(
        rect,
        CARD_RADIUS,
        visual.window_fill(),
        border,
        egui::StrokeKind::Inside,
    );
    let inset = zoom.max(1.);
    painter.rect_filled(
        Rect::from_min_size(rect.min + Vec2::splat(inset), vec2((rect.width() - 2. * inset).max(0.), 27. * zoom)),
        CARD_RADIUS.saturating_sub(inset.floor() as u8),
        if selected {
            visual.selection.bg_fill
        } else {
            visual.widgets.inactive.bg_fill
        },
    );
    if zoom < MIN_LABEL_ZOOM {
        return;
    }
    let mut font = egui::TextStyle::Body.resolve(ui.style());
    font.size *= zoom;
    let text_painter = painter.with_clip_rect(rect);
    text_painter.text(
        rect.min + vec2(10., 14.) * zoom,
        egui::Align2::LEFT_CENTER,
        operation_name(graph, node.op),
        font.clone(),
        visual.text_color(),
    );
    let text = if output {
        "Terrain output"
    } else {
        controls::port_name(node.op.port())
    };
    let output_point = rect.min + vec2(CARD.x, 44.) * zoom;
    painter.circle_filled(output_point, 3.8 * zoom, port_color(ui, node.op.port()));
    painter.text(
        output_point - vec2(10., 0.) * zoom,
        egui::Align2::RIGHT_CENTER,
        text,
        font.clone(),
        visual.weak_text_color(),
    );
    for (slot, input) in node.op.inputs().into_iter().enumerate() {
        let Some(input) = input else { continue };
        let p = inlet(rect, slot, zoom);
        let color = port_color(ui, input.port);
        painter.circle(
            p,
            3.5 * zoom,
            if input.source.is_some() {
                color
            } else {
                visual.window_fill()
            },
            Stroke::new(1., color),
        );
        text_painter.text(
            p + vec2(10., 0.) * zoom,
            egui::Align2::LEFT_CENTER,
            input_label(graph, node.op, slot, input.name),
            font.clone(),
            visual.text_color(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }

    fn fixture() -> Project {
        let profile: mapper_studio::native_profile::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/native/client-0f703d43.json"
        ))
        .unwrap();
        let mut project = Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            composition_engine: Some(
                papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into(),
            ),
            installation: Some(mapper_studio::installation::Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: mapper_studio::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: profile.native_sha256,
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Project::default()
        };
        project.monument_rules = Some(
            serde_json::from_value(serde_json::json!({
                "engine": mapper_studio::monument_source::VERSION,
                "source_sha256": "a".repeat(64),
                "surface_modifiers": mapper_studio::monument_source::SOURCE_PRIORITY_VERSION,
            }))
            .unwrap(),
        );
        assert!(project.supports_terrain_graph());
        project
    }

    fn demo(mask: u32) -> Graph {
        Graph {
            version: VERSION,
            output: 3,
            heightmaps: Default::default(),
            groups: Vec::new(),
            definitions: Default::default(),
            nodes: vec![
                Node {
                    id: 1,
                    op: Operation::NativeHeight,
                },
                Node {
                    id: 2,
                    op: Operation::Mask { coverage: 0.5 },
                },
                Node {
                    id: 3,
                    op: Operation::Raise {
                        height: 1,
                        mask,
                        metres: 40.,
                    },
                },
            ],
        }
    }

    fn frame(
        ctx: &egui::Context,
        panel: &mut Panel,
        project: &mut Project,
        size: [usize; 2],
        events: Vec<egui::Event>,
        textures: &mut snapshot::Textures,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    Pos2::ZERO,
                    vec2(size[0] as f32, size[1] as f32),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| panel.show(ui, project),
        );
        textures.update(&output.textures_delta);
        output.textures_delta.clear();
        output
    }

    fn text_position(output: &egui::FullOutput, label: &str) -> Option<Pos2> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                let center = text.pos + text.galley.size() * 0.5;
                shape.clip_rect.contains(center).then_some(center)
            }
            _ => None,
        })
    }

    fn pointer(pos: Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn node_search_accepts_a_unique_keyboard_match() {
        let ctx = egui::Context::default();
        let mut panel = Panel::default();
        panel.replace(Some(demo(2)));
        let mut render = |events| {
            let mut output = ctx.run_ui(egui::RawInput { events, focused: true, ..Default::default() },
                |ui| panel.picker(ui));
            output.textures_delta.clear();
        };
        render(vec![]);
        render(vec![
            egui::Event::Text("constant height".into()),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let graph = panel.draft.as_ref().unwrap();
        assert_eq!(graph.nodes.len(), 4);
        assert!(matches!(graph.nodes.last().unwrap().op, Operation::Height { .. }));
        assert!(panel.search.is_empty());
    }

    #[test]
    fn canvas_zoom_keeps_the_point_under_the_pointer_still() {
        let ctx = egui::Context::default();
        let mut textures = snapshot::Textures::default();
        let mut panel = Panel::default();
        let graph = demo(2);
        panel.replace(Some(graph.clone()));
        panel.offsets.insert(3, vec2(720., 390.));
        let mut project = fixture();
        let mut render = |panel: &mut Panel, events| {
            frame(&ctx, panel, &mut project, [940, 560], events, &mut textures)
        };
        let wire_start = |output: &egui::FullOutput| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::CubicBezier(wire) => Some(wire.points[0]),
                _ => None,
            }).unwrap()
        };
        render(&mut panel, vec![]);
        let point = wire_start(&render(&mut panel, vec![]));
        for delta in [1.4, 1. / 1.4] {
            render(&mut panel, vec![egui::Event::PointerMoved(point), egui::Event::Zoom(delta)]);
            let output = render(&mut panel, vec![]);
            let start = wire_start(&output);
            assert!(start.distance(point) < 1.5, "{start:?} moved away from {point:?}");
        }
        assert!((panel.zoom.unwrap() - 1.).abs() < 0.001);
        assert_eq!(panel.draft.as_ref(), Some(&graph));
        assert!(project.terrain_graph.is_none());
    }

    #[test]
    fn fit_shows_a_long_graph_below_the_previous_zoom_floor() {
        let ctx = egui::Context::default();
        crate::workbench_style::install(&ctx);
        let mut textures = snapshot::Textures::default();
        let mut graph = demo(2);
        for id in 4..=30 {
            graph.nodes.push(Node {
                id,
                op: Operation::Raise { height: id - 1, mask: 2, metres: 1. },
            });
        }
        graph.output = 30;
        let mut panel = Panel::default();
        panel.replace(Some(graph));
        panel.fit = true;
        let mut project = fixture();
        let size = [440, 620];
        frame(&ctx, &mut panel, &mut project, size, vec![], &mut textures);
        let output = frame(&ctx, &mut panel, &mut project, size, vec![], &mut textures);
        let zoom = panel.zoom.unwrap();
        assert!(zoom > 0. && zoom < 0.25);
        let cards: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(card) if (card.rect.size() - CARD * zoom).length() < 0.05
                && shape.clip_rect.contains_rect(card.rect) => Some((card.rect, shape.clip_rect)),
            _ => None,
        }).collect();
        assert_eq!(cards.len(), 30);
        let bounds = cards.iter().fold(Rect::NOTHING, |bounds, (card, _)| bounds.union(*card));
        assert!((bounds.center().y - cards[0].1.center().y).abs() < 4.);
        assert!(output.shapes.iter().any(|shape| matches!(shape.shape, egui::Shape::CubicBezier(_))));
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "Raise terrain"
        )));
        if let Some(directory) = std::env::var_os("MAPPER_NODE_ZOOM_SNAPSHOTS") {
            let path = std::path::PathBuf::from(directory).join("fit-long-graph.png");
            let image = textures.raster(&ctx, output, size);
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
    }

    #[test]
    fn inspector_search_selects_and_locates_an_existing_node() {
        for (query, keyboard) in [("shoreline", true), ("#2", false)] {
            let ctx = egui::Context::default();
            super::super::workbench_style::install(&ctx);
            ctx.all_styles_mut(|style| style.animation_time = 0.);
            let mut textures = snapshot::Textures::default();
            let mut panel = Panel::default();
            let mut graph = demo(2);
            graph.groups.push(NodeGroup { name: "Shoreline".into(), nodes: vec![2] });
            panel.replace(Some(graph.clone()));
            let offset = vec2(1100., 420.);
            panel.offsets.insert(2, offset);
            let mut project = fixture();
            let mut render = |panel: &mut Panel, events| {
                frame(&ctx, panel, &mut project, [940, 560], events, &mut textures)
            };
            let visible = |output: &egui::FullOutput, label: &str| {
                output.shapes.iter().rev().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        let center = text.pos + text.galley.size() * 0.5;
                        shape.clip_rect.contains(center).then_some(center)
                    }
                    _ => None,
                })
            };
            render(&mut panel, vec![]);
            let initial = render(&mut panel, vec![]);
            let operation = operation_name(&graph, graph.nodes[1].op);
            assert!(visible(&initial, operation).is_none());
            if keyboard {
                render(&mut panel, vec![egui::Event::Key {
                    key: egui::Key::F, physical_key: None, pressed: true,
                    repeat: false, modifiers: egui::Modifiers::COMMAND,
                }]);
            } else {
                let chooser = visible(&initial, "Raise terrain · 3").unwrap();
                render(&mut panel, pointer(chooser, true));
                render(&mut panel, pointer(chooser, false));
            }
            for _ in 0..2 {
                render(&mut panel, vec![]);
            }
            render(&mut panel, vec![egui::Event::Text(query.into())]);
            let filtered = render(&mut panel, vec![]);
            assert_eq!(panel.node_filter, query);
            let title = node_label(&graph, &graph.nodes[1]);
            let result = visible(&filtered, &title).unwrap();
            let field = visible(&filtered, query).unwrap();
            assert!((0. ..36.).contains(&(result.y - field.y)));
            assert_eq!(panel.selected, Some(3));
            if keyboard {
                render(&mut panel, vec![egui::Event::Key {
                    key: egui::Key::Enter, physical_key: None, pressed: true,
                    repeat: false, modifiers: egui::Modifiers::NONE,
                }]);
            } else {
                render(&mut panel, pointer(result, true));
                render(&mut panel, pointer(result, false));
            }
            assert_eq!(panel.selected, Some(2));
            assert!(panel.node_filter.is_empty());
            for _ in 0..3 {
                render(&mut panel, vec![]);
            }
            let located = render(&mut panel, vec![]);
            assert!(visible(&located, operation).is_some());
            assert_eq!(panel.offsets[&2], offset);
            assert_eq!(panel.draft, Some(graph));
            assert!(project.terrain_graph.is_none());
            if keyboard && let Some(directory) = std::env::var_os("MAPPER_NODE_SEARCH_TASK_DIR") {
                let image = textures.raster(&ctx, filtered, [940, 560]);
                let path = std::path::PathBuf::from(directory).join("node-search.png");
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        }
    }

    #[test]
    fn locate_brings_an_offscreen_node_into_view_without_moving_it() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut textures = snapshot::Textures::default();
        let mut panel = Panel::default();
        let graph = demo(2);
        panel.replace(Some(graph.clone()));
        let offset = vec2(1100., 420.);
        panel.offsets.insert(3, offset);
        let mut project = fixture();
        let mut render = |panel: &mut Panel, events| {
            frame(&ctx, panel, &mut project, [940, 560], events, &mut textures)
        };
        render(&mut panel, vec![]);
        let initial = render(&mut panel, vec![]);
        assert!(text_position(&initial, "Raise terrain").is_none());
        let button = text_position(&initial, "Locate").unwrap();
        render(&mut panel, pointer(button, true));
        render(&mut panel, pointer(button, false));
        assert_eq!(panel.locate, Some(3));
        for _ in 0..3 {
            render(&mut panel, vec![]);
        }
        let located = render(&mut panel, vec![]);
        assert!(text_position(&located, "Raise terrain").is_some());
        assert_eq!(panel.offsets[&3], offset);
        assert_eq!(panel.draft, Some(graph));
        assert!(project.terrain_graph.is_none());
    }

    #[test]
    fn layout_reuses_value_edits_and_invalidates_wiring_and_group_membership() {
        let mut graph = demo(2);
        graph.groups.push(NodeGroup {
            name: "Elevation".into(),
            nodes: vec![2, 3],
        });
        let layout = GraphLayout::new(&graph);
        assert!(layout.positions[&3].x > layout.positions[&2].x);
        graph.nodes[2].op = Operation::Raise {
            height: 1,
            mask: 2,
            metres: 12.,
        };
        graph.groups[0].name = "Terrain".into();
        assert!(layout.matches(&graph));
        let mut rewired = graph.clone();
        rewired.nodes[2].op.set_input(0, None);
        assert!(!layout.matches(&rewired));
        let mut reordered = graph.clone();
        reordered.nodes.swap(0, 1);
        assert!(!layout.matches(&reordered));
        graph.groups[0].nodes.remove(0);
        assert!(!layout.matches(&graph));
        assert_ne!(GraphLayout::new(&graph).positions[&3], layout.positions[&3]);
    }

    #[test]
    fn invisible_wire_culling_keeps_curves_that_cross_the_viewport() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let clip = Rect::from_min_size(Pos2::ZERO, vec2(70., 70.));
            let painter = ui.painter().with_clip_rect(clip);
            for y in [30., 200.] {
                draw_wire(
                    &painter,
                    Pos2::new(-200., y),
                    Pos2::new(220., y),
                    Stroke::new(1.4, Color32::WHITE),
                );
            }
        });
        output.textures_delta.clear();
        assert_eq!(
            output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::Shape::CubicBezier(_)))
                .count(),
            1
        );
    }

    #[test]
    fn moving_a_node_preserves_base_layout_and_moves_its_wires() {
        let ctx = egui::Context::default();
        let mut textures = snapshot::Textures::default();
        let mut panel = Panel::default();
        panel.replace(Some(demo(2)));
        let mut project = fixture();
        let size = [940, 560];
        let mut render = |panel: &mut Panel, events| {
            frame(&ctx, panel, &mut project, size, events, &mut textures)
        };
        render(&mut panel, vec![]);
        let initial = render(&mut panel, vec![]);
        let header = initial
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Raise terrain" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap();
        let wire_end = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::CubicBezier(wire) => Some(wire.points[3]),
                    _ => None,
                })
                .unwrap()
        };
        let base = panel.layout.as_ref().unwrap().positions[&3];
        let end = header + vec2(22., 36.);
        render(&mut panel, pointer(header, true));
        render(&mut panel, vec![egui::Event::PointerMoved(end)]);
        render(&mut panel, pointer(end, false));
        let moved = render(&mut panel, vec![]);
        assert_eq!(panel.layout.as_ref().unwrap().positions[&3], base);
        assert_eq!(panel.offsets[&3], end - header);
        assert!((wire_end(&moved) - wire_end(&initial) - (end - header)).length() < 0.01);
    }

    #[test]
    fn draft_validation_reuses_unchanged_graphs_and_rechecks_edits_and_recipe_context() {
        let mut project = fixture();
        let mut panel = Panel::default();
        panel.replace(Some(demo(2)));
        assert_eq!(panel.draft_issue(&project), None);
        let checked = panel.validation.as_ref().unwrap().graph.nodes.as_ptr();
        panel.offsets.insert(3, vec2(20., 10.));
        assert_eq!(panel.draft_issue(&project), None);
        assert_eq!(
            panel.validation.as_ref().unwrap().graph.nodes.as_ptr(),
            checked
        );

        project.size = 3000;
        assert_eq!(panel.draft_issue(&project), None);
        assert_eq!(panel.validation.as_ref().unwrap().size, 3000);
        panel.draft.as_mut().unwrap().nodes[2].op.set_input(1, None);
        assert!(panel.draft_issue(&project).unwrap().contains("Connect"));
        panel.draft.as_mut().unwrap().nodes[2]
            .op
            .set_input(1, Some(2));
        assert_eq!(panel.draft_issue(&project), None);

        project.installation = None;
        assert!(
            panel
                .draft_issue(&project)
                .unwrap()
                .contains("Rust installation")
        );
        panel.replace(None);
        assert_eq!(panel.draft_issue(&project), None);
        assert!(panel.validation.is_none());
    }

    #[test]
    fn graph_issue_locates_and_marks_an_offscreen_input() {
        let ctx = egui::Context::default();
        crate::workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut textures = snapshot::Textures::default();
        let mut panel = Panel::default();
        panel.replace(Some(demo(0)));
        panel.issue = Some("Heightmap could not be opened".into());
        panel.selected = Some(1);
        panel.offsets.insert(3, vec2(1100., 420.));
        let mut project = fixture();
        let size = [920, 550];
        let mut render = |panel: &mut Panel, events| {
            frame(&ctx, panel, &mut project, size, events, &mut textures)
        };
        render(&mut panel, vec![]);
        let initial = render(&mut panel, vec![]);
        assert!(text_position(&initial, "Raise terrain").is_none());
        assert!(text_position(&initial, "Connect Mask on Raise terrain").is_some());
        assert!(text_position(&initial, "Heightmap could not be opened").is_some());
        let dismiss = text_position(&initial, "Dismiss").unwrap();
        render(&mut panel, pointer(dismiss, true));
        render(&mut panel, pointer(dismiss, false));
        assert!(panel.issue.is_none());
        let draft = render(&mut panel, vec![]);
        let go = text_position(&draft, "Go to node").unwrap();
        render(&mut panel, pointer(go, true));
        render(&mut panel, pointer(go, false));
        for _ in 0..2 {
            render(&mut panel, vec![]);
        }
        let located = render(&mut panel, vec![]);
        assert_eq!(panel.selected, Some(3));
        assert!(text_position(&located, "Raise terrain").is_some());
        assert!(located.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Circle(circle) if circle.radius == 6.
        )));
        panel.draft.as_mut().unwrap().nodes[2].op.set_input(1, Some(2));
        let repaired = render(&mut panel, vec![]);
        assert!(text_position(&repaired, "Go to node").is_none());
        assert!(panel.validation.as_ref().unwrap().issue.is_none());
        assert!(project.terrain_graph.is_none());
        if let Some(directory) = std::env::var_os("MAPPER_NODE_ISSUE_SNAPSHOTS") {
            let path = std::path::PathBuf::from(directory).join("node-issue.png");
            let image = textures.raster(&ctx, located, size);
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
    }

    #[test]
    fn graph_wiring_edits_a_draft_and_apply_commits_the_recipe() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        crate::workbench_style::install(&ctx);
        let mut textures = snapshot::Textures::default();
        let mut panel = Panel::default();
        panel.replace(Some(demo(0)));
        let mut project = fixture();
        let size = [920, 550];
        let mut render = |panel: &mut Panel, project: &mut Project, events| {
            frame(&ctx, panel, project, size, events, &mut textures)
        };
        let opened = render(&mut panel, &mut project, vec![]);
        assert!(project.terrain_graph.is_none());
        assert!(text_position(&opened, "Unapplied changes").is_some());
        let find = |radius: f32| {
            opened
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Circle(circle)
                        if circle.radius == radius
                            && (circle.fill == Color32::from_rgb(140, 192, 164)
                                || circle.stroke.color == Color32::from_rgb(140, 192, 164)) =>
                    {
                        Some(circle.center)
                    }
                    _ => None,
                })
                .unwrap()
        };
        let source = find(3.8);
        let target = find(3.5);
        render(&mut panel, &mut project, pointer(source, true));
        render(
            &mut panel,
            &mut project,
            vec![egui::Event::PointerMoved(target)],
        );
        render(&mut panel, &mut project, pointer(target, false));
        assert_eq!(panel.draft.as_ref().unwrap(), &demo(2));
        assert!(project.terrain_graph.is_none());
        let updated = render(&mut panel, &mut project, vec![]);
        let apply = text_position(&updated, "Apply to recipe").unwrap();
        render(&mut panel, &mut project, pointer(apply, true));
        let applied = render(&mut panel, &mut project, pointer(apply, false));
        assert!(text_position(&applied, "In Recipe").is_some());
        assert!(text_position(&applied, "Unapplied changes").is_none());
        assert_eq!(project.terrain_graph.as_deref(), Some(&demo(2)));

        panel.selected = Some(2);
        let selected = render(&mut panel, &mut project, vec![]);
        let remove = selected
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Remove node" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap();
        render(&mut panel, &mut project, pointer(remove, true));
        render(&mut panel, &mut project, pointer(remove, false));
        panel.add(Operation::Mask { coverage: 0.2 });
        let draft = panel.draft.as_ref().unwrap();
        assert!(draft.nodes.iter().any(|node| node.id == 2));
        assert!(matches!(
            draft.nodes[1].op,
            Operation::Raise { mask: 0, .. }
        ));
        assert!(draft.validate(project.size).is_err());
        assert_eq!(project.terrain_graph.as_deref(), Some(&demo(2)));

        let draft = draft.clone();
        let offset = vec2(30., 45.);
        panel.offsets.insert(3, offset);
        for action in ["Keep editing", "Apply removal"] {
            let current = render(&mut panel, &mut project, vec![]);
            let graph = text_position(&current, "Graph").unwrap();
            render(&mut panel, &mut project, pointer(graph, true));
            render(&mut panel, &mut project, pointer(graph, false));
            let menu = render(&mut panel, &mut project, vec![]);
            let remove = text_position(&menu, "Remove graph").unwrap();
            render(&mut panel, &mut project, pointer(remove, true));
            render(&mut panel, &mut project, pointer(remove, false));
            let pending = render(&mut panel, &mut project, vec![]);
            assert!(text_position(&pending, "Removal pending").is_some());
            assert_eq!(project.terrain_graph.as_deref(), Some(&demo(2)));
            let point = text_position(&pending, action).unwrap();
            render(&mut panel, &mut project, pointer(point, true));
            render(&mut panel, &mut project, pointer(point, false));
            if action == "Keep editing" {
                assert_eq!(panel.draft.as_ref(), Some(&draft));
                assert_eq!(panel.offsets.get(&3), Some(&offset));
                assert_eq!(project.terrain_graph.as_deref(), Some(&demo(2)));
            }
        }
        let removed = render(&mut panel, &mut project, vec![]);
        assert!(project.terrain_graph.is_none());
        assert!(text_position(&removed, "Start from recipe").is_some());
        assert!(text_position(&removed, "Removal pending").is_none());
    }

    #[test]
    fn a_full_size_heightmap_inside_a_linked_group_survives_recipe_export() {
        let mut panel = Panel::default();
        let mut project = fixture();
        panel
            .add_heightmap(
                papyrus_terrain::terrain_graph::Heightmap {
                    label: "Survey".into(),
                    dimensions: [512; 2],
                    samples: vec![65535; MAX_HEIGHTMAP_PIXELS].into(),
                },
                project.size,
            )
            .unwrap();
        panel.add(Operation::Mask { coverage: 0.5 });
        let mut graph = panel.draft.take().unwrap();
        graph.groups.push(NodeGroup {
            name: "Height layers".into(),
            nodes: vec![1, 2],
        });
        graph.link_group(0, 1).unwrap();
        assert!(graph.validate(project.size).is_ok());
        project.terrain_graph = Some(Arc::new(graph));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Height layers.rec");
        let profile = mapper_studio::profile::Profile::new("Height layers".into(), project);
        profile.save(&file).unwrap();
        let reopened = mapper_studio::profile::Profile::load_snapshot(&file).unwrap();
        assert_eq!(reopened.recipe.terrain_graph, profile.recipe.terrain_graph);
    }

    #[test]
    fn double_clicking_a_linked_group_reveals_its_definition_in_a_compact_dock() {
        let ctx = egui::Context::default();
        crate::workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| {
            style.animation_time = 0.;
            style.scroll_animation = egui::style::ScrollAnimation::none();
        });
        let mut graph = demo(2);
        graph.groups.push(NodeGroup { name: "Elevation pass".into(), nodes: vec![2, 3] });
        let linked = graph.link_group(0, 3).unwrap();
        let mut panel = Panel::default();
        panel.replace(Some(graph.clone()));
        panel.selected = Some(1);
        let mut project = fixture();
        let mut textures = snapshot::Textures::default();
        let size = [440, 620];
        let mut render = |panel: &mut Panel, events| {
            frame(&ctx, panel, &mut project, size, events, &mut textures)
        };
        let visible = |output: &egui::FullOutput, caption: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == caption => {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    shape.clip_rect.contains_rect(rect).then_some(rect.center())
                }
                _ => None,
            })
        };
        render(&mut panel, vec![]);
        let initial = render(&mut panel, vec![]);
        assert!(visible(&initial, "Edit shared definition").is_none());
        let point = visible(&initial, "Elevation pass").unwrap();
        for pressed in [true, false, true, false] {
            render(&mut panel, pointer(point, pressed));
        }
        for _ in 0..4 {
            render(&mut panel, vec![]);
        }
        let opened = render(&mut panel, vec![]);
        assert_eq!(panel.selected, Some(linked));
        assert!(visible(&opened, "Edit shared definition").is_some());
        assert!(visible(&opened, "Raise terrain · 3").is_some());
        assert_eq!(panel.draft, Some(graph));
        assert!(project.terrain_graph.is_none());
        if let Some(directory) = std::env::var_os("MAPPER_GROUP_OPEN_SNAPSHOTS") {
            let image = textures.raster(&ctx, opened, size);
            let path = std::path::PathBuf::from(directory).join("open-linked-group.png");
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
    }

    #[test]
    fn resizing_the_inspector_changes_its_width_without_editing_the_graph() {
        let ctx = egui::Context::default();
        super::super::workbench_style::install(&ctx);
        let graph = demo(2);
        let mut panel = Panel::default();
        panel.replace(Some(graph.clone()));
        let mut project = fixture();
        let size = [940, 560];
        let mut textures = snapshot::Textures::default();
        let locate_x = |output: &egui::FullOutput| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Locate" => Some(text.pos.x),
                _ => None,
            }).unwrap()
        };
        let mut render = |panel: &mut Panel, events| {
            frame(&ctx, panel, &mut project, size, events, &mut textures)
        };
        render(&mut panel, vec![]);
        let before = locate_x(&render(&mut panel, vec![]));
        let divider_x = size[0] as f32 - INSPECTOR_WIDTH
            - ctx.global_style().spacing.item_spacing.x - 0.5 * INSPECTOR_SPLITTER;
        let start = Pos2::new(divider_x, 85.);
        let end = start - vec2(72., 0.);
        render(&mut panel, pointer(start, true));
        render(&mut panel, vec![egui::Event::PointerMoved(end)]);
        render(&mut panel, pointer(end, false));
        let resized = render(&mut panel, vec![]);
        assert_eq!(panel.inspector_width, Some(INSPECTOR_WIDTH + 72.));
        assert!((before - locate_x(&resized) - 72.).abs() < 1.);

        render(&mut panel, pointer(end, true));
        render(&mut panel, pointer(end, false));
        render(&mut panel, vec![egui::Event::Key {
            key: egui::Key::ArrowLeft, physical_key: None, pressed: true,
            repeat: false, modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(panel.inspector_width, Some(INSPECTOR_WIDTH + 92.));
        render(&mut panel, vec![]);
        let reset = end - vec2(20., 0.);
        for _ in 0..2 {
            render(&mut panel, pointer(reset, true));
            render(&mut panel, pointer(reset, false));
        }
        let restored = render(&mut panel, vec![]);
        assert_eq!(panel.inspector_width, None);
        assert!((before - locate_x(&restored)).abs() < 1.);
        assert_eq!(panel.draft, Some(graph));
        assert!(project.terrain_graph.is_none());
        if let Some(directory) = std::env::var_os("MAPPER_NODE_RESIZE_SNAPSHOTS") {
            let image = textures.raster(&ctx, resized, size);
            let path = std::path::PathBuf::from(directory).join("nodes-resized.png");
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
    }

    #[test]
    fn graph_workspace_renders_wide_and_compact_variants() {
        for (dark, size) in [(true, [940, 560]), (false, [440, 620])] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            crate::workbench_style::install(&ctx);
            let mut textures = snapshot::Textures::default();
            let mut project = fixture();
            let mut graph = demo(2);
            graph.groups.push(NodeGroup {
                name: "Elevation pass".into(),
                nodes: vec![2, 3],
            });
            if !dark {
                graph.link_group(0, 3).unwrap();
            }
            project.terrain_graph = Some(Arc::new(graph));
            let mut panel = Panel::default();
            frame(&ctx, &mut panel, &mut project, size, vec![], &mut textures);
            let output = frame(&ctx, &mut panel, &mut project, size, vec![], &mut textures);
            for label in [
                "Add node",
                "Apply to recipe",
                "In Recipe",
                if dark {
                    "Raise terrain"
                } else {
                    "Edit shared definition"
                },
                "Terrain output",
                "Elevation pass",
            ] {
                assert!(
                    output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text == label)),
                    "{label}"
                );
            }
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| matches!(shape.shape, egui::Shape::CubicBezier(_)))
            );
            if let Some(directory) = std::env::var_os("MAPPER_NODE_SNAPSHOTS") {
                let filename = if dark {
                    "nodes-wide.png"
                } else {
                    "nodes-compact.png"
                };
                let image = textures.raster(&ctx, output, size);
                mapper_studio::image_export::write_png(
                    &std::path::PathBuf::from(directory).join(filename),
                    &image,
                    || false,
                )
                .unwrap();
            }
        }
    }
}
