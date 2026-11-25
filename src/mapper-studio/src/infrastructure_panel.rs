use super::*;
use mapper_studio::{AssetIdentity, infrastructure::Source, prefab_assets::Catalog};
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{self, Receiver},
};
#[derive(Clone, PartialEq, Eq)]
struct Key {
    catalog: String,
    pin: Option<String>,
    surface_pin: Option<String>,
    roadside_pin: Option<String>,
    root: std::path::PathBuf,
}
struct Discovery {
    source: Source,
    previous_source: Option<Source>,
    surface: Option<String>,
    joined: Option<mapper_studio::road_cost_source::Profile>,
    ordered: Option<mapper_studio::source_road_planning::Policy>,
    roadside: Option<String>,
    warning: Option<String>,
}
type SourceDiscovery = Result<Discovery, String>;
struct Task {
    key: Key,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<SourceDiscovery>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct RoadPanel {
    wanted: Option<Key>,
    source: Option<Source>,
    previous_source: Option<Source>,
    surface: Option<String>,
    roadside: Option<String>,
    joined: Option<mapper_studio::road_cost_source::Profile>,
    ordered: Option<mapper_studio::source_road_planning::Policy>,
    task: Option<Task>,
    message: String,
    defaults_applied: Option<Project>,
}
impl RoadPanel {
    #[cfg(test)]
    pub(super) fn seed_source(&mut self, source: Source) {
        self.source = Some(source);
    }

    pub fn accept_prepared(&mut self, project: &Project, root: PathBuf, catalog: &Catalog,
        prepared: mapper_studio::client_preparation::ReadyRoads) {
        let pin = project.roads.as_ref().map(|r| r.source_sha256.clone());
        let surface_pin = project.roads.as_ref().and_then(|r| r.surface_sha256.clone());
        let roadside_pin = project.roads.as_ref().and_then(|r| r.staged.as_ref()).map(|s| s.roadside_source_sha256.clone());
        if pin.as_ref().is_some_and(|pin| pin != &prepared.source.sha256
            && prepared.previous_source.as_ref().is_none_or(|old| pin != &old.sha256)) {
            return;
        }
        if prepared.previous_source.is_none() && (surface_pin.as_ref().is_some_and(|pin| pin != &prepared.surface)
            || roadside_pin.as_ref().is_some_and(|pin| pin != &prepared.roadside)) {
            return;
        }
        self.task = None;
        self.defaults_applied = None;
        self.wanted = Some(Key { root, catalog: catalog.sha256.clone(), pin, surface_pin, roadside_pin });
        self.source = Some(prepared.source);
        self.previous_source = prepared.previous_source;
        self.surface = Some(prepared.surface);
        self.roadside = Some(prepared.roadside);
        self.joined = prepared.joined;
        self.ordered = Some(prepared.ordered);
        self.message.clear();
    }
    fn prepared_matches(&self, next: &Key) -> bool {
        self.task.is_none()
            && self.wanted.as_ref().is_some_and(|old| old.root == next.root && old.catalog == next.catalog)
            && self.source.as_ref().is_some_and(|source| next.pin.as_ref().is_none_or(|pin| pin == &source.sha256))
            && next.surface_pin.as_ref().is_none_or(|pin| self.surface.as_ref() == Some(pin))
            && next.roadside_pin.as_ref().is_none_or(|pin| self.roadside.as_ref() == Some(pin))
    }
    pub fn ready(&self,project:&Project)->bool {
        project.roads.as_ref().is_none_or(|r|self.task.is_none() && self.source.as_ref().is_some_and(|s|s.sha256==r.source_sha256))
    }
    fn staged_unavailable(&self, project: &Project) -> Option<&'static str> {
        let Some(source) = self.source.as_ref() else {
            return Some("Prepare road data from the selected Rust installation first.");
        };
        let Some(roads) = project.roads.as_ref() else {
            return Some("Enable road generation first.");
        };
        if roads.source_sha256 != source.sha256 {
            return Some("Wait for the saved road profile to finish loading.");
        }
        if roads.engine != papyrus_terrain::road_network::COST_VERSION {
            return Some("Update the road profile to use roadside generation.");
        }
        if !project.performance.monuments {
            return Some("Enable Monuments in Recipe first.");
        }
        if project
            .monument_rules
            .as_ref()
            .is_none_or(|r| r.coastal_profile.is_none())
        {
            return Some("Waiting for installation monument rules.");
        }
        if project.rails.is_some() && !mapper_studio::rail_generation::before_roads(project) {
            return Some("Road terrain fitting with this rail workflow is not supported yet.");
        }
        if self.surface.is_none() {
            return Some("Matching paved-road surface data is not available.");
        }
        if self.roadside.is_none() {
            return Some("Roadside data is not available for the selected Rust installation.");
        }
        if roads
            .surface_sha256
            .as_ref()
            .is_some_and(|pin| Some(pin) != self.surface.as_ref())
        {
            return Some("Wait for the saved road surfaces to finish loading.");
        }
        if roads
            .staged
            .as_ref()
            .is_some_and(|p| Some(&p.roadside_source_sha256) != self.roadside.as_ref())
        {
            return Some("Restore the saved roadside data or disable this workflow.");
        }
        None
    }
    fn set_staged(&self, project: &mut Project, enabled: bool) -> Result<(), String> {
        if !enabled {
            let mut next = project.clone();
            if let Some(roads) = next.roads.as_mut() {
                roads.set_global_earthwork(false)?;
                roads.staged = None;
            }
            next.validate()?;
            *project = next;
            return Ok(());
        }
        if let Some(reason) = self.staged_unavailable(project) {
            return Err(reason.into());
        }
        let mut next = project.clone();
        let has_late_rails =
            next.rails.is_some() && !mapper_studio::rail_generation::before_roads(&next);
        let roads = next.roads.as_mut().ok_or("Enable roads first")?;
        roads
            .surface_sha256
            .get_or_insert(self.surface.clone().ok_or("Missing road surfaces")?);
        roads.staged = Some(mapper_studio::road_generation::Policy::for_new_recipe(
            self.roadside.clone().ok_or("Missing roadside data")?,
            self.ordered.clone(),
            has_late_rails,
        )?);
        next.validate()?;
        *project = next;
        Ok(())
    }
    #[cfg(test)]
    fn set_access_profile(
        roads: &mut mapper_studio::infrastructure::Recipe,
        source: &Source,
        profile: Option<&mapper_studio::road_cost_source::Profile>,
    ) -> Result<(), String> {
        let mut next = roads.clone();
        if profile.is_none() {
            next.entrance_launch = None;
        }
        if let Some(profile) = profile {
            profile.check(source)?;
            next.access_search = Some(papyrus_terrain::road_network::COMPONENT_VERSION.into());
        }
        next.access_river_water = profile
            .and_then(|p| p.joins.as_ref())
            .and_then(|j| j.river_water.clone());
        let budget=next.access_costmap.as_ref().map(|p|p.maximum_work);
        next.access_costmap = profile.cloned();
        if let (Some(budget),Some(p))=(budget,next.access_costmap.as_mut()) {p.maximum_work=p.maximum_work.min(budget);}
        next.access_smoothing = profile.map(|_| papyrus_terrain::road_smoothing::VERSION.into());
        next.validate()?;
        *roads = next;
        Ok(())
    }
    #[cfg(test)]
    fn set_ring_profile(
        roads: &mut mapper_studio::infrastructure::Recipe,
        source: &Source,
        profile: Option<&mapper_studio::road_cost_source::Profile>,
    ) -> Result<(), String> {
        let mut next = roads.clone();
        if let Some(profile) = profile {
            profile.check(source)?;
            if next.source_sha256 != source.sha256
                || next.engine != papyrus_terrain::road_network::COST_VERSION
            {
                return Err("Source-based routing requires matching source-cost roads".into());
            }
            next.main_ring = Some(true);
            next.ring_search = Some(papyrus_terrain::road_ring::RECOVERY_VERSION.into());
        }
        let budget=next.ring_costmap.as_ref().map(|p|p.maximum_work);
        next.ring_costmap = profile.cloned();
        if let (Some(budget),Some(p))=(budget,next.ring_costmap.as_mut()) {p.maximum_work=p.maximum_work.min(budget);}
        next.validate()?;
        *roads = next;
        Ok(())
    }

