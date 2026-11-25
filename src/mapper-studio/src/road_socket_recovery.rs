//! Installation-bound environment for checked socket geometry proposals.
use crate::{
    Project, infrastructure, placement_preview, prefab_assets::Catalog, prefab_textures::Bindings,
    road_paint_source, roadside_source,
};
use papyrus_terrain::{
    monument_composition::Composed,
    placement::Solution,
    road_network::{self, socket_recovery},
};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
pub struct Environment<'a> {
    maps: &'a Composed,
    recipe: infrastructure::Recipe,
    costs: Option<papyrus_terrain::road_costmap::Grid>,
    cost_lattice: Option<papyrus_terrain::road_costmap::CellLattice>,
    obstacles: Vec<road_network::Obstacle>,
    owners: Vec<road_network::Obstacle>,
    settings: road_network::Settings,
    protected_without_water: i32,
    family_settings: std::collections::BTreeMap<u32, road_network::Settings>,
    ownership: road_network::EntrancePolicy,
    launch: road_network::LaunchPolicy,
    height_origin: f32,
    vertical_span: f32,
    water_policy: Option<papyrus_terrain::road_water::Policy>,
    source_water: Option<SourceWater>,
    water: Option<Vec<f32>>,
}
struct SourceWater {
    policy: papyrus_terrain::road_water::Policy,
    terrain_profile_sha256: String,
    ocean_level: f32,
}
#[derive(Debug, serde::Serialize)]
pub struct FamilyRouteAudit {
    pub hierarchy: u32,
    pub water_policy: Option<papyrus_terrain::road_water::Policy>,
    pub water_guide_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_terrain_profile_sha256: Option<String>,
    pub audit: socket_recovery::RoadSetAudit,
}
#[derive(Debug, serde::Serialize)]
pub struct AllRoutesAudit {
    pub engine: &'static str,
    pub roads: usize,
    pub families: Vec<FamilyRouteAudit>,
    pub work: usize,
    pub complete_final_routes_checked: bool,
    pub runtime_validated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constructed_ground_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constructed_ocean_sha256: Option<String>,
    #[serde(skip)]
    pub(crate) constructed_ocean: Option<papyrus_terrain::ocean::Mask>,
}
#[derive(Debug, serde::Serialize)]
pub struct AllRoutesFailure {
    pub work: usize,
    pub hierarchy: Option<u32>,
    pub reason: &'static str,
    pub detail: Option<Box<socket_recovery::RouteFailure>>,
}
impl Environment<'_> {
    fn family_water_policy(
        &self,
        hierarchy: u32,
    ) -> Result<Option<&papyrus_terrain::road_water::Policy>, &'static str> {
        if hierarchy > 2 {
            return Err("Unsupported route water hierarchy");
        }
        if self
            .recipe
            .staged
            .as_ref()
            .is_some_and(|p| crate::road_generation::uses_source_cost_guidance(&p.engine))
        {
            return self
                .source_water
                .as_ref()
                .map(|s| Some(&s.policy))
                .ok_or("Missing source terrain water policy");
        }
        Ok(match hierarchy {
            0 => self
                .recipe
                .ring_costmap
                .as_ref()
                .and_then(|p| p.joins.as_ref())
                .and_then(|j| j.river_water.as_ref()),
            1 => self.water_policy.as_ref(),
            _ => None,
        })
    }
    pub(crate) fn propose(
        &self,
        plan: &crate::road_planning::Plan,
        key: socket_recovery::Key,
        degrees: f64,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<socket_recovery::Proposal>, socket_recovery::Failure> {
        let report = plan.planning_report();
        if self
            .recipe
            .staged
            .as_ref()
            .is_some_and(|p| crate::road_generation::uses_source_cost_guidance(&p.engine))
        {
            let socket = road_network::Destination::Monument {
                monument: key.monument,
                connector: key.connector,
            };
            let mut family = None;
            for (i, road) in report.roads.iter().enumerate() {
                if road.from != socket && road.to != socket {
                    continue;
                }
                let mut matches = report
                    .families
                    .iter()
                    .filter(|f| i >= f.first_road && i - f.first_road < f.road_count);
                let current = matches.next().ok_or(socket_recovery::Failure::Invalid(
                    "Socket has no retained source family",
                ))?;
                if matches.next().is_some()
                    || family.is_some_and(|f| f != (current.hierarchy, current.seed))
                {
                    return Err(socket_recovery::Failure::Invalid(
                        "Socket spans ambiguous source families",
                    ));
                }
                family = Some((current.hierarchy, current.seed));
            }
            let (hierarchy, seed) = family.ok_or(socket_recovery::Failure::Invalid(
                "Socket has no source roads",
            ))?;
            let settings =
                *self
                    .family_settings
                    .get(&hierarchy)
                    .ok_or(socket_recovery::Failure::Invalid(
                        "Missing source socket family shape",
                    ))?;
            let (lattice, costs) = plan
                .source_cost_grid(hierarchy, seed)
                .map_err(|_| {
                    socket_recovery::Failure::Invalid("Source socket grid binding differs")
                })?
                .ok_or(socket_recovery::Failure::Invalid(
                    "Missing retained source socket grid",
                ))?;
            let policy = self
                .family_water_policy(hierarchy)
                .map_err(socket_recovery::Failure::Invalid)?;
            let guide = policy
                .zip(self.water.as_deref())
                .map(|(policy, heights)| papyrus_terrain::road_water::Guide { policy, heights });
            let mut input = self.input_with_source_policy(Some(costs), guide);
            input.cost_lattice = Some(lattice);
            input.settings = road_network::Settings {
                seed,
                protected_topology: settings.protected_topology
                    & !guide.map_or(0, |g| g.policy.topology),
                ..settings
            };
            socket_recovery::propose_source(report, &input, key, degrees, maximum_work, cancelled)
        } else {
            socket_recovery::propose(report, &self.input(), key, degrees, maximum_work, cancelled)
        }
    }
    /// Check every final source family against the pristine pre-road revision.
    /// Grids retain their captured family binding. Historical water permissions
    /// come from the recipe; source-2 uses its verified whole-road terrain pass.
    pub fn validate_all_routes(
        &self,
        report: &road_network::Report,
        points: &[Vec<[f32; 3]>],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AllRoutesAudit>, AllRoutesFailure> {
        self.validate_all_routes_impl(report, points, None, None, maximum_work, cancelled)
    }
    pub(crate) fn validate_all_routes_from_plan(
        &self,
        plan: &crate::road_planning::Plan,
        points: &[Vec<[f32; 3]>],
        constructed: &[i16],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AllRoutesAudit>, AllRoutesFailure> {
        self.validate_all_routes_impl(
            plan.planning_report(),
            points,
            Some(plan),
            self.recipe
                .staged
                .as_ref()
                .is_some_and(|p| crate::road_generation::uses_source_cost_guidance(&p.engine))
                .then_some(constructed),
            maximum_work,
            cancelled,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn validate_all_routes_impl(
        &self,
        report: &road_network::Report,
        points: &[Vec<[f32; 3]>],
        plan: Option<&crate::road_planning::Plan>,
        constructed: Option<&[i16]>,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AllRoutesAudit>, AllRoutesFailure> {
        let mut work = 0usize;
        let fail = |work, hierarchy, reason| AllRoutesFailure {
            work,
            hierarchy,
            reason,
            detail: None,
        };
        if cancelled() {
            return Ok(None);
        }
        if maximum_work == 0
            || maximum_work > 384_000_000
            || points.len() > 256
            || points.len() != report.roads.len()
            || report.families.len() > 256
            || (report.entrance_launch != Some(self.launch)
                && !(points.is_empty() && report.entrance_launch.is_none()))
        {
            return Err(fail(work, None, "Invalid complete route inputs"));
        }
        if report.source_layout.is_some() && plan.is_none() {
            return Err(fail(
                work,
                None,
                "Ordered routes require their retained source plan",
            ));
        }
        // Reserve receipt work before validation, as before, so failed
        // attempts retain their work charges and retry allowances.
        if let Some(height) = constructed {
            if height.len() != self.maps.layers.height.len() {
                return Err(fail(
                    work,
                    None,
                    "Constructed route ground dimensions differ",
                ));
            }
            work = height
                .len()
                .checked_mul(4)
                .filter(|&charge| charge <= maximum_work)
                .ok_or_else(|| fail(maximum_work, None, "Constructed ground binding work exhausted"))?;
        }
        let source_surface = self
            .recipe
            .staged
            .as_ref()
            .is_some_and(|p| crate::road_generation::is_source_engine(&p.engine));
        if source_surface && (plan.is_none() || report.source_layout.is_none()) {
            return Err(fail(
                work,
                None,
                "Source road surface checks require their retained ordered plan",
            ));
        }
        // Match finish_surfaces' classification of these exact final heights.
        // Only a map which already carries an ocean stage may reclassify it.
        let constructed_ocean =
            if let Some(height) = constructed.filter(|_| self.maps.ocean.is_some()) {
                let n = self.maps.layers.surface_resolution;
                work = work
                    .checked_add(height.len() + n * n * 8)
                    .filter(|v| *v <= maximum_work)
                    .ok_or_else(|| {
                        fail(
                            maximum_work,
                            None,
                            "Constructed ocean preparation work exhausted",
                        )
                    })?;
                match papyrus_terrain::ocean::classify_height(
                    height,
                    self.maps.layers.height_resolution,
                    n,
                    &cancelled,
                )
                .map_err(|_| fail(work, None, "Constructed ocean classification failed"))?
                {
                    Some(mask) => Some(mask),
                    None => return Ok(None),
                }
            } else {
                None
            };
        if let Some(mask) = &constructed_ocean {
            work = work
                .checked_add(mask.classification.len() * 2)
                .filter(|&total| total <= maximum_work)
                .ok_or_else(|| fail(maximum_work, None, "Constructed ocean binding work exhausted"))?;
        }
        let mut coverage = vec![false; points.len()];
        let mut ranges = Vec::with_capacity(report.families.len());
        let mut families = Vec::new();
        for family in &report.families {
            if cancelled() {
                return Ok(None);
            }
            let end = family
                .first_road
                .checked_add(family.road_count)
                .filter(|v| *v <= points.len())
                .ok_or_else(|| {
                    fail(
                        work,
                        Some(family.hierarchy),
                        "Invalid complete route family range",
                    )
                })?;
            let selected = (family.first_road..end).collect::<Vec<_>>();
            for &i in &selected {
                work = work
                    .checked_add(1)
                    .filter(|v| *v <= maximum_work)
                    .ok_or_else(|| {
                        fail(
                            maximum_work,
                            Some(family.hierarchy),
                            "Complete route work exhausted",
                        )
                    })?;
                if std::mem::replace(&mut coverage[i], true) {
                    return Err(fail(
                        work,
                        Some(family.hierarchy),
                        "Overlapping complete route families",
                    ));
                }
            }
            ranges.push(selected);
        }
        if coverage.iter().any(|v| !*v) {
            return Err(fail(work, None, "Incomplete source route coverage"));
        }
        const DIRT_FEEDBACK_WORK: usize = 32_000_000;
        let mut primary_failure: Option<(u32, Box<socket_recovery::RouteFailure>)> = None;
        let scanned = (|| -> Result<Option<()>, AllRoutesFailure> {
            for (family, selected) in report.families.iter().zip(ranges) {
                if cancelled() {
                    return Ok(None);
                }
                if selected.is_empty() || (primary_failure.is_some() && family.hierarchy != 2) {
                    continue;
                }
                let settings = *self.family_settings.get(&family.hierarchy).ok_or_else(|| {
                    fail(
                        work,
                        Some(family.hierarchy),
                        "Missing source route family shape",
                    )
                })?;
                if family.width != settings.width || family.splat_channel != settings.splat_channel
                {
                    return Err(fail(
                        work,
                        Some(family.hierarchy),
                        "Route family differs from its source shape",
                    ));
                }
                let enabled = if family.hierarchy == 0 {
                    self.recipe.ring_costmap.is_some()
                } else {
                    self.recipe.access_costmap.is_some()
                };
                let source_grid = if enabled {
                    match plan {
                        Some(p) => {
                            p.source_cost_grid(family.hierarchy, family.seed)
                                .map_err(|_| {
                                    fail(
                                        work,
                                        Some(family.hierarchy),
                                        "Retained source grid binding differs",
                                    )
                                })?
                        }
                        None => None,
                    }
                } else {
                    None
                };
                let grid = if !enabled || source_grid.is_some() {
                    None
                } else if family.hierarchy == 0 {
                    Some(
                        &report
                            .ring_costmap
                            .as_ref()
                            .ok_or_else(|| fail(work, Some(0), "Missing source main-road grid"))?["grid"],
                    )
                } else {
                    let label = match family.hierarchy {
                        1 => "paved",
                        2 => "dirt",
                        _ => {
                            return Err(fail(
                                work,
                                Some(family.hierarchy),
                                "Unsupported route hierarchy",
                            ));
                        }
                    };
                    let matching = report
                        .source_access_costmaps
                        .iter()
                        .filter(|v| v["family"] == label)
                        .collect::<Vec<_>>();
                    if matching.len() != 1 {
                        return Err(fail(
                            work,
                            Some(family.hierarchy),
                            "Missing or ambiguous source access grid",
                        ));
                    }
                    Some(&matching[0]["grid"])
                };
                let (costs, cost_lattice) = if let Some((lattice, values)) = source_grid {
                    work = work
                        .checked_add(values.len())
                        .filter(|v| *v <= maximum_work)
                        .ok_or_else(|| {
                            fail(
                                maximum_work,
                                Some(family.hierarchy),
                                "Source grid validation work exhausted",
                            )
                        })?;
                    (Some(Cow::Borrowed(values)), Some(lattice))
                } else {
                    let costs = if let Some(grid) = grid {
                        if grid["engine"] != papyrus_terrain::road_costmap::VERSION
                            || grid["resolution"] != 257
                            || grid["seed"] != family.seed
                            || grid["trail"] != (family.hierarchy == 2)
                        {
                            return Err(fail(
                                work,
                                Some(family.hierarchy),
                                "Source route grid binding differs",
                            ));
                        }
                        let raw = grid["costs"]
                            .as_array()
                            .filter(|v| v.len() == 257 * 257)
                            .ok_or_else(|| {
                                fail(
                                    work,
                                    Some(family.hierarchy),
                                    "Invalid source route grid dimensions",
                                )
                            })?;
                        let mut values = Vec::with_capacity(raw.len());
                        for (i, value) in raw.iter().enumerate() {
                            if i.is_multiple_of(4096) && cancelled() {
                                return Ok(None);
                            }
                            work = work
                                .checked_add(1)
                                .filter(|v| *v <= maximum_work)
                                .ok_or_else(|| {
                                    fail(
                                        maximum_work,
                                        Some(family.hierarchy),
                                        "Complete route grid work exhausted",
                                    )
                                })?;
                            values.push(value.as_u64().filter(|v| *v <= 1_000_000).ok_or_else(
                                || {
                                    fail(
                                        work,
                                        Some(family.hierarchy),
                                        "Invalid source route cost cell",
                                    )
                                },
                            )? as u32);
                        }
                        Some(values)
                    } else {
                        None
                    };
                    (costs.map(Cow::Owned), None)
                };
                let policy = self
                    .family_water_policy(family.hierarchy)
                    .map_err(|reason| fail(work, Some(family.hierarchy), reason))?;
                let guide = policy.zip(self.water.as_deref()).map(|(policy, heights)| {
                    papyrus_terrain::road_water::Guide { policy, heights }
                });
                let mut input = self.input_with_source_policy(costs.as_deref(), guide);
                input.cost_lattice = cost_lattice;
                input.settings = road_network::Settings {
                    seed: family.seed,
                    protected_topology: settings.protected_topology
                        & !guide.map_or(0, |g| g.policy.topology),
                    ..settings
                };
                let family_work_limit = if primary_failure.is_some() {
                    DIRT_FEEDBACK_WORK
                } else {
                    128_000_000
                };
                let allowance = maximum_work.saturating_sub(work).min(family_work_limit);
                if allowance == 0 {
                    return Err(fail(
                        work,
                        Some(family.hierarchy),
                        "No remaining complete route allowance",
                    ));
                }
                let checked =
                    if let Some(height) = constructed {
                        socket_recovery::validate_source_road_set_on_ground(
                            report,
                            &input,
                            &selected,
                            points,
                            socket_recovery::ConstructedGround {
                                height,
                                ocean: constructed_ocean.as_ref(),
                                minimum_dry_height: self
                                    .source_water
                                    .as_ref()
                                    .ok_or_else(|| {
                                        fail(
                                            work,
                                            Some(family.hierarchy),
                                            "Missing constructed source water level",
                                        )
                                    })?
                                    .ocean_level,
                            },
                            allowance,
                            &cancelled,
                        )
                    } else if self.recipe.staged.as_ref().is_some_and(|p| {
                        crate::road_generation::uses_source_cost_guidance(&p.engine)
                    }) {
                        socket_recovery::validate_source_road_set(
                            report, &input, &selected, points, allowance, &cancelled,
                        )
                    } else if source_surface {
                        socket_recovery::validate_road_surface_set(
                            report, &input, &selected, points, allowance, &cancelled,
                        )
                    } else {
                        socket_recovery::validate_road_set(
                            report, &input, &selected, points, allowance, &cancelled,
                        )
                    };
                if let Some((_, primary)) = primary_failure.as_mut() {
                    match checked {
                        Ok(Some(audit)) => work += audit.work,
                        Ok(None) => return Ok(None),
                        Err(detail) => {
                            work += detail.work;
                            if matches!(&detail.error, socket_recovery::Failure::GroundBlocked {
                                ground: Some(ground), obstacle: None, ..
                            } if matches!(ground.reason, "water or low ground" | "unowned protected ground"))
                            {
                                primary.additional_blocked_ground.push(detail.error);
                            }
                            primary
                                .additional_blocked_ground
                                .extend(detail.additional_blocked_ground);
                        }
                    }
                    break;
                }
                match checked {
                    Ok(Some(audit)) => {
                        work = work
                            .checked_add(audit.work)
                            .filter(|v| *v <= maximum_work)
                            .ok_or_else(|| {
                                fail(
                                    maximum_work,
                                    Some(family.hierarchy),
                                    "Complete route work overflow",
                                )
                            })?;
                        families.push(FamilyRouteAudit {
                            hierarchy: family.hierarchy,
                            water_policy: policy.cloned(),
                            water_guide_available: guide.is_some(),
                            source_terrain_profile_sha256: self
                                .source_water
                                .as_ref()
                                .map(|s| s.terrain_profile_sha256.clone()),
                            audit,
                        });
                    }
                    Ok(None) => return Ok(None),
                    Err(detail) => {
                        if constructed.is_some()
                            && family.hierarchy < 2
                            && matches!(
                                &detail.error,
                                socket_recovery::Failure::GroundBlocked {
                                    obstacle: Some(_), ..
                                } | socket_recovery::Failure::GroundBlocked {
                                    ground: Some(road_network::GroundBlock {
                                        reason: "water or low ground" | "terrain hole" | "unowned protected ground",
                                        ..
                                    }),
                                    ..
                                }
                            )
                        {
                            // Keep the primary paved failure and use the remaining
                            // allowance to find blocked terrain in the dirt family.
                            work += detail.work;
                            primary_failure = Some((family.hierarchy, detail));
                            continue;
                        }
                        return Err(AllRoutesFailure {
                            work: work.saturating_add(detail.work).min(maximum_work),
                            hierarchy: Some(family.hierarchy),
                            reason: "Final source route footprint rejected",
                            detail: Some(detail),
                        });
                    }
                }
            }
            Ok(Some(()))
        })();
        if let Some((hierarchy, detail)) = primary_failure {
            if matches!(&scanned, Ok(None)) {
                return Ok(None);
            }
            return Err(AllRoutesFailure {
                work: scanned.as_ref().err().map_or(work, |error| error.work),
                hierarchy: Some(hierarchy),
                reason: "Final source route footprint rejected",
                detail: Some(detail),
            });
        }
        if scanned?.is_none() {
            return Ok(None);
        }
        if coverage.iter().any(|v| !*v) {
            return Err(fail(work, None, "Incomplete source route coverage"));
        }
        let constructed_ground_sha256 = if let Some(height) = constructed {
            let mut hash = Sha256::new();
            if crate::road_planning::hash_raster_words(
                &mut hash,
                height,
                |value| value.to_le_bytes(),
                &cancelled,
            )
            .is_none()
            {
                return Ok(None);
            }
            Some(format!("{:x}", hash.finalize()))
        } else {
            None
        };
        let constructed_ocean_sha256 = if let Some(mask) = &constructed_ocean {
            let mut hash = Sha256::new();
            for chunk in mask.classification.chunks(4096) {
                if cancelled() {
                    return Ok(None);
                }
                hash.update(chunk);
            }
            Some(format!("{:x}", hash.finalize()))
        } else {
            None
        };
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(AllRoutesAudit {
            engine: if constructed_ocean.is_some() {
                "papyrus-complete-road-route-gate-5"
            } else if constructed.is_some() {
                "papyrus-complete-road-route-gate-4"
            } else if self.source_water.is_some() {
                "papyrus-complete-road-route-gate-3"
            } else if source_surface {
                "papyrus-complete-road-route-gate-2"
            } else {
                "papyrus-complete-road-route-gate-1"
            },
            roads: points.len(),
            families,
            work,
            complete_final_routes_checked: true,
            runtime_validated: false,
            constructed_ground_sha256,
            constructed_ocean_sha256,
            constructed_ocean,
        }))
    }
    pub fn source_family_settings(
        &self,
    ) -> &std::collections::BTreeMap<u32, road_network::Settings> {
        &self.family_settings
    }
    pub(crate) fn unconditionally_protected_topology(&self, hierarchy: u32) -> i32 {
        let Some(settings) = self.family_settings.get(&hierarchy) else {
            return 0;
        };
        let water = self.family_water_policy(hierarchy).ok().flatten().map_or(0, |p| p.topology);
        settings.protected_topology & !water & !self.ownership.owned_topology
    }
    /// Apply the exact routing family's source costs and explicit river guide.
    /// Reconstruct protection from the source roles, never another family's
    /// water exception. The caller binds these policies to the saved family.
    pub fn input_with_source_policy<'a>(
        &'a self,
        costs: Option<&'a [u32]>,
        water: Option<papyrus_terrain::road_water::Guide<'a>>,
    ) -> socket_recovery::Environment<'a> {
        let mut input = self.input();
        input.costs = costs;
        input.water = water;
        input.settings.protected_topology =
            self.protected_without_water & !water.map_or(0, |w| w.policy.topology);
        input
    }
    pub fn ownership_footprints(&self) -> &[road_network::Obstacle] {
        &self.owners
    }
    pub fn input(&self) -> socket_recovery::Environment<'_> {
        let maps = self.maps;
        socket_recovery::Environment {
            layers: &maps.layers,
            alpha: &maps.alpha,
            obstacles: &self.obstacles,
            owners: &self.owners,
            settings: self.settings,
            ownership: self.ownership,
            launch: self.launch,
            costs: self.costs.as_ref().map(|g| g.costs.as_slice()),
            cost_lattice: self.cost_lattice,
            height_origin: self.height_origin,
            vertical_span: self.vertical_span,
            water: self
                .water_policy
                .as_ref()
                .zip(self.water.as_deref())
                .map(|(policy, heights)| papyrus_terrain::road_water::Guide { policy, heights }),
        }
    }
}
/// Uses the same verified metadata and ownership transforms as road planning.
/// The supplied map revision must be pre-road, so retries never retain paint.
pub fn prepare<'a>(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &'a Composed,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Environment<'a>>, String> {
    prepare_inner(
        project, solution, catalog, bindings, maps, None, true, cancelled,
    )
}
pub fn prepare_retaining_rails<'a>(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &'a Composed,
    rails: &crate::retained_rail_scene::Receipt,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Environment<'a>>, String> {
    prepare_inner(
        project,
        solution,
        catalog,
        bindings,
        maps,
        Some(rails),
        true,
        cancelled,
    )
}
pub(crate) fn prepare_for_plan<'a>(
    context: crate::road_generation::Context<'_>,
    maps: &'a Composed,
    rails: Option<&crate::retained_rail_scene::Receipt>,
    plan: &crate::road_planning::Plan,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Environment<'a>>, String> {
    // The source-guided proposal and acceptance checks use each family's
    // retained pre-guidance grid from the plan. A second paved costmap cannot
    // represent that grid in the presence of ring or rail reservations.
    let retained_source_grids = plan.planning_report().source_layout.is_some()
        && context
            .project
            .roads
            .as_ref()
            .and_then(|r| r.staged.as_ref())
            .is_some_and(|p| crate::road_generation::uses_source_cost_guidance(&p.engine));
    prepare_inner(
        context.project,
        context.solution,
        context.catalog,
        context.bindings,
        maps,
        rails,
        !retained_source_grids,
        cancelled,
    )
}
fn check_pre_road_maps(
    project: &Project,
    maps: &Composed,
    rails: Option<&crate::retained_rail_scene::Receipt>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<()>, String> {
    if cancelled() {
        return Ok(None);
    }
    if maps.layers.size != project.size
        || maps.layers.terrain_seed != project.seeds[0].value
        || maps.layers.biome_seed != project.seeds[1].value
        || maps.roads.is_some()
        || (maps.rails.is_some() && rails.is_none())
        || maps.objects.is_some()
        || maps.decoration.is_some()
        || maps.gameplay_height.is_some()
        || maps.spawn_audit.is_some()
        || maps.topology_before_overrides.is_some()
        || maps.topology_overrides.is_some()
    {
        return Err("Socket recovery requires pristine pre-road maps".into());
    }
    if let Some(receipt) = rails {
        let scene = maps
            .rails
            .as_ref()
            .ok_or("Socket recovery requires the retained rail scene")?;
        let recipe = project
            .rails
            .as_ref()
            .ok_or("Socket recovery requires the retained rail recipe")?;
        if receipt
            .check(scene, recipe, maps.layers.surface_resolution, cancelled)?
            .is_none()
        {
            return Ok(None);
        }
    }
    Ok(Some(()))
}
#[allow(clippy::too_many_arguments)]
fn prepare_inner<'a>(
    project: &Project,
    solution: &Solution,
    catalog: &Catalog,
    bindings: &Bindings,
    maps: &'a Composed,
    rails: Option<&crate::retained_rail_scene::Receipt>,
    build_access_costs: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Environment<'a>>, String> {
    if cancelled() {
        return Ok(None);
    }
    project.validate()?;
    if check_pre_road_maps(project, maps, rails, &cancelled)?.is_none() {
        return Ok(None);
    }
    let recipe = project.roads.as_ref().ok_or("Missing socket road recipe")?;
    let launch = recipe
        .entrance_launch
        .filter(|p| p.engine == road_network::LaunchEngine::GlobalEarthwork)
        .ok_or("Socket recovery requires launch revision 8")?;
    let source = infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?;
    let road_side = roadside_source::Source::cached(
        &recipe
            .staged
            .as_ref()
            .ok_or("Socket recovery requires staged roads")?
            .roadside_source_sha256,
        bindings.installation_root(),
        catalog,
        &cancelled,
    )?;
    let paint = road_paint_source::Profile::for_source(&road_side)?
        .ok_or("Missing socket height profile")?;
    let (costs, cost_lattice) = if build_access_costs && let Some(profile) = &recipe.access_costmap
    {
        let Some(blockers) =
            profile.prepare_blockers(project, solution, catalog, bindings, &source, &cancelled)?
        else {
            return Ok(None);
        };
        let Some(blockers) = crate::road_cost_source::with_retained_placement(
            project, maps, rails, blockers, &cancelled,
        )?
        else {
            return Ok(None);
        };
        if profile.policy.sampling.is_some() {
            let Some(costs) = profile.build_cell_family(
                &source,
                &maps.layers,
                &blockers,
                project.seeds[3].value,
                false,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            (Some(costs.grid), Some(costs.lattice))
        } else {
            let Some(costs) = profile.build_family(
                &maps.layers,
                &blockers,
                project.seeds[3].value,
                false,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            (Some(costs), None)
        }
    } else {
        (None, None)
    };
    let mut obstacles = Vec::new();
    let mut owners = Vec::new();
    for outcome in &solution.outcomes {
        if cancelled() {
            return Ok(None);
        }
        let Some(position) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Unknown socket layout target")?;
        let asset = catalog
            .entries
            .get(
                target
                    .prefab_path
                    .as_ref()
                    .ok_or("Missing socket target prefab")?,
            )
            .ok_or("Missing socket prefab metadata")?;
        let bounds = asset
            .bounds
            .as_ref()
            .ok_or("Socket recovery requires complete obstacle bounds")?;
        obstacles.push(road_network::Obstacle {
            monument: outcome.id,
            center: [position.x, position.z],
            extent: bounds.horizontal_extent(),
            yaw: position.yaw,
        });
        let ([_, bounds], _) = placement_preview::bounds_for_recipe(project, asset)
            .map_err(|e| format!("Socket owner footprint: {e}"))?;
        let (s, c) = position.yaw.to_radians().sin_cos();
        owners.push(road_network::Obstacle {
            monument: outcome.id,
            center: [
                position.x + c * bounds.center[0] - s * bounds.center[1],
                position.z + s * bounds.center[0] + c * bounds.center[1],
            ],
            extent: bounds.extent,
            yaw: position.yaw,
        });
    }
    let source_terrain = if recipe
        .staged
        .as_ref()
        .is_some_and(|p| crate::road_generation::uses_source_cost_guidance(&p.engine))
    {
        Some(
            crate::road_terrain_source::Profile::for_paint(&paint)?
                .ok_or("Missing source terrain water profile")?,
        )
    } else {
        None
    };
    let source_water = source_terrain
        .as_ref()
        .map(|terrain| -> Result<SourceWater, String> {
            Ok(SourceWater {
                policy: terrain.water_policy()?,
                terrain_profile_sha256: terrain.sha256()?,
                ocean_level: terrain.water_no_hit_level,
            })
        })
        .transpose()?;
    let water_policy = recipe.access_river_water.clone();
    let ring_water = recipe
        .ring_costmap
        .as_ref()
        .and_then(|p| p.joins.as_ref())
        .and_then(|j| j.river_water.as_ref());
    let water = if source_water.is_some() || water_policy.is_some() || ring_water.is_some() {
        if let Some(rivers) = &maps.rivers {
            let Some(levels) = rivers.collider_heights(
                project.size,
                maps.layers.surface_resolution,
                &cancelled,
            )?
            else {
                return Ok(None);
            };
            Some(levels.heights)
        } else if project.rivers.is_some() {
            return Err("Socket recovery lost generated river world".into());
        } else {
            source_terrain.as_ref().map(|terrain| {
                vec![terrain.water_no_hit_level; maps.layers.surface_resolution.pow(2)]
            })
        }
    } else {
        None
    };
    let p = &source.profile;
    let traversal = p
        .access_routing
        .ok_or("Socket recovery requires source traversal policy")?;
    traversal.validate()?;
    let ownership = p.entrances.ok_or("Missing socket ownership policy")?;
    if traversal.traversable_topology & ownership.owned_topology != 0 {
        return Err("Socket traversal cannot override monument ownership".into());
    }
    // Match build_launched_access: traversable roles are priced, not blocked.
    // Explicit river support receives the same role exception as the source
    // launcher, and is never inferred from the map's encoded water height.
    let water_flags = if water.is_some() {
        water_policy.as_ref().map_or(0, |w| w.topology)
    } else {
        0
    };
    let protected_topology = p.protected_topology & !traversal.traversable_topology & !water_flags;
    let settings_without_water = road_network::Settings {
        seed: project.seeds[3].value,
        width: p.width,
        shoulder: p.shoulder,
        maximum_grade: recipe.maximum_grade,
        maximum_cut_fill: recipe.maximum_cut_fill,
        terrain_offset: p.terrain_offset,
        road_topology: p.road_topology,
        roadside_topology: p.roadside_topology,
        protected_topology: p.protected_topology & !traversal.traversable_topology,
        splat_channel: p.splat_channel,
    };
    let mut family_settings = std::collections::BTreeMap::from([(1, settings_without_water)]);
    if let Some(ring) = p.ring {
        family_settings.insert(
            ring.hierarchy,
            road_network::Settings {
                width: ring.width,
                ..settings_without_water
            },
        );
    }
    if let Some(trail) = &p.trails {
        family_settings.insert(
            trail.hierarchy,
            road_network::Settings {
                width: trail.width,
                shoulder: trail.shoulder,
                terrain_offset: trail.terrain_offset,
                splat_channel: trail.splat_channel,
                ..settings_without_water
            },
        );
    }
    Ok(Some(Environment {
        maps,
        recipe: recipe.clone(),
        costs,
        cost_lattice,
        obstacles,
        owners,
        settings: road_network::Settings {
            seed: project.seeds[3].value,
            width: p.width,
            shoulder: p.shoulder,
            maximum_grade: recipe.maximum_grade,
            maximum_cut_fill: recipe.maximum_cut_fill,
            terrain_offset: p.terrain_offset,
            road_topology: p.road_topology,
            roadside_topology: p.roadside_topology,
            protected_topology,
            splat_channel: p.splat_channel,
        },
        ownership,
        protected_without_water: p.protected_topology & !traversal.traversable_topology,
        family_settings,
        launch,
        height_origin: paint.height_origin,
        vertical_span: paint.vertical_span,
        water_policy,
        source_water,
        water,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::{
        erosion::decode,
        native_layers::Layers,
        road_network::{Destination, Family, Road},
    };
    #[test]
    fn socket_environment_preserves_cell_centres_for_native_costs() {
        let fixture = Fixture::new();
        let mut environment = fixture.env();
        assert!(environment.input().cost_lattice.is_none());
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 7.5).unwrap();
        let cells = lattice.resolution() * lattice.resolution();
        environment.costs = Some(papyrus_terrain::road_costmap::Grid {
            engine: papyrus_terrain::road_costmap::CELL_VERSION,
            resolution: lattice.resolution(),
            seed: 7,
            trail: false,
            counts: Default::default(),
            work: cells,
            costs: vec![101; cells],
        });
        environment.cost_lattice = Some(lattice);
        let input = environment.input();
        assert_eq!(input.cost_lattice, Some(lattice));
        assert_eq!(input.costs.unwrap().len(), cells);
        assert_eq!(
            input.cost_lattice.unwrap().cell([0., 0.]).unwrap(),
            [66, 66]
        );
    }
    #[test]
    fn pre_road_admission_requires_the_exact_retained_scene_and_recipe() {
        let mut fixture = Fixture::new();
        let maps = &mut fixture.maps;
        let n = maps.layers.surface_resolution;
        let (recipe, scene) = crate::retained_rail_scene::tests::fixture(n);
        let receipt = crate::retained_rail_scene::Receipt::capture(&scene, &recipe, n, || false)
            .unwrap()
            .unwrap();
        let mut project = Project {
            size: maps.layers.size,
            rails: Some(recipe),
            ..Default::default()
        };
        project.seeds[0].value = maps.layers.terrain_seed;
        project.seeds[1].value = maps.layers.biome_seed;
        assert_eq!(
            check_pre_road_maps(&project, maps, None, &|| false).unwrap(),
            Some(())
        );
        assert!(check_pre_road_maps(&project, maps, Some(&receipt), &|| false).is_err());
        maps.rails = Some(scene);
        assert!(check_pre_road_maps(&project, maps, None, &|| false).is_err());
        assert_eq!(
            check_pre_road_maps(&project, maps, Some(&receipt), &|| false).unwrap(),
            Some(())
        );
        maps.rails.as_mut().unwrap().reservations.raster[0] = 1;
        assert!(check_pre_road_maps(&project, maps, Some(&receipt), &|| false).is_err());
        maps.rails.as_mut().unwrap().reservations.raster[0] = 0;
        project.rails.as_mut().unwrap().settings["ring"]["seed"] = 42.into();
        assert!(check_pre_road_maps(&project, maps, Some(&receipt), &|| false).is_err());
        assert_eq!(
            check_pre_road_maps(&project, maps, Some(&receipt), &|| true).unwrap(),
            None
        );
        project.rails = None;
        assert!(check_pre_road_maps(&project, maps, Some(&receipt), &|| false).is_err());
    }
    struct Fixture {
        maps: Composed,
        recipe: infrastructure::Recipe,
        report: road_network::Report,
        settings: road_network::Settings,
    }
    impl Fixture {
        fn new() -> Self {
            let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
            let h = ((20_f32 + 500.) / 1000. * 32766.).round() as i16;
            let y = decode(h);
            let layers = Layers {
                size: 1000,
                terrain_seed: 1,
                biome_seed: 2,
                height_resolution: hr,
                surface_resolution: sr,
                height: vec![h; hr * hr],
                topology: vec![0; sr * sr],
                splat: vec![0; sr * sr * 8],
                biome: vec![0; sr * sr * 5],
                normalized_biome_cells: 0,
                normalized_splat_cells: 0,
            };
            let maps = papyrus_terrain::monument_composition::compose(
                &layers,
                &vec![255; sr * sr],
                &[],
                || false,
            )
            .unwrap()
            .unwrap();
            let mut circle = (0..12)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / 12.;
                    [200. * a.cos(), y, 200. * a.sin()]
                })
                .collect::<Vec<_>>();
            circle.push(circle[0]);
            let paved = vec![circle[0], [0., y, 0.], circle[6]];
            let dirt = vec![circle[3], [0., y, 100.], paved[1]];
            let net = |road, point| Destination::Network { road, point };
            let launch = road_network::LaunchPolicy {
                engine: road_network::LaunchEngine::GlobalEarthwork,
                minimum_run_widths: 1.,
                sample_spacing: 1.,
                maximum_work: 2_000_000,
            };
            let mut report = road_network::Report {
                entrance_launch: Some(launch),
                roads: vec![
                    Road {
                        from: Destination::Loop,
                        to: Destination::Loop,
                        points: circle,
                    },
                    Road {
                        from: net(0, 0),
                        to: net(0, 6),
                        points: paved,
                    },
                    Road {
                        from: net(0, 3),
                        to: net(1, 1),
                        points: dirt,
                    },
                ],
                ..Default::default()
            };
            for h in 0..3 {
                report.families.push(Family {
                    infrastructure_type: 0,
                    hierarchy: h,
                    seed: 7,
                    first_road: h as usize,
                    road_count: 1,
                    socket_count: 0,
                    unresolved: vec![],
                    width: [12., 10., 4.][h as usize],
                    splat_channel: if h == 2 { 0 } else { 7 },
                });
            }
            let settings = road_network::Settings {
                seed: 7,
                width: 10.,
                shoulder: 2.,
                maximum_grade: 0.2,
                maximum_cut_fill: 8.,
                terrain_offset: -0.125,
                road_topology: 2048,
                roadside_topology: 4096,
                protected_topology: 1024 | 16384,
                splat_channel: 7,
            };
            let mut recipe = infrastructure::Recipe::new("a".repeat(64));
            recipe.entrance_launch = Some(launch);
            Self {
                maps,
                recipe,
                report,
                settings,
            }
        }
        fn costs(&mut self) {
            let profile: crate::road_cost_source::Profile = serde_json::from_str(include_str!(
                "../../../compatibility/infrastructure/client-0f703d43-road-rivers.json"
            ))
            .unwrap();
            self.recipe.ring_costmap = Some(profile.clone());
            self.recipe.access_costmap = Some(profile);
            let grid = |trail| serde_json::json!({"engine":papyrus_terrain::road_costmap::VERSION,"resolution":257,"seed":7,"trail":trail,"costs":vec![101;257*257]});
            self.report.ring_costmap = Some(serde_json::json!({"grid":grid(false)}));
            self.report.source_access_costmaps = vec![
                serde_json::json!({"family":"paved","grid":grid(false)}),
                serde_json::json!({"family":"dirt","grid":grid(true)}),
            ];
        }
        fn points(&self) -> Vec<Vec<[f32; 3]>> {
            self.report.roads.iter().map(|r| r.points.clone()).collect()
        }
        fn env(&self) -> Environment<'_> {
            let family_settings = std::collections::BTreeMap::from([
                (
                    0,
                    road_network::Settings {
                        width: 12.,
                        ..self.settings
                    },
                ),
                (1, self.settings),
                (
                    2,
                    road_network::Settings {
                        width: 4.,
                        shoulder: 1.,
                        splat_channel: 0,
                        ..self.settings
                    },
                ),
            ]);
            Environment {
                maps: &self.maps,
                recipe: self.recipe.clone(),
                costs: None,
                cost_lattice: None,
                obstacles: vec![],
                owners: vec![],
                settings: self.settings,
                protected_without_water: self.settings.protected_topology,
                family_settings,
                ownership: road_network::EntrancePolicy {
                    minimum_height: 1.,
                    owned_topology: 1024,
                    maximum_exit_distance: 512.,
                    maximum_exit_nodes: 4096,
                },
                launch: self.recipe.entrance_launch.unwrap(),
                height_origin: -500.,
                vertical_span: 1000.,
                water_policy: self.recipe.access_river_water.clone(),
                source_water: None,
                water: None,
            }
        }
        fn validate(&self) -> Result<Option<AllRoutesAudit>, AllRoutesFailure> {
            self.env()
                .validate_all_routes(&self.report, &self.points(), 128_000_000, || false)
        }
    }
    #[test]
    fn source_surface_contract_cannot_validate_without_its_retained_ordered_plan() {
        let mut f = Fixture::new();
        assert!(f.validate().unwrap().is_some());
        f.recipe.staged = Some(crate::road_generation::Policy {
            engine: crate::road_generation::SOURCE_VERSION.into(),
            roadside_source_sha256: "a".repeat(64),
            roadside_preferences: Vec::new(),
            layout: None,
        });
        assert_eq!(
            f.validate().unwrap_err().reason,
            "Source road surface checks require their retained ordered plan"
        );
    }
    #[test]
    fn empty_final_routes_are_explicitly_scoped_without_inventing_a_launch_or_water_pass() {
        let mut f = Fixture::new();
        f.report.roads.clear();
        f.report.families.clear();
        f.report.entrance_launch = None;
        let audit = f.validate().unwrap().unwrap();
        assert_eq!(audit.roads, 0);
        assert!(audit.families.is_empty());
        assert!(audit.complete_final_routes_checked && !audit.runtime_validated);
        f.report.families.push(Family {
            infrastructure_type: 0,
            hierarchy: 0,
            seed: 7,
            first_road: 1,
            road_count: 0,
            socket_count: 0,
            unresolved: vec![],
            width: 12.,
            splat_channel: 7,
        });
        assert!(f.validate().is_err());
    }
    #[test]
    fn full_route_gate_checks_all_families_optional_costs_and_mandatory_ground() {
        let mut f = Fixture::new();
        let audit = f.validate().unwrap().unwrap();
        assert_eq!(audit.roads, 3);
        assert_eq!(audit.families.len(), 3);
        assert!(
            audit
                .families
                .iter()
                .all(|a| a.audit.whole_selected_routes_checked && !a.audit.source_costs_applicable)
        );
        f.costs();
        assert!(
            f.validate()
                .unwrap()
                .unwrap()
                .families
                .iter()
                .all(|a| a.audit.source_costs_applicable)
        );
        f.report.source_access_costmaps[1]["grid"]["costs"][154 * 257 + 128] = serde_json::json!(0);
        let failed = f.validate().unwrap_err();
        assert_eq!(failed.hierarchy, Some(2));
        assert!(matches!(
            failed.detail.unwrap().error,
            socket_recovery::Failure::CostBlocked { .. }
        ));
        f.recipe.access_costmap = None;
        assert!(
            !f.validate().unwrap().unwrap().families[2]
                .audit
                .source_costs_applicable
        );
        let n = f.maps.layers.surface_resolution;
        f.maps.alpha[(n * 3 / 5) * n + n / 2] = 0;
        let failed = f.validate().unwrap_err();
        assert_eq!(failed.hierarchy, Some(2));
        assert!(matches!(
            failed.detail.unwrap().error,
            socket_recovery::Failure::GroundBlocked { .. }
        ));
        assert!(
            f.env()
                .validate_all_routes(&f.report, &f.points(), 128_000_000, || true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.env()
                .validate_all_routes(&f.report, &f.points(), 1, || false)
                .unwrap_err()
                .work,
            1
        );
    }
    #[test]
    fn full_route_gate_rejects_bad_shapes_ranges_seeds_and_incomplete_coverage() {
        let mut f = Fixture::new();
        f.costs();
        f.report.families[1].width += 1.;
        assert!(f.validate().is_err());
        f.report.families[1].width -= 1.;
        f.report.source_access_costmaps[1]["grid"]["seed"] = serde_json::json!(8);
        assert_eq!(f.validate().unwrap_err().hierarchy, Some(2));
        f.report.source_access_costmaps[1]["grid"]["seed"] = serde_json::json!(7);
        let last = f.report.families.pop().unwrap();
        assert_eq!(
            f.validate().unwrap_err().reason,
            "Incomplete source route coverage"
        );
        f.report.families.push(last);
        f.report.families[1].first_road = 0;
        assert_eq!(
            f.validate().unwrap_err().reason,
            "Overlapping complete route families"
        );
    }
    #[test]
    fn complete_gate_preserves_only_exact_checked_exit_cost_buffers() {
        let mut f = Fixture::new();
        f.costs();
        let socket = road_network::Destination::Monument {
            monument: 1,
            connector: 11,
        };
        f.report.roads[1].from = socket;
        f.report.entrance_joins.push(road_network::EntranceJoin {
            monument: 1,
            connector: 11,
            points: f.report.roads[1].points.clone(),
        });
        f.report.source_access_costmaps[0]["grid"]["costs"][128 * 257 + 154] = serde_json::json!(0);
        let costs: Vec<u32> =
            serde_json::from_value(f.report.source_access_costmaps[0]["grid"]["costs"].clone())
                .unwrap();
        let owner = road_network::Obstacle {
            monument: 1,
            center: [200., 0.],
            extent: [10., 10.],
            yaw: 0.,
        };
        {
            let mut env = f.env();
            env.owners.push(owner.clone());
            assert!(
                env.validate_all_routes(&f.report, &f.points(), 128_000_000, || false)
                    .unwrap()
                    .unwrap()
                    .complete_final_routes_checked
            );
            // The superseded per-socket check cannot represent that source
            // exception. It must not be reapplied after a complete family pass.
            let input = env.input_with_source_policy(Some(&costs), None);
            assert!(
                socket_recovery::validate_routes(
                    &f.report,
                    &input,
                    socket_recovery::Key {
                        monument: 1,
                        connector: 11
                    },
                    &f.points(),
                    128_000_000,
                    || false
                )
                .is_err()
            );
        }
        f.report.entrance_joins[0].points[1][2] += 10.;
        let mut env = f.env();
        env.owners.push(owner);
        let failure = env
            .validate_all_routes(&f.report, &f.points(), 128_000_000, || false)
            .unwrap_err();
        assert_eq!(failure.hierarchy, Some(1));
        assert!(matches!(
            failure.detail.unwrap().error,
            socket_recovery::Failure::CostBlocked { .. }
        ));
    }
    #[test]
    fn source_terrain_water_policy_covers_dirt_without_borrowing_ring_permissions() {
        let mut f = Fixture::new();
        let n = f.maps.layers.surface_resolution;
        f.maps.layers.topology[(n * 3 / 5) * n + n / 2] |= 16384;
        let terrain: crate::road_terrain_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-terrain.json"
        ))
        .unwrap();
        let mut env = f.env();
        assert!(env.family_water_policy(2).unwrap().is_none());
        env.recipe.staged = Some(crate::road_generation::Policy {
            engine: crate::road_generation::RAW_SOURCE_VERSION.into(),
            roadside_source_sha256: "a".repeat(64),
            roadside_preferences: Vec::new(),
            layout: None,
        });
        assert!(env.family_water_policy(2).is_err());
        env.source_water = Some(SourceWater {
            policy: terrain.water_policy().unwrap(),
            terrain_profile_sha256: terrain.sha256().unwrap(),
            ocean_level: terrain.water_no_hit_level,
        });
        env.water = Some(vec![17.; n * n]);
        assert!(env.recipe.ring_costmap.is_none());
        assert!(env.recipe.access_river_water.is_none());
        for hierarchy in 0..=2 {
            let policy = env.family_water_policy(hierarchy).unwrap().unwrap();
            assert_eq!(policy.topology, terrain.policy.water_topology);
            assert_eq!(policy.clearance, terrain.policy.water_clearance);
        }
        assert!(env.family_water_policy(3).is_err());
        let policy = env.family_water_policy(2).unwrap().unwrap();
        let guide = papyrus_terrain::road_water::Guide {
            policy,
            heights: env.water.as_deref().unwrap(),
        };
        let mut input = env.input_with_source_policy(None, Some(guide));
        input.settings = road_network::Settings {
            protected_topology: env.family_settings[&2].protected_topology & !policy.topology,
            ..env.family_settings[&2]
        };
        assert!(
            socket_recovery::validate_road_surface_set(
                &f.report,
                &input,
                &[2],
                &f.points(),
                128_000_000,
                || false
            )
            .unwrap()
            .is_some()
        );
        let mut protected = f.maps.layers.clone();
        protected.topology[(n * 3 / 5) * n + n / 2] |= 1024;
        input.layers = &protected;
        assert!(matches!(
            socket_recovery::validate_road_surface_set(
                &f.report,
                &input,
                &[2],
                &f.points(),
                128_000_000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(socket_recovery::RouteFailure {
                error: socket_recovery::Failure::GroundBlocked { .. },
                ..
            })
        ));
        input.layers = &f.maps.layers;
        let holes = vec![0; n * n];
        input.alpha = &holes;
        assert!(matches!(
            socket_recovery::validate_road_surface_set(
                &f.report,
                &input,
                &[2],
                &f.points(),
                128_000_000,
                || false
            )
                .as_ref()
                .map_err(Box::as_ref),
            Err(socket_recovery::RouteFailure {
                error: socket_recovery::Failure::GroundBlocked { .. },
                ..
            })
        ));
    }
    #[test]
    fn final_paved_failures_keep_confirmed_wet_dirt_feedback() {
        let mut f = Fixture::new();
        f.costs();
        let points = f.points();
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 10.).unwrap();
        let mut plan =
            crate::road_planning::Plan::from_report(std::mem::take(&mut f.report), || false)
                .unwrap()
                .unwrap();
        plan.bind_uniform_test_costs(lattice);
        let mut env = f.env();
        env.recipe.staged = Some(crate::road_generation::Policy {
            engine: crate::road_generation::RAW_SOURCE_VERSION.into(),
            roadside_source_sha256: "a".repeat(64),
            roadside_preferences: Vec::new(),
            layout: None,
        });
        let terrain: crate::road_terrain_source::Profile = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/client-0f703d43-road-terrain.json"
        ))
        .unwrap();
        env.source_water = Some(SourceWater {
            policy: terrain.water_policy().unwrap(),
            terrain_profile_sha256: terrain.sha256().unwrap(),
            ocean_level: 0.,
        });
        let ground = &f.maps.layers.height;
        let accepted = env
            .validate_all_routes_from_plan(&plan, &points, ground, 128_000_000, || false)
            .unwrap()
            .unwrap();
        let reference: Vec<_> = ground.iter().flat_map(|word| word.to_le_bytes()).collect();
        assert_eq!(
            accepted.constructed_ground_sha256,
            Some(format!("{:x}", Sha256::digest(reference))),
        );
        let limit = ground.len() * 4 - 1;
        let bounded = env
            .validate_all_routes_from_plan(&plan, &points, ground, limit, || false)
            .unwrap_err();
        assert_eq!(bounded.reason, "Constructed ground binding work exhausted");
        assert_eq!(bounded.work, limit);
        let n = f.maps.layers.height_resolution;
        let mut height = f.maps.layers.height.clone();
        let below_sea_level = (499_f32 / 1000. * 32766.).round() as i16;
        for (z, row) in height.chunks_exact_mut(n).enumerate() {
            let world_z = (z as f32 / (n - 1) as f32 - 0.5) * 1000.;
            if (80. ..=120.).contains(&world_z) {
                row[n / 2 - 2..=n / 2 + 2].fill(below_sea_level);
            }
        }
        let audit = |env: &Environment<'_>, height: &[i16], work| {
            env.validate_all_routes_from_plan(&plan, &points, height, work, || false)
                .unwrap_err()
        };
        for (hierarchy, foreign_obstacle) in [(0, true), (0, false), (1, false)] {
            let mut height = height.clone();
            if foreign_obstacle {
                env.obstacles.push(road_network::Obstacle {
                    monument: 42,
                    center: [200., 0.],
                    extent: [8., 8.],
                    yaw: 0.,
                });
            } else {
                env.obstacles.clear();
                let target_x = if hierarchy == 0 { 200. } else { 80. };
                let to_world = |cell| (cell as f32 / (n - 1) as f32 - 0.5) * 1000.;
                for (z, row) in height.chunks_exact_mut(n).enumerate() {
                    if to_world(z).abs() <= 4. {
                        for (x, cell) in row.iter_mut().enumerate() {
                            if (to_world(x) - target_x).abs() <= 4. {
                                *cell = below_sea_level;
                            }
                        }
                    }
                }
            }
            let failed = audit(&env, &height, 128_000_000);
            assert_eq!(failed.hierarchy, Some(hierarchy), "{failed:?}");
            let detail = failed.detail.as_ref().unwrap();
            assert!(matches!(
                &detail.error,
                socket_recovery::Failure::GroundBlocked { road, obstacle, ground, .. }
                    if *road == hierarchy as usize
                        && if foreign_obstacle {
                            obstacle.as_ref().is_some_and(|obstacle| obstacle.monument == 42)
                        } else {
                            ground.as_ref().is_some_and(|ground| ground.reason == "water or low ground")
                        }
            ), "{failed:?}");
            assert_eq!(detail.additional_blocked_ground.len(), 2, "{failed:?}");
            for crossing in &detail.additional_blocked_ground {
                assert!(matches!(
                    crossing,
                    socket_recovery::Failure::GroundBlocked { road: 2, ground: Some(ground), .. }
                        if ground.reason == "water or low ground"
                ));
            }
            if hierarchy == 0 {
                let primary_work =
                    height.len() * 4 + points.len() + lattice.resolution().pow(2) + detail.work;
                let bounded = audit(&env, &height, primary_work);
                assert_eq!(bounded.hierarchy, Some(0));
                assert_eq!(bounded.work, primary_work);
                assert!(bounded.detail.unwrap().additional_blocked_ground.is_empty());
                assert!(failed.work > primary_work);
            }
        }
    }

    #[test]
    fn paved_water_policy_cannot_leak_into_dirt_route_protection() {
        let mut f = Fixture::new();
        f.costs();
        f.recipe.access_river_water = f
            .recipe
            .ring_costmap
            .as_ref()
            .unwrap()
            .joins
            .as_ref()
            .unwrap()
            .river_water
            .clone();
        let n = f.maps.layers.surface_resolution;
        f.maps.layers.topology[(n / 2) * n + n * 3 / 5] |= 16384;
        let mut env = f.env();
        env.water = Some(vec![17.; n * n]);
        let audit = env
            .validate_all_routes(&f.report, &f.points(), 128_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(audit.families[1].water_guide_available);
        assert!(
            !audit.families[2].water_guide_available && audit.families[2].water_policy.is_none()
        );
        f.maps.layers.topology[(n * 3 / 5) * n + n / 2] |= 16384;
        let mut env = f.env();
        env.water = Some(vec![17.; n * n]);
        let failure = env
            .validate_all_routes(&f.report, &f.points(), 128_000_000, || false)
            .unwrap_err();
        assert_eq!(failure.hierarchy, Some(2));
    }
}