    pub fn tick(
        &mut self,
        project: &Project,
        catalog: Option<Arc<Catalog>>,
        root: Option<std::path::PathBuf>,
        ctx: &egui::Context,
    ) {
        let key = catalog
            .as_ref()
            .filter(|_| {
                root.is_some() && mapper_studio::native_preview::is_native(&project.preview_engine)
            })
            .map(|c| Key {
                catalog: c.sha256.clone(),
                pin: project.roads.as_ref().map(|r| r.source_sha256.clone()),
                surface_pin: project
                    .roads
                    .as_ref()
                    .and_then(|r| r.surface_sha256.clone()),
                roadside_pin: project
                    .roads
                    .as_ref()
                    .and_then(|r| r.staged.as_ref())
                    .map(|p| p.roadside_source_sha256.clone()),
                root: root.clone().unwrap(),
            });
        // Binding or disabling prepared roads changes recipe pins, not the
        // installation data already verified by this panel's worker.
        if let Some(next) = &key
            && self.prepared_matches(next)
        {
            self.wanted = key.clone();
        }
        if self.wanted != key {
            self.task = None;
            self.defaults_applied = None;
            self.source = None;
            self.previous_source = None;
            self.surface = None;
            self.roadside = None;
            self.joined = None;
            self.message.clear();
            self.wanted = key.clone();
            if let Some((key, catalog)) = key.zip(catalog) {
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let (sender, receiver) = mpsc::channel();
                let pin = key.pin.clone();
                let root = key.root.clone();
                let surface_pin = key.surface_pin.clone();
                let roadside_pin = key.roadside_pin.clone();
                let repaint = ctx.clone();
                std::thread::spawn(move || {
                    let _verification=mapper_studio::installation::VerificationScope::default();
                    let _loading =
                        mapper_studio::loading_progress::begin("Road data", Some(signal.clone()));
                    let result =
                        mapper_studio::road_extract::prepare(&root, &catalog, pin.as_deref(), || {
                            signal.load(Ordering::Relaxed)
                        })
                        .and_then(|source| {
                            let candidate=if pin.is_some() {let candidate=Source::discover(&catalog,None,||signal.load(Ordering::Relaxed))?;
                                (candidate.profile.revision>source.profile.revision).then_some(candidate)} else {None};
                            let (source,previous_source)=if let Some(candidate)=candidate {(candidate,Some(source))}else{(source,None)};
                            let surface_pin=if previous_source.is_some() {None}else{surface_pin.as_deref()};
                            let roadside_pin=if previous_source.is_some() {None}else{roadside_pin.as_deref()};
                            let surface_result = if let Some(pin) = surface_pin {
                                mapper_studio::road_surface_source::Source::cached(
                                    &root,
                                    pin,
                                    &source,
                                    || signal.load(Ordering::Relaxed),
                                )
                                .map(Some)
                            } else {
                                mapper_studio::road_extract::prepare_surface(&root, &source, || {
                                    signal.load(Ordering::Relaxed)
                                })
                            };
                            let (surface, warning) = match surface_result {
                                Ok(surface) => (surface.map(|s| s.sha256), None),
                                Err(error) => {
                                    (None, Some(format!("Road surfaces unavailable: {error}")))
                                }
                            };
                            let roadside_result = if let Some(pin) = roadside_pin {
                                mapper_studio::roadside_source::Source::cached(
                                    pin,
                                    &root,
                                    &catalog,
                                    || signal.load(Ordering::Relaxed),
                                )
                                .map(Some)
                            } else {
                                mapper_studio::roadside_source::prepare(&root, &catalog, || {
                                    signal.load(Ordering::Relaxed)
                                })
                            };
                            let roadside_result = roadside_result.and_then(|source| {
                                let Some(source) = source else {
                                    return Ok(None);
                                };
                                let paint =
                                    mapper_studio::road_paint_source::Profile::for_source(&source)?
                                        .ok_or("No matching roadside paint policy")?;
                                mapper_studio::road_terrain_source::Profile::for_paint(&paint)?
                                    .ok_or("No matching roadside terrain policy")?;
                                Ok(Some(source.sha256))
                            });
                            let (roadside, roadside_warning) = match roadside_result {
                                Ok(pin) => (pin, None),
                                Err(error) => (
                                    None,
                                    Some(format!("Roadside generation unavailable: {error}")),
                                ),
                            };
                            let warning = match (warning, roadside_warning) {
                                (Some(a), Some(b)) => Some(format!("{a}. {b}")),
                                (a, b) => a.or(b),
                            };
                            let ordered=mapper_studio::road_extract::prepare_ordered(&root,&catalog,&source,None,||signal.load(Ordering::Relaxed))?
                                .map(|inputs|mapper_studio::source_road_planning::Policy::bind(&inputs));
                            let joined =
                                mapper_studio::road_cost_source::Profile::bundled_joined(&source)?;
                            Ok(Discovery {
                                source,
                                previous_source,
                                surface,
                                joined,
                                ordered,
                                roadside,
                                warning,
                            })
                        });
                    let _ = sender.send(result);
                    repaint.request_repaint();
                });
                self.task = Some(Task {
                    key,
                    cancel,
                    receiver,
                });
            }
        }
        let result = self
            .task
            .as_ref()
            .and_then(|t| match t.receiver.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Infrastructure discovery worker ended".into()))
                }
                Err(TryRecvError::Empty) => None,
            });
        if let Some(result) = result {
            let task = self.task.take().unwrap();
            if self.wanted.as_ref() == Some(&task.key) && !task.cancel.load(Ordering::Relaxed) {
                match result {
                    Ok(Discovery {
                        source,
                        previous_source,
                        surface,
                        joined,
                        ordered,
                        roadside,
                        warning,
                    }) => {
                        self.defaults_applied = None;
                        self.message = warning.unwrap_or_default();
                        self.source = Some(source);
                        self.previous_source = previous_source;
                        self.surface = surface;
                        self.roadside = roadside;
                        self.joined = joined;
                        self.ordered = ordered;
                    }
                    Err(e) => self.message = e,
                }
            }
        }
    }
    /// Repaint may run many times with the same recipe and prepared inputs.
    pub fn refresh_defaults(&mut self, project: &mut Project) -> Result<(), String> {
        if project.roads.is_none() || self.source.is_none() {
            self.defaults_applied = None;
        } else if self.defaults_applied.as_ref() != Some(project) {
            self.apply_defaults(project)?;
            self.defaults_applied = Some(project.clone());
        }
        Ok(())
    }

    pub fn apply_defaults(&self, project:&mut Project) -> Result<(),String> {
        if project.roads.is_none() {return Ok(());}
        let Some(source)=&self.source else {return Ok(());};
        let mut next=project.clone();
        mapper_studio::generation_defaults::apply(&mut next);
        let roads=next.roads.as_mut().unwrap();
        if roads.source_sha256!=source.sha256 {
            let previous=self.previous_source.as_ref().ok_or("Road metadata does not match the prepared installation")?;
            *roads=roads.rebind_source(previous,source,self.surface.clone(),self.roadside.clone(),self.ordered.clone())?;
        }
        roads.apply_source_defaults(source)?;
        // apply_source_defaults binds both cost profiles atomically. Rebinding
        // only the ring first can invalidate an already ordered road recipe.
        roads.apply_correctness_defaults();
        if roads.surface_sha256.is_none() {roads.surface_sha256=self.surface.clone();}
        if self.staged_unavailable(&next).is_none() {
            if next.roads.as_ref().unwrap().staged.is_none() {self.set_staged(&mut next,true)?;}
            next.roads.as_mut().unwrap().set_global_earthwork(true)?;
        }
        if let Some(staged)=next.roads.as_mut().and_then(|r|r.staged.as_mut())
            && staged.layout.is_none() {staged.layout=self.ordered.clone();}
        next.roads.as_mut().unwrap().apply_correctness_defaults();
        next.validate()?;*project=next;Ok(())
    }
    pub fn show(&mut self, ui: &mut egui::Ui, project: &mut Project, pins: Option<AssetIdentity>, reveal: bool) {
        let native = mapper_studio::native_preview::is_native(&project.preview_engine);
        let enabled = project.roads.is_some();
        let (mut header, changed) = super::panels::recipe_section(
            ui, "Roads & trails", enabled, enabled || (native && self.source.is_some() && pins.is_some()), false,
            "Generate roads using the selected installation’s road data.",
        );
        if let Some(enabled) = changed {
            if enabled {
                let mut next=project.clone();
                let result=(||->Result<(),String>{
                    let pins=pins.as_ref().ok_or("Waiting for monument assets")?;
                    next.check_monument_assets(&pins.catalog_sha256,&pins.bindings_sha256)?;
                    next.monument_assets.get_or_insert(pins.clone());
                    let source = self.source.as_ref().ok_or("Waiting for road metadata")?;
                    let roads = if self.joined.as_ref().is_some_and(|p| p.policy.sampling.is_some()) {
                        mapper_studio::infrastructure::Recipe::for_prepared_source(
                            source,
                            self.surface.clone().ok_or("Waiting for road surfaces")?,
                            self.roadside.clone().ok_or("Waiting for roadside data")?,
                            self.ordered.clone().ok_or("Waiting for ordered road inputs")?,
                            next.rails.is_some() && !mapper_studio::rail_generation::before_roads(&next),
                        )?
                    } else {
                        mapper_studio::infrastructure::Recipe::for_source(source)
                    };
                    next.roads = Some(roads);
                    self.apply_defaults(&mut next)?;next.validate()
                })();
                match result {Ok(())=>{*project=next;self.message.clear();},Err(error)=>self.message=error}
            } else {project.roads=None;project.rails=None;project.objects=None;}
        }
        if reveal {
            header.set_open(true);
        }
        let (toggle, _) = header.body(|ui| self.road_settings(ui, project));
        if reveal {
            toggle.scroll_to_me(Some(egui::Align::Min));
        }
    }

    fn road_settings(&mut self, ui: &mut egui::Ui, project: &mut Project) {
        if !mapper_studio::native_preview::is_native(&project.preview_engine) {
            ui.weak("Roads require native terrain");
            return;
        }
        if let Some(policy) = project.roads.as_mut().and_then(|roads| roads.staged.as_mut())
            && !policy.roadside_preferences.is_empty()
            && ui.button("Clear roadside moves")
                .on_hover_text("Use automatic roadside placement on the next generation.")
                .clicked()
        {
            policy.roadside_preferences.clear();
        }
        if let (Some(roads),Some(source))=(&mut project.roads,&self.source) {
            if source.profile.ring.is_some() && roads.engine==papyrus_terrain::road_network::COST_VERSION {
                if let Some(ring) = super::panels::generation_override(
                    ui, "Ring road", roads.main_ring,
                    "Generate a loop for the main road.",
                    "Generate paved roads without a main loop.",
                    "Use the generator default, which enables a ring road.",
                ) {
                    let mut next = roads.clone();
                    next.main_ring = ring;
                    next.ring_costmap = if ring != Some(false) && next.ring_search.is_some() {
                        next.access_costmap.clone()
                    } else {
                        None
                    };
                    match next.validate() {
                        Ok(()) => *roads = next,
                        Err(error) => self.message = error,
                    }
                }
            }
            if source.profile.trails.is_some() && roads.engine==papyrus_terrain::road_network::COST_VERSION {
                if let Some(trails) = super::panels::generation_override(
                    ui, "Dirt trails", roads.include_trails,
                    "Generate dirt trails alongside paved roads.",
                    "Skip dirt trails and keep paved roads.",
                    "Use the generator default, which includes dirt trails.",
                ) {
                    roads.include_trails = trails;
                }
            }
            let source_completion = roads.staged.as_ref().is_some_and(|p|
                mapper_studio::road_generation::is_source_engine(&p.engine));
            ui.collapsing("Advanced routing", |ui| {
                egui::Grid::new("road-routing-limits")
                    .num_columns(2)
                    .min_col_width(0.)
                    .show(ui, |ui| {
                        let label = ui.label("Grade (%)").on_hover_text(if source_completion {
                            "Guides route planning. Road terrain follows the selected Rust installation."
                        } else {
                            "Maximum grade allowed by this recipe's road fitting."
                        });
                        let mut value = roads.maximum_grade * 100.;
                        if ui.add(egui::DragValue::new(&mut value)
                            .range(1. ..=50.).speed(0.5)).labelled_by(label.id).changed()
                        {
                            roads.maximum_grade = value / 100.;
                        }
                        ui.end_row();
                        let label = ui.label("Cut/fill (m)").on_hover_text(if source_completion {
                            "Guides route planning. Installation-defined road shaping can differ from this guide."
                        } else {
                            "Maximum terrain change allowed by this recipe's road fitting."
                        });
                        ui.add(egui::DragValue::new(&mut roads.maximum_cut_fill)
                            .range(0. ..=24.).speed(0.1)).labelled_by(label.id);
                        ui.end_row();
                    });
            });
            if let Some(reason)=self.staged_unavailable(project) {ui.small(reason);}
        } else if self.task.is_some() {
            ui.spinner();ui.small("Preparing installation road data…");
            if ui.button("Cancel road preparation").clicked() {self.task=None;self.message="Road preparation cancelled".into();}
        }
        if self.source.is_none() && self.task.is_none() && ui.button("Retry road preparation").clicked() {self.wanted=None;self.message.clear();}
        if !self.message.is_empty() {diagnostic_ui::message(ui,&self.message,"road-details");}
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }

    fn staged_fixture() -> (RoadPanel, Project, AssetIdentity) {
        let (mut panel, mut project, pins) = fixture();
        panel.surface = Some("d".repeat(64));
        panel.roadside = Some("e".repeat(64));
        let rules = project.monument_rules.as_mut().unwrap();
        rules.engine = mapper_studio::coastal_placement::VERSION.into();
        rules.coastal_profile = Some(
            serde_json::from_str(include_str!(
                "../../../compatibility/monuments/client-0f703d43-coastal.json"
            ))
            .unwrap(),
        );
        project.placement_engine = mapper_studio::coastal_placement::VERSION.into();
        project.validate().unwrap();
        (panel, project, pins)
    }



    #[test]
    fn explicit_access_binding_adopts_matching_river_policy_and_disabling_clears_it() {
        let (panel, _, _) = fixture();
        let source = panel.source.as_ref().unwrap();
        let profile: mapper_studio::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-rivers.json"
        ))
        .unwrap();
        let mut roads = mapper_studio::infrastructure::Recipe::for_source(source);
        RoadPanel::set_access_profile(&mut roads, source, Some(&profile)).unwrap();
        assert_eq!(
            roads.access_river_water,
            profile.joins.as_ref().unwrap().river_water
        );
        RoadPanel::set_access_profile(&mut roads, source, None).unwrap();
        assert!(roads.access_river_water.is_none());
        roads.validate().unwrap();
    }
    fn fixture() -> (RoadPanel, Project, AssetIdentity) {
        let bound: mapper_studio::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-joins.json"
        ))
        .unwrap();
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-topology.json"
        ))
        .unwrap();
        let base = bound.sources.len() - profile["code_sources"].as_array().unwrap().len() - 1;
        let mut source:Source=serde_json::from_value(serde_json::json!({
            "format":"papyrus-infrastructure-source","schema":1,"adapter":"test",
            "catalog_sha256":"a".repeat(64),"sources":bound.sources[..base],"extractor_sha256":"b".repeat(64),
            "profile":profile,"prefabs":{}
        })).unwrap();
        source.sha256 = bound.infrastructure_sha256.clone();
        bound.check(&source).unwrap();
        let pins = AssetIdentity {
            catalog_sha256: "a".repeat(64),
            bindings_sha256: "b".repeat(64),
        };
        let project = Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            composition_engine: Some(papyrus_terrain::monument_composition::VERSION.into()),
            installation: Some(mapper_studio::installation::Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: bound.sources[0].clone(),
                scene_manifest: Some(bound.sources[1].clone()),
            }),
            monument_assets: Some(pins.clone()),
            monument_rules: Some(mapper_studio::monument_source::Recipe {
                engine: mapper_studio::monument_source::VERSION.into(),
                source_sha256: "c".repeat(64),
                coastal_profile: None,
                tier_search: None,
                earthwork: None,
                coastal_normals: None,
                coastal_navigation: None,
                coastal_search: None,
                canonical_yaw: None,
                land_alignment: None,
                surface_modifiers: None,
                stage_rules: None,
                dungeon_source: None, offshore_source: None,
            }),
            ..Default::default()
        };
        project.validate().unwrap();
        (
            RoadPanel {
                source: Some(source),
                joined: Some(bound),
                ..Default::default()
            },
            project,
            pins,
        )
    }
    fn frame(
        ctx: &egui::Context,
        panel: &mut RoadPanel,
        project: &mut Project,
        pins: &AssetIdentity,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 1200.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                crate::panels::open_recipe_section(ui, "Roads & trails");
                panel.show(ui, project, Some(pins.clone()), false);
            },
        );
        out.textures_delta.clear();
        out
    }
    fn click(
        ctx: &egui::Context,
        panel: &mut RoadPanel,
        project: &mut Project,
        pins: &AssetIdentity,
        label: &str,
    ) {
        let (row, label) = label.split_once('/').map_or((None, label), |(row, label)| (Some(row), label));
        let mut output = frame(ctx, panel, project, pins, vec![]);
        if label != "Advanced routing"
            && !output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text==label))
        {
            ctx.all_styles_mut(|s| s.animation_time = 0.);
            click(ctx, panel, project, pins, "Advanced routing");
            output = frame(ctx, panel, project, pins, vec![]);
        }
        let row_y = row.map(|row| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == row => Some(text.pos.y),
            _ => None,
        }).unwrap_or_else(|| panic!("Missing road setting: {row}")));
        let pos = if label == "Roads & trails" {
            crate::panels::recipe_checkbox_position(&output, label)
        } else { output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label
                    && row_y.is_none_or(|y| (t.pos.y - y).abs() < 4.) => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing road control: {label}")) };
        for pressed in [true, false] {
            frame(
                ctx,
                panel,
                project,
                pins,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    #[test]
    fn road_preparation_can_be_cancelled_and_retried_without_changing_the_recipe() {
        let ctx = egui::Context::default();
        let (mut panel, mut project, pins) = fixture();
        let before = project.clone();
        panel.source = None;
        let key = Key {
            catalog: "a".repeat(64),
            pin: None,
            surface_pin: None,
            roadside_pin: None,
            root: std::path::PathBuf::from("C:/test/Rust"),
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        panel.wanted = Some(key.clone());
        panel.task = Some(Task {
            key,
            cancel: cancel.clone(),
            receiver,
        });
        click(
            &ctx,
            &mut panel,
            &mut project,
            &pins,
            "Cancel road preparation",
        );
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(before, project);
        assert_eq!(panel.message, "Road preparation cancelled");
        drop(sender);
        panel.task = None;
        click(
            &ctx,
            &mut panel,
            &mut project,
            &pins,
            "Retry road preparation",
        );
        assert!(panel.wanted.is_none());
        assert_eq!(before, project);
    }







    #[test]
    fn source_ring_binding_rejects_mismatch_atomically() {
        let (panel, _, _) = fixture();
        let source = panel.source.as_ref().unwrap();
        let mut roads = mapper_studio::infrastructure::Recipe::for_source(source);
        roads.include_trails = Some(false);
        roads.maximum_grade = 0.17;
        let mut bad = panel.joined.clone().unwrap();
        bad.sources[0].bytes += 1;
        let before = roads.clone();
        assert!(RoadPanel::set_ring_profile(&mut roads, source, Some(&bad)).is_err());
        assert_eq!(roads, before);
        RoadPanel::set_ring_profile(&mut roads, source, panel.joined.as_ref()).unwrap();
        assert_eq!(roads.maximum_grade, 0.17);
        assert_eq!(roads.include_trails, Some(false));
    }

    #[test]
    fn roads_enable_all_available_corrections_and_keep_content_choices() {
        let ctx=egui::Context::default();let(mut panel,mut project,pins)=staged_fixture();
        let seeds=project.seeds;let targets=project.targets.clone();
        click(&ctx,&mut panel,&mut project,&pins,"Roads & trails");
        let roads=project.roads.as_ref().unwrap();assert!(roads.access_smoothing.is_some());assert!(roads.access_costmap.is_some());
        assert!(roads.staged.is_some());assert!(roads.surface_sha256.is_some());
        assert_eq!(roads.staged.as_ref().unwrap().engine,mapper_studio::road_generation::VERSION);
        assert_eq!(roads.entrance_launch.unwrap().engine,papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork);
        assert_eq!(project.seeds,seeds);assert_eq!(project.targets,targets);
        click(&ctx,&mut panel,&mut project,&pins,"Dirt trails/No");assert_eq!(project.roads.as_ref().unwrap().include_trails,Some(false));
        let stable=project.clone();panel.apply_defaults(&mut project).unwrap();assert_eq!(project,stable);
        click(&ctx, &mut panel, &mut project, &pins, "Dirt trails/?");
        assert_eq!(project.roads.as_ref().unwrap().include_trails, None);
    }
    #[test]
    fn enabling_and_disabling_roads_reuses_the_prepared_installation() {
        let ctx = egui::Context::default();
        let (mut panel, mut project, pins) = staged_fixture();
        let root = std::path::PathBuf::from("prepared-client");
        let catalog = Arc::new(Catalog {
            sha256: pins.catalog_sha256.clone(),
            installation: project.installation.clone().unwrap(),
            entries: Default::default(),
            sources: vec![],
        });
        let prepared = mapper_studio::client_preparation::ReadyRoads {
            source: panel.source.take().unwrap(), previous_source: None,
            surface: panel.surface.take().unwrap(), roadside: panel.roadside.take().unwrap(),
            joined: panel.joined.take(),
            ordered: mapper_studio::source_road_planning::Policy {
                engine: mapper_studio::source_road_planning::VERSION.into(),
                assets_sha256: "a".repeat(64), roles_sha256: "b".repeat(64),
                ring_clearance: None, terrain_entrances: None,
            },
        };
        panel = RoadPanel::default();
        panel.accept_prepared(&project, root.clone(), &catalog, prepared);
        panel.tick(&project, Some(catalog.clone()), Some(root.clone()), &ctx);
        assert!(panel.task.is_none(), "Startup data must be used directly");
        let source = panel.source.as_ref().unwrap().sha256.clone();
        for _ in 0..2 {
            click(&ctx, &mut panel, &mut project, &pins, "Roads & trails");
            panel.tick(&project, Some(catalog.clone()), Some(root.clone()), &ctx);
            assert!(panel.task.is_none(), "A content toggle must not reload prepared road data");
            assert_eq!(panel.source.as_ref().unwrap().sha256, source);
        }
        let key = panel.wanted.clone().unwrap();
        for changed in [
            Key { root: "other-client".into(), ..key.clone() },
            Key { catalog: "0".repeat(64), ..key.clone() },
            Key { pin: Some("0".repeat(64)), ..key.clone() },
            Key { surface_pin: Some("0".repeat(64)), ..key.clone() },
            Key { roadside_pin: Some("0".repeat(64)), ..key.clone() },
        ] {
            assert!(!panel.prepared_matches(&changed));
        }
    }

    #[test]
    fn prepared_road_controls_preserve_the_saved_implicit_ring_default() {
        let ctx=egui::Context::default();let(mut panel,mut project,pins)=staged_fixture();
        click(&ctx,&mut panel,&mut project,&pins,"Roads & trails");
        project.roads.as_mut().unwrap().main_ring=None;
        let saved=project.clone();
        let bytes=serde_json::to_vec(&saved).unwrap();
        for _ in 0..3 {
            panel.apply_defaults(&mut project).unwrap();
            frame(&ctx,&mut panel,&mut project,&pins,vec![]);
            assert_eq!(project,saved);
            assert_eq!(serde_json::to_vec(&project).unwrap(),bytes);
            let roads=project.roads.as_ref().unwrap();
            assert!(roads.main_ring!=Some(false));
            assert!(roads.ring_costmap.is_some());
        }
    }
    #[test]
    fn new_ordered_roads_use_connected_source_completion_and_keep_strict_contracts() {
        let ctx=egui::Context::default();let(mut panel,mut project,pins)=staged_fixture();
        panel.ordered=Some(mapper_studio::source_road_planning::Policy {
            engine:mapper_studio::source_road_planning::VERSION.into(),
            assets_sha256:"a".repeat(64),roles_sha256:"b".repeat(64),
            ring_clearance:None,terrain_entrances:None,
        });
        click(&ctx,&mut panel,&mut project,&pins,"Roads & trails");
        assert_eq!(project.roads.as_ref().unwrap().staged.as_ref().unwrap().engine,mapper_studio::road_generation::CONNECTED_SOURCE_VERSION);
        assert!(project.roads.as_ref().unwrap().include_trails.unwrap_or(true));
        let stable=project.clone();panel.apply_defaults(&mut project).unwrap();assert_eq!(project,stable);
        for engine in [mapper_studio::road_generation::VERSION,mapper_studio::road_generation::SOURCE_VERSION,mapper_studio::road_generation::CONNECTED_SOURCE_VERSION] {
            project.roads.as_mut().unwrap().staged.as_mut().unwrap().engine=engine.into();
            project.roads.as_mut().unwrap().include_trails=Some(false);
            let saved=project.clone();panel.apply_defaults(&mut project).unwrap();assert_eq!(project,saved);
        }
    }
    #[test]
    fn current_native_roads_enable_atomically_with_all_prepared_inputs() {
        let ctx = egui::Context::default();
        let (mut panel, mut project, pins) = fixture();
        let costs: mapper_studio::road_cost_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-8dd4e235-road-reservations.json"
        )).unwrap();
        let profile: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-8dd4e235-road-topology.json"
        )).unwrap();
        let mut source: Source = serde_json::from_value(serde_json::json!({
            "format":"papyrus-infrastructure-source", "schema":2,
            "adapter":"papyrus-path-connectors-2", "catalog_sha256":pins.catalog_sha256,
            "sources":costs.sources[..3], "extractor_sha256":"b".repeat(64),
            "profile":profile, "prefabs":{}
        })).unwrap();
        source.sha256 = costs.infrastructure_sha256.clone();
        project.installation.as_mut().unwrap().native = costs.sources[0].clone();
        project.installation.as_mut().unwrap().scene_manifest = Some(costs.sources[1].clone());
        panel.source = Some(source);
        panel.joined = Some(costs);
        panel.surface = Some("c".repeat(64));
        panel.roadside = Some("d".repeat(64));
        panel.ordered = Some(mapper_studio::source_road_planning::Policy {
            engine:mapper_studio::source_road_planning::VERSION.into(),
            assets_sha256:"e".repeat(64), roles_sha256:"f".repeat(64),
            ring_clearance:None, terrain_entrances:None,
        });
        click(&ctx, &mut panel, &mut project, &pins, "Roads & trails");
        let roads = project.roads.as_ref().unwrap();
        assert!(roads.include_trails.unwrap_or(true));
        assert_eq!(roads.access_costmap.as_ref().unwrap().policy.sampling.as_deref(),
            Some(papyrus_terrain::road_costmap::NATIVE_SAMPLING_VERSION));
        assert_eq!(roads.access_costmap, roads.ring_costmap);
        let saved = project.clone();
        panel.apply_defaults(&mut project).unwrap();
        assert_eq!(project, saved);
        let current = panel.source.as_ref().unwrap();
        let mut previous: Source = serde_json::from_value(serde_json::to_value(current).unwrap()).unwrap();
        previous.profile.revision -= 1;
        previous.sha256 = "1".repeat(64);
        let mut historical = project.roads.clone().unwrap();
        historical.source_sha256 = previous.sha256.clone();
        historical.main_ring = Some(false);
        historical.ring_costmap = None;
        historical.include_trails = Some(false);
        let costs = historical.access_costmap.as_mut().unwrap();
        costs.infrastructure_sha256 = previous.sha256.clone();
        costs.policy.sampling = None;
        costs.maximum_work = 1_000_000;
        historical.validate().unwrap();
        let migrated = historical.rebind_source(
            &previous, current, panel.surface.clone(), panel.roadside.clone(), panel.ordered.clone(),
        ).unwrap();
        migrated.validate().unwrap();
        assert_eq!(migrated.main_ring, Some(false));
        assert!(migrated.ring_costmap.is_none());
        assert_eq!(migrated.include_trails, Some(false));
        assert_eq!(migrated.access_costmap.as_ref().unwrap().maximum_work, 1_000_000);
        assert!(migrated.access_costmap.as_ref().unwrap().policy.sampling.is_none());
        assert!(historical.rebind_source(
            &previous, current, panel.surface.clone(), panel.roadside.clone(), None,
        ).is_err());
        project.roads = None;
        panel.ordered = None;
        let before = project.clone();
        click(&ctx, &mut panel, &mut project, &pins, "Roads & trails");
        assert_eq!(project, before);
        assert!(panel.message.contains("ordered road inputs"));
    }
    #[test]
    fn ordered_roads_support_earlier_rails_and_reject_late_rails_atomically() {
        let ctx = egui::Context::default();
        let (mut panel, mut project, pins) = staged_fixture();
        click(&ctx, &mut panel, &mut project, &pins, "Roads & trails");
        panel.ordered = Some(mapper_studio::source_road_planning::Policy {
            engine: mapper_studio::source_road_planning::VERSION.into(),
            assets_sha256: "a".repeat(64),
            roles_sha256: "b".repeat(64),
            ring_clearance: None,
            terrain_entrances: None,
        });
        project.rails = Some(mapper_studio::rail_generation::Recipe {
            engine: mapper_studio::rail_generation::BEFORE_ROADS_VERSION.into(),
            source_sha256: "1".repeat(64),
            mesh_sha256: "2".repeat(64),
            settings: mapper_studio::rail_generation::initial_settings(42),
        });
        project.validate().unwrap();
        let mut expected = project.clone();
        expected.roads.as_mut().unwrap().staged = Some(
            mapper_studio::road_generation::Policy::for_new_recipe(
                panel.roadside.clone().unwrap(),
                panel.ordered.clone(),
                false,
            )
            .unwrap(),
        );
        panel.set_staged(&mut project, true).unwrap();
        assert_eq!(project, expected);
        assert_eq!(
            project.roads.as_ref().unwrap().staged.as_ref().unwrap().engine,
            mapper_studio::road_generation::CONNECTED_SOURCE_VERSION
        );
        panel.apply_defaults(&mut project).unwrap();
        expected.roads.as_mut().unwrap().apply_correctness_defaults();
        assert_eq!(project, expected);
        panel.apply_defaults(&mut project).unwrap();
        assert_eq!(project, expected);
        project.rails.as_mut().unwrap().engine = mapper_studio::rail_generation::VERSION.into();
        let saved = project.clone();
        assert!(panel.set_staged(&mut project, true).is_err());
        assert_eq!(project, saved);
    }
    #[test]
    fn road_defaults_reuse_a_validated_recipe_until_inputs_change() {
        let (mut panel, mut project, pins) = fixture();
        let source = panel.source.as_ref().unwrap();
        project.roads = Some(mapper_studio::infrastructure::Recipe::for_source(source));
        panel.refresh_defaults(&mut project).unwrap();
        assert!(project.roads.as_ref().unwrap().access_costmap.is_some());
        let target_buffer = project.targets.as_ptr();
        panel.refresh_defaults(&mut project).unwrap();
        assert_eq!(project.targets.as_ptr(), target_buffer);

        project.roads.as_mut().unwrap().maximum_grade = 0.17;
        panel.refresh_defaults(&mut project).unwrap();
        assert_eq!(project.roads.as_ref().unwrap().maximum_grade, 0.17);
        assert_eq!(panel.defaults_applied.as_ref(), Some(&project));

        let source = panel.source.take().unwrap();
        let catalog = Catalog {
            sha256: pins.catalog_sha256,
            installation: project.installation.clone().unwrap(),
            sources: Vec::new(),
            entries: Default::default(),
        };
        let surface = "d".repeat(64);
        let prepared = mapper_studio::client_preparation::ReadyRoads {
            source,
            previous_source: None,
            surface: surface.clone(),
            roadside: "e".repeat(64),
            joined: panel.joined.clone(),
            ordered: mapper_studio::source_road_planning::Policy {
                engine: mapper_studio::source_road_planning::VERSION.into(),
                assets_sha256: "a".repeat(64),
                roles_sha256: "b".repeat(64),
                ring_clearance: None,
                terrain_entrances: None,
            },
        };
        assert!(project.roads.as_ref().unwrap().surface_sha256.is_none());
        panel.accept_prepared(&project, "test-installation".into(), &catalog, prepared);
        panel.refresh_defaults(&mut project).unwrap();
        assert_eq!(
            project.roads.as_ref().unwrap().surface_sha256.as_deref(),
            Some(surface.as_str())
        );
        assert_eq!(project.roads.as_ref().unwrap().maximum_grade, 0.17);
    }

    #[test]
    fn saved_roads_upgrade_automatically_and_mismatched_sources_are_atomic() {
        let(panel,mut project,_)=fixture();let mut roads=mapper_studio::infrastructure::Recipe::for_source(panel.source.as_ref().unwrap());
        roads.main_ring=Some(false);roads.include_trails=Some(false);roads.maximum_grade=0.17;roads.maximum_cut_fill=6.;project.roads=Some(roads);
        panel.apply_defaults(&mut project).unwrap();let r=project.roads.as_ref().unwrap();
        assert!(r.access_smoothing.is_some());assert!(r.ring_costmap.is_none());assert_eq!(r.maximum_grade,0.17);assert_eq!(r.maximum_cut_fill,6.);assert_eq!(r.include_trails,Some(false));
        project.roads.as_mut().unwrap().source_sha256="f".repeat(64);let before=project.clone();assert!(panel.apply_defaults(&mut project).is_err());assert_eq!(project,before);
    }
    #[test]
    fn correction_switches_are_absent_from_the_road_panel() {
        let ctx=egui::Context::default();let(mut panel,mut project,pins)=fixture();project.roads=Some(mapper_studio::infrastructure::Recipe::for_source(panel.source.as_ref().unwrap()));
        project.roads.as_mut().unwrap().maximum_cut_fill=0.;
        let output=frame(&ctx,&mut panel,&mut project,&pins,vec![]);
        assert_eq!(project.roads.as_ref().unwrap().maximum_cut_fill,0.,"Displaying controls must not raise a saved safety limit");
        for shape in output.shapes {if let egui::Shape::Text(t)=shape.shape {for forbidden in ["Smooth constrained road bends","Source-based access and dirt routing","Use source routing and smooth bends","Refresh roadside topology","Reserve earlier road corridors","Fit and check road entrance surfaces"] {assert_ne!(t.galley.job.text,forbidden);}}}
    }

    #[test]
    fn simplified_road_panel_fits_both_themes() {
        for dark in [false, true] {
            for width in [210, 290] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                workbench_style::install(&ctx);
                ctx.all_styles_mut(|style| style.animation_time = 0.);
                let (mut panel, mut project, pins) = fixture();
                project.roads = Some(mapper_studio::infrastructure::Recipe::for_source(
                    panel.source.as_ref().unwrap(),
                ));
                panel.apply_defaults(&mut project).unwrap();
                project.roads.as_mut().unwrap().maximum_grade = 0.17;
                project.roads.as_mut().unwrap().maximum_cut_fill = 0.;
                let before = project.clone();
                let mut textures = snapshot::Textures::default();
                let size = [width, 420];
                let render = |panel: &mut RoadPanel,
                              project: &mut Project,
                              events,
                              textures: &mut snapshot::Textures| {
                    let mut out = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(width as f32, 420.),
                            )),
                            events,
                            focused: true,
                            ..Default::default()
                        },
                        |ui| {
                            ui.set_max_width(width as f32 - 16.);
                            crate::panels::open_recipe_section(ui, "Roads & trails");
                            panel.show(ui, project, Some(pins.clone()), false);
                        },
                    );
                    textures.update(&out.textures_delta);
                    out.textures_delta.clear();
                    out
                };
                let _ = render(&mut panel, &mut project, vec![], &mut textures);
                let mut out = render(&mut panel, &mut project, vec![], &mut textures);
                for expanded in [false, true] {
                    if expanded {
                        let point = out
                            .shapes
                            .iter()
                            .find_map(|shape| match &shape.shape {
                                egui::Shape::Text(t) if t.galley.job.text == "Advanced routing" => {
                                    Some(t.galley.rect.translate(t.pos.to_vec2()).center())
                                }
                                _ => None,
                            })
                            .expect("Routing disclosure is visible");
                        for pressed in [true, false] {
                            let _ = render(
                                &mut panel,
                                &mut project,
                                vec![
                                    egui::Event::PointerMoved(point),
                                    egui::Event::PointerButton {
                                        pos: point,
                                        button: egui::PointerButton::Primary,
                                        pressed,
                                        modifiers: egui::Modifiers::NONE,
                                    },
                                ],
                                &mut textures,
                            );
                        }
                        out = render(&mut panel, &mut project, vec![], &mut textures);
                    }
                    let mut labels = Vec::new();
                    for shape in &out.shapes {
                        if let egui::Shape::Text(t) = &shape.shape {
                            let rect = t.galley.rect.translate(t.pos.to_vec2());
                            assert!(
                                rect.max.x <= width as f32 && shape.clip_rect.contains_rect(rect),
                                "Clipped text at {width}px: {}",
                                t.galley.job.text
                            );
                            labels.push(t.galley.job.text.as_str());
                        }
                    }
                    for label in ["Grade (%)", "Cut/fill (m)"] {
                        assert_eq!(labels.contains(&label), expanded);
                    }
                    assert_eq!(project, before, "Disclosure must preserve saved settings");
                    if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                        // Rasterization borrows the accumulated atlas after this frame.
                        let image = textures.raster(&ctx, out.clone(), size);
                        mapper_studio::image_export::write_png(
                            &std::path::Path::new(&dir).join(format!(
                                "roads-{}-{width}-{}.png",
                                if dark { "dark" } else { "light" },
                                if expanded { "expanded" } else { "compact" }
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
    #[test]
    fn road_metadata_upgrade_preserves_constraints_and_rejects_another_installation() {
        let(panel,_,_)=fixture();let current=panel.source.as_ref().unwrap();let mut previous:Source=serde_json::from_value(serde_json::to_value(current).unwrap()).unwrap();
        previous.profile.revision-=1;previous.sha256="f".repeat(64);
        let mut old=mapper_studio::infrastructure::Recipe::for_source(&previous);
        old.main_ring=Some(false);old.include_trails=Some(false);old.maximum_grade=0.17;old.maximum_cut_fill=0.;
        let mut costs=panel.joined.clone().unwrap();costs.infrastructure_sha256=previous.sha256.clone();costs.maximum_work=8000;old.access_costmap=Some(costs);
        let next=old.rebind_source(&previous,current,None,None,None).unwrap();
        assert_eq!(next.source_sha256,current.sha256);assert_eq!(next.maximum_grade,0.17);assert_eq!(next.maximum_cut_fill,0.);
        assert_eq!(next.main_ring,Some(false));assert_eq!(next.include_trails,Some(false));assert_eq!(next.access_costmap.as_ref().unwrap().maximum_work,8000);
        assert!(next.access_smoothing.is_some());
        let mut foreign=serde_json::to_value(current).unwrap();foreign["sources"][0]["sha256"]="e".repeat(64).into();
        let mut foreign:Source=serde_json::from_value(foreign).unwrap();foreign.sha256=current.sha256.clone();
        assert!(old.rebind_source(&previous,&foreign,None,None,None).is_err());
    }

}
