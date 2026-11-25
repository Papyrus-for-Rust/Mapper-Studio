//! Headless access to the same engine, placement, cache and composition adapters.
use crate::{
    Project, composition, native_preview, native_processing, placement_preview, prefab_assets,
    prefab_textures,
};
use eframe::egui::ColorImage;
use papyrus_terrain::{landscape, monument_composition::Composed, placement::Solution};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub struct Options {
    pub rust_root: Option<PathBuf>,
    pub worker: Option<PathBuf>,
    pub catalog: Option<PathBuf>,
    pub bindings: Option<PathBuf>,
    pub skip_composition: bool,
}
pub struct Generated {
    pub recipe: Project,
    pub image: ColorImage,
    pub heights: Vec<i16>,
    pub resolution: usize,
    pub native: Option<native_processing::Processed>,
    pub composed: Option<Composed>,
    pub cables: Option<std::sync::Arc<crate::scene_wires::Scene>>,
    pub independent_biomes: Option<Vec<u8>>,
    pub solution: Solution,
    pub asset_receipts: Option<serde_json::Value>,
    pub warnings: Vec<String>,
}

pub struct MapWorld {
    pub recipe: Project,
    pub maps: Composed,
    pub solution: Solution,
    pub catalog: prefab_assets::Catalog,
}
fn stopped(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Generation cancelled".into())
    } else {
        Ok(())
    }
}

pub fn generate(
    project: &Project,
    options: &Options,
    cancel: &AtomicBool,
    progress: impl FnMut(&str, f32),
) -> Result<Option<Generated>, String> {
    Ok(generate_with_catalog(project, options, cancel, progress)?
        .map(|(generated, _)| generated))
}

/// Return the verified catalog used for composition when the caller also
/// needs to export the world. Keep an outer verification scope for both steps.
pub fn generate_with_catalog(
    project: &Project,
    options: &Options,
    cancel: &AtomicBool,
    progress: impl FnMut(&str, f32),
) -> Result<Option<(Generated, Option<prefab_assets::Catalog>)>, String> {
    generate_inner(project, options, cancel, true, progress)
}

/// Compose the accepted world for `.map` export without preparing its detailed
/// overview or inspection data. Retain an outer verification scope for export.
pub fn generate_map_world(
    project: &Project,
    options: &Options,
    cancel: &AtomicBool,
    progress: impl FnMut(&str, f32),
) -> Result<Option<MapWorld>, String> {
    let Some((generated, catalog)) = generate_inner(project, options, cancel, false, progress)?
    else {
        return Ok(None);
    };
    Ok(Some(MapWorld {
        recipe: generated.recipe,
        maps: generated.composed.ok_or("Map export requires the accepted composed world")?,
        solution: generated.solution,
        catalog: catalog.ok_or("Map export requires the verified generation catalog")?,
    }))
}

fn generate_inner(
    project: &Project,
    options: &Options,
    cancel: &AtomicBool,
    preview: bool,
    mut progress: impl FnMut(&str, f32),
) -> Result<Option<(Generated, Option<prefab_assets::Catalog>)>, String> {
    let mut timing = composition::StageTimer::start("headless_generation");
    let _verification = crate::installation::VerificationScope::default();
    let _identities = crate::prefab_identities::Scope::default();
    let _monuments = crate::monument_source::Scope::default();
    project.validate()?;
    project.require_master_seed()?;
    let mut effective = project.clone();
    crate::generation_defaults::apply(&mut effective);
    let project = &effective;
    project.validate()?;
    if options.skip_composition && project.gpu_erosion.is_some() {
        return Err("GPU erosion recipes require composition".into());
    }
    if options.skip_composition && project.terrain_graph.is_some() {
        return Err("Terrain graph recipes require composition".into());
    }
    if project.rails.is_some() && options.skip_composition {
        return Err(
            "A rail-enabled recipe requires composition; --no-compose would skip requested rails"
                .into(),
        );
    }
    if project
        .topology_edits
        .as_ref()
        .is_some_and(|e| e.enabled && !e.strokes.is_empty())
        && options.skip_composition
    {
        return Err("Cannot skip composition with manual topology overrides enabled".into());
    }
    if project.decoration.is_some() && options.skip_composition {
        return Err("A decoration-enabled recipe requires composition; --no-compose would skip requested decoration".into());
    }
    if project.roads.is_some() && options.skip_composition {
        return Err(
            "A road-enabled recipe requires composition; --no-compose would skip requested roads"
                .into(),
        );
    }
    if project.performance.monuments && project.has_monument_addons() && options.skip_composition {
        return Err("A recipe with monument add-ons requires composition".into());
    }
    if options.catalog.is_some() != options.bindings.is_some() {
        return Err("Provide both --catalog and --bindings".into());
    }
    if options.skip_composition && options.catalog.is_some() {
        return Err("--no-compose cannot be combined with texture asset options".into());
    }
    if placement_preview::is_earthwork(&project.placement_engine) && options.skip_composition {
        return Err("Earthwork placement requires composition; --no-compose is unavailable".into());
    }
    let key = project.terrain_key();
    let native = native_preview::is_native(&key.engine);
    if !native
        && (options.rust_root.is_some() || options.worker.is_some() || options.catalog.is_some())
    {
        return Err("Rust installation/asset options require a native terrain recipe".into());
    }
    if options.catalog.is_some() && project.composition_engine.is_none() {
        return Err("Recipe has monument composition disabled".into());
    }
    let compose = native && project.composition_engine.is_some() && !options.skip_composition;
    if !preview && !compose {
        return Err("Map export requires composed native generation".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    composition::record_timing(&mut timing, "recipe_validated");
    let assets = if compose {
        let root = options
            .rust_root
            .as_deref()
            .ok_or("Native generation requires --rust-root matching the saved installation")?;
        progress("Verifying monument assets", 0.01);
        let source = project
            .installation
            .as_ref()
            .ok_or("Missing installation identity")?;
        let cancelled = || cancel.load(Ordering::Relaxed);
        let explicit = options.catalog.as_ref().zip(options.bindings.as_ref());
        let (catalog, bindings) = if let Some((catalog, bindings)) = explicit {
            let catalog = prefab_assets::load(root, catalog, source, cancelled)?;
            let bindings =
                prefab_textures::Bindings::load_manifest(root, bindings, &catalog, cancelled)?;
            // A valid asset pair can still belong to a different saved recipe.
            project.check_monument_assets(&catalog.sha256, &bindings.sha256)?;
            (catalog, bindings)
        } else {
            crate::asset_discovery::load_cached(
                root,
                source,
                project.monument_assets.as_ref(),
                cancelled,
            )?
        };
        composition::record_timing(&mut timing, "asset_manifests");
        if project.performance.monuments
            && (project.has_monument_addons() || crate::custom_monuments::has_enabled_placements(project))
        {
            let Some(identities) = crate::prefab_identities::Source::prepare(root, &catalog, || {
                cancel.load(Ordering::Relaxed)
            })? else {
                return Ok(None);
            };
            if project.has_monument_addons() {
                let addons = crate::monument_addons::resolve(project)?;
                crate::monument_addons::Dependencies::new(&identities, &catalog)
                    .check(&addons)?;
            }
            if crate::custom_monuments::assemble(project, 0, &identities.prefabs, cancelled)?.is_none() {
                return Ok(None);
            }
        }
        if explicit.is_some() {
            bindings.verify_payloads(cancelled)?;
        }
        composition::record_timing(&mut timing, "asset_dependencies");
        let mut resolved =
            crate::generation_defaults::resolve(project, root, &catalog, &bindings, cancelled)?;
        composition::record_timing(&mut timing, "source_rules");
        resolved.monument_assets.get_or_insert_with(|| crate::AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: bindings.sha256.clone(),
        });
        if resolved.performance.monuments
            && let Some(rules) = &resolved.monument_rules
        {
            crate::tunnel_source::Source::for_recipe(root, &catalog, rules, &cancelled)?;
        }
        composition::record_timing(&mut timing, "tunnel_rules");
        Some((catalog, bindings, resolved))
    } else {
        None
    };
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let selects_layout = assets.as_ref().is_some_and(|(_, _, recipe)| {
        composition::selects_monument_layout(recipe)
    });
    let mut result = if native {
        let root = options
            .rust_root
            .as_deref()
            .ok_or("Native generation requires --rust-root matching the saved installation")?;
        let worker = options
            .worker
            .clone()
            .map(Ok)
            .unwrap_or_else(native_preview::worker_path)?;
        let processed = if options.skip_composition {
            native_processing::build(root, &worker, &key, None, cancel, &mut progress)?
        } else if preview {
            native_processing::build_for_project_with_snapshots(
                root,
                &worker,
                project,
                None,
                None,
                cancel,
                false,
                |label, fraction, _| progress(label, fraction),
            )?
        } else {
            native_processing::build_for_map(root, &worker, project, cancel, &mut progress)?
        };
        let Some(processed) = processed else {
            return Ok(None);
        };
        composition::record_timing(&mut timing, "terrain_ready");
        let image = if compose {
            // Composition supplies the detailed overview once all terrain edits are applied.
            ColorImage::new([0, 0], Vec::new())
        } else {
            progress("Rendering detailed terrain", 0.965);
            let Some(image) = crate::map_image::render_with_palette(
                &processed.source.layers,
                None,
                processed.ocean.as_deref(),
                crate::map_image::DISPLAY_RESOLUTION,
                processed.source.palette.as_deref(),
                || cancel.load(Ordering::Relaxed),
            )?
            else {
                return Ok(None);
            };
            image
        };
        let solution = if selects_layout || placement_preview::is_earthwork(&project.placement_engine) {
            // Filled after source-bound asset verification below; never published as a result.
            Solution {
                outcomes: Vec::new(),
                required_missing: 0,
                attempts: 0,
            }
        } else {
            progress("Searching monument layout", 0.97);
            let Some(solution) =
                placement_preview::solve(project, &processed.source.heights, native_preview::RESOLUTION, || {
                    cancel.load(Ordering::Relaxed)
                })?
            else {
                return Ok(None);
            };
            solution
        };
        let heights = if preview {
            processed.source.heights.as_ref().clone()
        } else {
            Vec::new()
        };
        Generated {
            recipe: project.clone(),
            image,
            heights,
            resolution: native_preview::RESOLUTION,
            native: Some(processed),
            composed: None,
            cables: None,
            independent_biomes: None,
            solution,
            asset_receipts: None,
            warnings: Vec::new(),
        }
    } else {
        if key.engine != landscape::VERSION {
            return Err("CLI generation requires the current landscape model or a supported native recipe. Legacy projects can still be inspected and opened in the GUI.".into());
        }
        let Some(land) = landscape::generate(
            landscape::Settings {
                terrain_seed: key.seed as i32,
                biome_seed: key.biome_seed.ok_or("Missing biome seed")? as i32,
                resolution: 513,
                size: key.size as usize,
            },
            |s, _| progress(s.label(), 0.5),
            || cancel.load(Ordering::Relaxed),
        )?
        else {
            return Ok(None);
        };
        let image = crate::terrain_view::render(&land, key.size, key.biome_seed.unwrap(), 513)?;
        let heights: Vec<_> = land
            .heights
            .iter()
            .map(|&h| ((h + 500.) / 1000. * 32766. + 0.5).clamp(0., 32766.) as i16)
            .collect();
        let Some(solution) =
            placement_preview::solve(project, &heights, 513, || cancel.load(Ordering::Relaxed))?
        else {
            return Ok(None);
        };
        let mut biomes = Vec::with_capacity(land.biomes.len() * 4);
        for c in 0..4 {
            biomes.extend(land.biomes.iter().map(|b| b[c]));
        }
        Generated {recipe:project.clone(),image,heights,resolution:513,native:None,composed:None,cables:None,independent_biomes:Some(biomes),solution,asset_receipts:None,warnings:vec!["Independent preview has sampled terrain and four biome channels; full native surface layers and authored monument stamps are absent".into()]}
    };
    let mut verified_catalog = None;
    if let (Some((catalog, bindings, resolved_project)), Some(raw)) = (assets, &result.native)
    {
        let project = &resolved_project;
        result.recipe = project.clone();
        if !selects_layout && placement_preview::is_earthwork(&project.placement_engine) {
            progress("Searching authored monument earthwork", 0.985);
            let Some(solution) = placement_preview::solve_native(
                project,
                &raw.source.layers,
                &catalog,
                &bindings,
                || cancel.load(Ordering::Relaxed),
            )?
            else {
                return Ok(None);
            };
            result.solution = solution;
        }
        progress("Composing map", 0.99);
        let Some(composition::ResolvedLayout {
            base: _,
            maps: composed,
            solution: resolved,
            cables,
        }) = composition::compose_resolved_layout(
            &raw.source,
            project,
            (!selects_layout).then_some(&result.solution),
            &catalog,
            &bindings,
            options.worker.as_deref(),
            preview,
            || cancel.load(Ordering::Relaxed),
        )?
        else {
            return Ok(None);
        };
        composition::record_timing(&mut timing, "composed");
        result.solution = resolved;
        if !preview {
            result.composed = Some(composed);
            return Ok((!cancel.load(Ordering::Relaxed)).then_some((result, Some(catalog))));
        }
        match cables {
            Ok(scene) => result.cables = scene,
            Err(error) => result.warnings.push(format!("Cable preview unavailable: {error}")),
        }
        progress("Rendering preview", 0.995);
        let Some(image) = crate::map_image::render_composed(
            &composed,
            crate::map_image::DISPLAY_RESOLUTION,
            raw.source.palette.as_deref(),
            || cancel.load(Ordering::Relaxed),
        )?
        else {
            return Ok(None);
        };
        result.image = image;
        if let Some(roads) = &composed.roads {
            result.warnings.push(format!("Road network: {} paths; {} unresolved monuments; {} incomplete metadata records.{} Junction meshes, bridges, road objects and game validation are pending.",roads.roads.len(),roads.unresolved.len(),roads.missing_metadata.len(),if roads.budget_exhausted {" Search budget reached."}else{""}));
            if let Some(ring) = &roads.ring {
                result.warnings.push(format!(
                    "Main road ring: {}; {} routing attempts; {} inward waypoint moves.{}",
                    ring.status,
                    ring.route_attempts,
                    ring.waypoint_moves,
                    ring.reason.map(|r| format!(" {r}")).unwrap_or_default()
                ));
                if let Some(reason) = ring.shaping_fallback {
                    result.warnings.push(format!("Main road ring retains its original valid path because smoothing failed: {reason}."));
                }
            }
            if roads.shaping_policy.is_some() {
                result.warnings.push(format!("Road shaping: {} accepted smooth paths; {} original-route fallbacks ({} reached their shaping budget). Authored entrance chains and network junctions remain fixed.", roads.shaping.accepted, roads.shaping.fallbacks, roads.shaping.budget_fallbacks));
            }
            if let Some(surface) = &roads.surfaces {
                result.warnings.push(format!("Road surfaces: {} source-template LOD chunks. Material shaders, collision cooking and runtime object assembly remain unvalidated.",surface.chunks.len()));
            }
        }
        result.warnings.push(composed.spawn_audit.as_ref().map_or_else(
            || "Spawn eligibility uninspected: no matching installation-derived filters are cached".into(),
            |audit| format!("Spawn audit: {} potential player cells; {} terrain rules; {} fixed-point populations excluded. Physics, placement occupancy, population density and in-game spawning remain unvalidated.{}", audit.character_candidate_cells, audit.rules.len(), audit.fixed_point_populations.len(), if audit.character_candidate_cells == 0 { " No potential player spawn area found." } else { "" }),
        ));
        if let Some(r) = &composed.topology_overrides {
            result.warnings.push(format!("Manual topology: {} strokes; {} changed cells. Spawn diagnostics refreshed; object filters, geometry and water surfaces are not regenerated.",r.strokes,r.changed_cells));
        }
        if let Some(objects) = &composed.objects {
            result.warnings.push(format!("Live objects: {} admitted instances; {} scene phases outside the object pass. Prefab assembly and physics validation remain pending.", objects.instances.len(), objects.external_phases.len()));
        }
        if let Some(decor) = &composed.decoration {
            if let Some(p) = &decor.terrain_placement {
                result.warnings.push(format!("Decoration terrain: {} accepted placements; {} overlap rejections; {} protected-ground rejections; {} height writes. Physics and complete prefab validation pending.",p.counts.accepted,p.counts.occupied,p.counts.protected,p.height_writes));
                if let Some(t) = &p.topology {
                    result.warnings.push(format!("Decoration topology: {} instances; {} changed cells; {} filter rejections; {} earlier-filter conflicts.",t.instances,t.changed_cells,t.filter_rejections,t.read_conflicts));
                }
                if let Some(m) = &p.materials {
                    result.warnings.push(format!("Decoration materials: {} modifier instances; {} changed cells; {} filter rejections; {} earlier-filter conflicts. Complete prefab and physics validation pending.",m.instances,m.changed_cells,m.filter_rejections,m.read_conflicts));
                }
                if let Some(h) = &p.height_effects {
                    result.warnings.push(format!("Decoration height modes: {} modifier instances; {} changed samples; maximum raise {:.2} m, lower {:.2} m. {}",h.instances,h.changed_samples,h.maximum_raise,h.maximum_lower,if p.collider_effects.is_some() {"Game validation remains pending."} else {"Collider contributors and game validation remain pending."}));
                }
                if let Some(c) = &p.collider_effects {
                    result.warnings.push(format!("Decoration raw colliders: {} instances; {} meshes; {} raised samples; maximum raise {:.2} m. PhysX cooking and game validation remain pending.",c.instances,c.meshes,c.changed_samples,c.maximum_raise));
                }
                if let Some(blocking) = &p.blocking {
                    result.warnings.push(format!("Decoration blockers: {} accepted instances; {} blocked surface cells. Conservative reservations are additional; runtime placement remains unvalidated.", blocking.instances, blocking.cells));
                }
            }
            let fitting = decor.fitting.as_ref().map(|f| format!(" {} geometrically fitted poses; {} unsupported; {} anchor rejections. Terrain effects, occupancy and game validation pending.{}",f.counts.fitted,f.counts.unsupported,f.counts.rejected_anchors,if f.budget_exhausted {" Fitting budget reached."}else{""})).unwrap_or_else(||" Authored transforms, anchors, terrain effects and collision checks remain pending.".into());
            result.warnings.push(format!(
                "Static decoration: {} provisional candidates; {} validated instances.{fitting}{}",
                decor.candidates.len(),
                decor.validated_instances,
                if decor.budget_exhausted {
                    " Candidate budget reached; see per-stage coverage."
                } else {
                    ""
                }
            ));
        }
        result.asset_receipts = Some(
            serde_json::json!({"catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,"sources":catalog.sources}),
        );
        result.warnings.push(if composed.ocean.is_some() {
            "Ocean classification was rebuilt after composition. Local water bodies, colliders and in-game entrance validation remain unsupported"
        } else { "Legacy composition has no ocean recomputation, local water bodies, collider or in-game entrance validation" }.into());
        result.composed = Some(composed);
        if crate::monument_source::supported(&project.placement_engine) {
            if crate::coastal_placement::supported(&project.placement_engine) {
                result.warnings.push("Ocean-coastal source transforms, authored terrain checks and final deep-water boat reachability enabled. Freshwater surfaces, water collision volumes and runtime physics remain unvalidated.".into());
            }
            result.warnings.push("Source monument anchors, local terrain filters and tier-corner checks enabled. Global stage policy, placement-map occupancy, remaining checks and physics are not fully validated.".into());
        }
        verified_catalog = Some(catalog);
    } else if native {
        result
            .warnings
            .push("Terrain/layout preview only; monument modifiers were not composed".into());
    }
    if result.solution.required_missing > 0
        && let Some(issue) = crate::generation_validation::required_monuments_issue(
            &result.recipe, &result.solution,
        )
    {
        result.warnings.push(issue);
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    Ok(Some((result, verified_catalog)))
}

/// Preview artifacts only. This deliberately cannot publish a Rust .map.
pub fn save_bundle(
    project: &Project,
    generated: &Generated,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<serde_json::Value, String> {
    project.validate()?;
    stopped(cancel)?;
    if project != &generated.recipe {
        return Err("Preview artifacts do not match the supplied recipe".into());
    }
    let world_rivers = generated.composed.as_ref().and_then(|c| c.rivers.as_ref())
        .or_else(|| generated.native.as_ref().and_then(|n| n.source.rivers.as_ref()));
    if project.rivers.is_some() != world_rivers.is_some()
        || (project.rivers.is_some()
            && generated.native.as_ref().is_none_or(|n| {
                let key = native_processing::initial_key(
                    &project.terrain_key(),
                    native_processing::rivers_follow_terrain(project) && n.source.rivers.is_none(),
                );
                n.source.key != native_preview::processing_key(&key)
            }))
        || (project.rivers.is_some()
            && generated
                .composed
                .as_ref()
                .is_some_and(|c| c.rivers.is_none()))
    {
        return Err(
            "River preview artifacts are missing or differ from the supplied recipe".into(),
        );
    }
    if let (Some(recipe), Some(world)) = (&project.rivers, world_rivers) {
        recipe.verify_world(world, project.seeds[2].value)?;
    }
    if destination
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("map"))
    {
        return Err("--output must name a new bundle directory. Use --map for a playable map.".into());
    }
    if destination.exists() || destination.file_name().is_none() {
        return Err("Preview output must be a new directory".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stage = tempfile::Builder::new()
        .prefix(".mapper-stage-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let root = stage.path();
    let mut bundle = BundleWriter::new(root, cancel);
    bundle.document("project.mapper", project, "Project exceeds the 4 MiB limit.")?;
    if let Some(cables) = &generated.cables {
        bundle.compact("cables.json", cables.as_ref())?;
    }
    bundle.document(
        "generation-provenance.json",
        &crate::provenance::Provenance::for_recipe(project, &[]),
        "Document exceeds 4 MiB",
    )?;
    if let Some(palette) = generated
        .native
        .as_ref()
        .and_then(|n| n.source.palette.as_ref())
    {
        bundle.bytes("terrain-palette.json", palette.bytes())?;
    }
    if let Some(rails) = generated.composed.as_ref().and_then(|c| c.rails.as_ref()) {
        bundle.pretty("rails.json", &rails.report)?;
        bundle.bytes("rail-placement-blocked.u8", &rails.reservations.raster)?;
        bundle.compact("rail-placement-sweeps.json", &rails.reservations.samples)?;
        for chunk in &rails.meshes {
            bundle.compact(&chunk.filename(), chunk)?;
        }
    }
    if let Some(height) = generated
        .composed
        .as_ref()
        .and_then(|c| c.gameplay_height.as_ref())
    {
        let layers = &generated.composed.as_ref().unwrap().layers;
        if height.height.len() != layers.height.len()
            || height
                .height
                .iter()
                .zip(&layers.height)
                .any(|(h, t)| h < t || *h > 32766)
        {
            return Err("Invalid composed gameplay height grid".into());
        }
        bundle.words("gameplay-height.i16le", &height.height, |h| h.to_le_bytes())?;
        bundle.pretty("gameplay-height.json", &height.report)?;
    }
    validate_overview(&generated.image)?;
    bundle.file("overview.png", |file| encode_png(file, &generated.image))?;
    let layout = serde_json::json!({"engine":project.placement_engine,"seed":project.seeds[4].value,"required_missing":generated.solution.required_missing,"attempts":generated.solution.attempts,
        "outcomes":generated.solution.outcomes.iter().map(|o|serde_json::json!({"id":o.id,"position":o.position.as_ref().map(|p|serde_json::json!({"x":p.x,"y":p.y,"z":p.z,"yaw":p.yaw,"clearance":p.clearance})),"distance":o.distance,"within_radius":o.within_radius,"reason":o.reason})).collect::<Vec<_>>()});
    bundle.pretty("layout.json", &layout)?;
    if let Some(roads) = generated.composed.as_ref().and_then(|c| c.roads.as_ref()) {
        bundle.pretty("roads.json", &roads.json())?;
        if let Some(surface) = &roads.surfaces {
            for chunk in &surface.chunks {
                bundle.compact(&chunk.filename(), chunk)?;
            }
        }
    }
    if let Some(audit) = generated
        .composed
        .as_ref()
        .and_then(|c| c.spawn_audit.as_ref())
    {
        bundle.pretty("spawn-audit.json", &audit.json()?)?;
        bundle.bytes("character-spawn-candidates.u8", &audit.character_mask)?;
    }
    if let Some(offshore) = generated.composed.as_ref().and_then(|c| c.offshore.as_ref()) {
        bundle.pretty("offshore.json", offshore)?;
    }
    if let Some(tunnels) = generated.composed.as_ref().and_then(|c| c.tunnels.as_ref()) {
        bundle.pretty("tunnels.json", tunnels)?;
    }
    if let Some(dungeons) = generated.composed.as_ref().and_then(|c| c.dungeons.as_ref()) {
        bundle.pretty("dungeons.json", dungeons)?;
    }
    if let Some(objects) = generated.composed.as_ref().and_then(|c| c.objects.as_ref()) {
        bundle.pretty("objects.json", objects)?;
        bundle.bytes("objects-occupancy.u8", &objects.terrain.occupancy)?;
        if let Some(blocked) = &objects.terrain.blocking {
            bundle.bytes("objects-blocked.u8", &blocked.raster)?;
        }
    }
    if let Some(decor) = generated
        .composed
        .as_ref()
        .and_then(|c| c.decoration.as_ref())
    {
        bundle.pretty("decoration-candidates.json", decor)?;
    }
    if let Some(placement) = generated
        .composed
        .as_ref()
        .and_then(|c| c.decoration.as_ref())
        .and_then(|d| d.terrain_placement.as_ref())
    {
        bundle.bytes("decoration-occupancy.u8", &placement.occupancy)?;
        if let Some(blocking) = &placement.blocking {
            bundle.bytes("decoration-blocked.u8", &blocking.raster)?;
        }
    }
    if let Some(report) = generated
        .composed
        .as_ref()
        .and_then(|c| c.topology_overrides.as_ref())
    {
        bundle.pretty("topology-overrides.json", report)?;
    }
    if let Some(report) = generated.composed.as_ref().and_then(|c| c.clutter_topology.as_ref()) {
        bundle.pretty("clutter-topology.json", report)?;
    }
    if let Some(report) = generated.composed.as_ref().and_then(|c| c.cliff_topology.as_ref()) {
        bundle.pretty("cliff-topology.json", report)?;
    }
    let mut resolution = generated.resolution;
    let mut surface = None;
    if let Some(native) = &generated.native {
        let layers = generated
            .composed
            .as_ref()
            .map_or(native.source.layers.as_ref(), |c| &c.layers);
        resolution = layers.height_resolution;
        surface = Some(layers.surface_resolution);
        bundle.words("height.i16le", &layers.height, |h| h.to_le_bytes())?;
        bundle.bytes("biome.u8", &layers.biome)?;
        bundle.bytes("splat.u8", &layers.splat)?;
        bundle.words("topology.i32le", &layers.topology, |t| t.to_le_bytes())?;
        if let Some(composed) = &generated.composed {
            bundle.bytes("alpha.u8", &composed.alpha)?;
        } else {
            let mut intact = io::repeat(255).take(layers.surface_resolution.pow(2) as u64);
            bundle.file("alpha.u8", |writer| {
                io::copy(&mut intact, writer).map_err(|e| e.to_string())?;
                Ok(())
            })?;
        }
        if let Some(ocean) = generated
            .composed
            .as_ref()
            .and_then(|c| c.ocean.as_ref())
            .or_else(|| native.source.rivers.as_ref().and(native.ocean.as_deref()))
        {
            bundle.bytes("ocean-classification.u8", &ocean.classification)?;
            bundle.words("local-water.i16le", &ocean.local_water, |h| h.to_le_bytes())?;
        }
        if let Some(ocean) = &native.ocean {
            bundle.bytes("base-ocean-classification.u8", &ocean.classification)?;
            bundle.words("base-ocean-topology.i32le", &ocean.topology, |t| t.to_le_bytes())?;
        }
        if let Some(rivers) = world_rivers {
            if let Some(objects) = &rivers.objects {
                bundle.pretty("river-objects.json", objects)?;
            }
            let raw = rivers
                .collider_heights(project.size, layers.surface_resolution, || {
                    cancel.load(Ordering::Relaxed)
                })?
                .ok_or("River collider export cancelled")?;
            bundle.words("river-collider-heights.f32le", &raw.heights, |h| h.to_le_bytes())?;
            bundle.pretty("river-paths.json", &serde_json::json!({
                "engine":papyrus_terrain::river_pipeline::VERSION,"recipe":project.rivers,"seed":project.seeds[2].value,
                "layout_policy":rivers.layout_policy,"water_policy":rivers.water_policy,"paths":rivers.paths,
            }))?;
            bundle.pretty("river-generation.json", &rivers.report)?;
            bundle.compact("river-water-geometry.json", &rivers.water_chunks)?;
        }
    } else {
        bundle.words("height.i16le", &generated.heights, |h| h.to_le_bytes())?;
        if let Some(biomes) = &generated.independent_biomes {
            bundle.bytes("biome.u8", biomes)?;
        }
    }
    let native_profile = if generated.native.is_some() {
        let profile = crate::native_profile::resolve(&project.terrain_key())?;
        Some(serde_json::json!({"id":profile.id,"abi":profile.abi,"sha256":profile.sha256()?}))
    } else {
        None
    };
    bundle.sync_files()?;
    let files = bundle.files;
    let mut manifest = serde_json::json!({"format":"papyrus-preview-bundle","schema":1,"playable":false,"size":project.size,"height_resolution":resolution,"surface_resolution":surface,
        "biome_channels":if generated.native.is_some(){5}else{4},"splat_channels":if generated.native.is_some(){Some(8)}else{None},
        "grid_rows":"world -Z to +Z; planar channels; little-endian words","overview_resolution":generated.image.size,"overview_rows":"north (+Z) at top","height_encoding":"signed i16 normalized by 32766 over -500..500 metres",
        "alpha_base":"intact terrain; imported base alpha not supported","composed":generated.composed.is_some(),"composition_engine":if generated.composed.is_some(){project.composition_engine.as_deref()}else{None},
        "water":{"ocean_rebuilt_after_composition":generated.composed.as_ref().is_some_and(|c|c.ocean.is_some()),"local_water":"dry sentinel only (-500 m); water-body rasterization pending"},
        "spawn_audit":generated.composed.as_ref().and_then(|c|c.spawn_audit.as_ref()).map(|a|serde_json::json!({"source_sha256":a.source_sha256,"potential_player_cells":a.character_candidate_cells,"physics_validated":false})),
        "clutter_topology":generated.composed.as_ref().and_then(|c|c.clutter_topology.as_ref()),
        "cliff_topology":generated.composed.as_ref().and_then(|c|c.cliff_topology.as_ref()),
        "tunnels":generated.composed.as_ref().and_then(|c|c.tunnels.as_ref()).map(|s|serde_json::json!({"file":"tunnels.json","engine":s.engine,"source_sha256":s.source_sha256,"instances":s.instances.len(),"game_runtime_validated":false})),
        "dungeons":generated.composed.as_ref().and_then(|c|c.dungeons.as_ref()).map(|scene|serde_json::json!({"file":"dungeons.json","engine":scene.engine,"source_sha256":scene.source_sha256,"roots":scene.stages.iter().map(|s|s.roots.len()).sum::<usize>(),"instances":scene.instances.len(),"game_runtime_validated":false})),
        "offshore":generated.composed.as_ref().and_then(|c|c.offshore.as_ref()).map(|scene|serde_json::json!({"file":"offshore.json","engine":scene.engine,"source_sha256":scene.source_sha256,"instances":scene.instances.len(),"game_runtime_validated":false})),
        "gameplay_height":generated.composed.as_ref().and_then(|c|c.gameplay_height.as_ref()).map(|h|serde_json::json!({"file":"gameplay-height.i16le","report":"gameplay-height.json","engine":h.report.engine,"scope":if project.rails.is_some(){if generated.composed.as_ref().and_then(|c|c.roads.as_ref()).is_some_and(|r|r.surfaces.is_some()){"road and rail collision triangles"}else{"rail collision triangles only"}}else{"road collision triangles only"},"other_late_contributors_complete":false,"physics_cooking_validated":false})),
        "rails":project.rails,
        "decoration":project.decoration,"objects":project.objects,"roads":project.roads,"native_profile":native_profile,"assets":generated.asset_receipts,"warnings":generated.warnings,"native_cache":generated.native.as_ref().map(|n|&n.cache_status),"files":files});
    if let Some(cables) = &generated.cables {
        manifest["cables"] = serde_json::json!({"file":"cables.json","engine":cables.engine,
            "source_sha256":cables.source_sha256,"roots_sha256":cables.roots_sha256,
            "nodes":cables.nodes.len(),"spans":cables.report.spans.len(),
            "registration_order":cables.registration_order,"game_runtime_validated":false});
    }
    if project.rivers.is_some() {
        manifest["rivers"] = serde_json::json!({"recipe":project.rivers,"seed":project.seeds[2].value,
            "paths":"river-paths.json","report":"river-generation.json","geometry":"river-water-geometry.json",
            "collider_heights":"river-collider-heights.f32le","collider_no_hit_height":0.,
            "collider_sampling":"nearest cell-centre triangle proxy"});
        if let Some(objects) = world_rivers.and_then(|r| r.objects.as_ref()) {
            manifest["rivers"]["objects"] = serde_json::json!({"file":"river-objects.json",
                "source_sha256":objects.source_sha256,"seed":objects.seed,"instances":objects.instances.len()});
        }
        manifest["water"]["local_water"] = serde_json::json!(
            "source-template river registration; other local water-body families incomplete"
        );
    }
    if let Some(palette) = generated
        .native
        .as_ref()
        .and_then(|n| n.source.palette.as_ref())
    {
        manifest["terrain_palette"] = serde_json::json!({"sha256":palette.sha256,"biomes":palette.biome_count(),"file":"terrain-palette.json","render_key":palette.source_render_key(),"renderer":if palette.cartography().is_some(){"papyrus-cartographic-colors-1"}else{"papyrus-installation-colors-1"},"cartography_material":palette.defaults(), "biome_coverage_complete":palette.covers_layers(generated.composed.as_ref().map_or(generated.native.as_ref().unwrap().source.layers.as_ref(),|m|&m.layers))});
    }
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let mut file = File::create(root.join("manifest.json")).map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    stopped(cancel)?;
    if destination.exists() {
        return Err("Output appeared during generation; no files replaced".into());
    }
    fs::rename(root, destination).map_err(|e| e.to_string())?;
    Ok(manifest)
}
pub(crate) fn write_png(path: &Path, image: &ColorImage) -> Result<(), String> {
    validate_overview(image)?;
    let file = File::create(path).map_err(|e| e.to_string())?;
    encode_png(BufWriter::new(file), image)
}

fn validate_overview(image: &ColorImage) -> Result<(), String> {
    if image.size.contains(&0)
        || image.size.iter().any(|&n| n > 4096)
        || image.pixels.len() != image.size[0] * image.size[1]
    {
        return Err("Invalid overview image".into());
    }
    Ok(())
}

fn encode_png(file: impl Write, image: &ColorImage) -> Result<(), String> {
    let mut encoder = png::Encoder::new(file, image.size[0] as u32, image.size[1] as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(bytemuck::cast_slice(&image.pixels))
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

struct HashedFile<'a> {
    file: File,
    hash: Sha256,
    bytes: u64,
    cancel: &'a AtomicBool,
}

impl Write for HashedFile<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Generation cancelled"));
        }
        let written = self.file.write(input)?;
        self.hash.update(&input[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

struct BundleWriter<'a> {
    root: &'a Path,
    cancel: &'a AtomicBool,
    files: BTreeMap<String, serde_json::Value>,
    pending_sync: Vec<File>,
}

const BUNDLE_BUFFER_BYTES: usize = 64 * 1024;
const BUNDLE_SYNC_BATCH: usize = 16;
const BUNDLE_SYNC_WORKERS: usize = 4;

impl<'a> BundleWriter<'a> {
    fn new(root: &'a Path, cancel: &'a AtomicBool) -> Self {
        Self {
            root,
            cancel,
            files: BTreeMap::new(),
            pending_sync: Vec::with_capacity(BUNDLE_SYNC_BATCH),
        }
    }

    fn sync_files(&mut self) -> Result<(), String> {
        use rayon::prelude::*;

        if self.pending_sync.is_empty() {
            return Ok(());
        }
        let cancel = self.cancel;
        let pending = &self.pending_sync;
        let sync = |group: &[File]| -> Result<(), String> {
            for file in group {
                stopped(cancel)?;
                file.sync_all().map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        let parallel = pending.len() > 1
            && crate::generation_resources::current().effective_workers() > 1;
        if !parallel
            || crate::generation_resources::try_with_cpu_pool(|pool| {
                let workers = pool.current_num_threads().min(BUNDLE_SYNC_WORKERS);
                let group_size = pending.len().div_ceil(workers);
                pool.install(|| pending.par_chunks(group_size).try_for_each(sync))
            })?.is_none()
        {
            sync(pending)?;
        }
        self.pending_sync.clear();
        Ok(())
    }

    fn file(
        &mut self,
        name: &str,
        write: impl FnOnce(&mut BufWriter<HashedFile<'_>>) -> Result<(), String>,
    ) -> Result<(), String> {
        stopped(self.cancel)?;
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(self.root.join(name))
            .map_err(|e| e.to_string())?;
        let mut writer = BufWriter::with_capacity(
            BUNDLE_BUFFER_BYTES,
            HashedFile {
                file,
                hash: Sha256::new(),
                bytes: 0,
                cancel: self.cancel,
            },
        );
        write(&mut writer)?;
        stopped(self.cancel)?;
        let output = writer.into_inner().map_err(|e| e.to_string())?;
        self.files.insert(
            name.into(),
            serde_json::json!({"bytes":output.bytes,"sha256":format!("{:x}",output.hash.finalize())}),
        );
        self.pending_sync.push(output.file);
        if self.pending_sync.len() == BUNDLE_SYNC_BATCH {
            self.sync_files()?;
        }
        Ok(())
    }

    fn bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let cancel = self.cancel;
        self.file(name, |writer| {
            for chunk in bytes.chunks(BUNDLE_BUFFER_BYTES) {
                stopped(cancel)?;
                writer.write_all(chunk).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }

    fn words<T: bytemuck::Pod, const N: usize>(
        &mut self,
        name: &str,
        values: &[T],
        _to_le_bytes: impl Fn(&T) -> [u8; N],
    ) -> Result<(), String> {
        #[cfg(target_endian = "little")]
        {
            self.bytes(name, bytemuck::cast_slice(values))
        }
        #[cfg(target_endian = "big")]
        {
            let cancel = self.cancel;
            self.file(name, |writer| {
                let mut data = Vec::with_capacity(8192 * N);
                for chunk in values.chunks(8192) {
                    stopped(cancel)?;
                    data.clear();
                    data.extend(chunk.iter().flat_map(&_to_le_bytes));
                    writer.write_all(&data).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        }
    }

    fn compact(&mut self, name: &str, value: &impl serde::Serialize) -> Result<(), String> {
        self.file(name, |writer| {
            serde_json::to_writer(writer, value).map_err(|e| e.to_string())
        })
    }

    fn pretty(&mut self, name: &str, value: &impl serde::Serialize) -> Result<(), String> {
        self.file(name, |writer| {
            serde_json::to_writer_pretty(writer, value).map_err(|e| e.to_string())
        })
    }

    fn document(
        &mut self,
        name: &str,
        value: &impl serde::Serialize,
        oversize: &str,
    ) -> Result<(), String> {
        stopped(self.cancel)?;
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > crate::MAX_PROJECT_BYTES {
            return Err(oversize.into());
        }
        self.bytes(name, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_grids_keep_little_endian_bits() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let mut writer = BundleWriter::new(dir.path(), &cancel);
        writer
            .words("height", &[0x0123_i16, -2], |v| v.to_le_bytes())
            .unwrap();
        writer
            .words("topology", &[0x01020304_i32], |v| v.to_le_bytes())
            .unwrap();
        writer
            .words("river", &[-0.0_f32, 1.0], |v| v.to_le_bytes())
            .unwrap();
        assert_eq!(
            fs::read(dir.path().join("height")).unwrap(),
            [0x23, 1, 0xfe, 0xff]
        );
        assert_eq!(fs::read(dir.path().join("topology")).unwrap(), [4, 3, 2, 1]);
        assert_eq!(
            fs::read(dir.path().join("river")).unwrap(),
            [0, 0, 0, 0x80, 0, 0, 0x80, 0x3f]
        );
    }

    #[test]
    fn bundle_json_stream_preserves_bytes_and_checks_cancellation_during_serialization() {
        use serde::ser::SerializeSeq;

        struct CancelAfterFirstValue<'a>(&'a AtomicBool, &'a str);
        impl serde::Serialize for CancelAfterFirstValue<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(self.1)?;
                self.0.store(true, Ordering::Relaxed);
                seq.serialize_element(self.1)?;
                panic!("JSON writer continued after cancellation");
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let mut writer = BundleWriter::new(dir.path(), &cancel);
        let payload = "Hillside \"Île\"\n".repeat(8192);
        let value = serde_json::json!({"landmarks": [&payload, "coast"]});
        writer.compact("compact.json", &value).unwrap();
        writer.pretty("pretty.json", &value).unwrap();
        for (name, expected) in [
            ("compact.json", serde_json::to_vec(&value).unwrap()),
            ("pretty.json", serde_json::to_vec_pretty(&value).unwrap()),
        ] {
            assert_eq!(fs::read(dir.path().join(name)).unwrap(), expected);
            assert_eq!(writer.files[name]["bytes"], expected.len());
            assert_eq!(
                writer.files[name]["sha256"],
                format!("{:x}", Sha256::digest(&expected))
            );
        }
        let error = writer
            .compact("cancelled.json", &CancelAfterFirstValue(&cancel, &payload))
            .unwrap_err();
        assert!(error.contains("Generation cancelled"), "{error}");
        assert!(!writer.files.contains_key("cancelled.json"));
    }

    #[test]
    fn overview_png_keeps_rgba_byte_order() {
        use eframe::egui::Color32;

        let image = ColorImage::new(
            [2, 1],
            vec![
                Color32::from_rgb(1, 2, 3),
                Color32::from_rgba_premultiplied(11, 22, 33, 128),
            ],
        );
        let mut png = Vec::new();
        encode_png(&mut png, &image).unwrap();
        let mut reader = png::Decoder::new(io::Cursor::new(png)).read_info().unwrap();
        let mut bytes = [0; 8];
        reader.next_frame(&mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 255, 11, 22, 33, 128]);
    }

    #[test]
    fn independent_bundle_is_repeatable_hash_linked_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let project = Project::default();
        let cancel = AtomicBool::new(false);
        let result = generate(&project, &Options::default(), &cancel, |_, _| {})
            .unwrap()
            .unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let a = save_bundle(&project, &result, &first, &cancel).unwrap();
        let b = save_bundle(&project, &result, &second, &cancel).unwrap();
        assert_eq!(a, b);
        let files = a["files"].as_object().unwrap();
        assert_eq!(fs::read_dir(&first).unwrap().count(), files.len() + 1);
        for (name, receipt) in files {
            let bytes = fs::read(first.join(name)).unwrap();
            assert_eq!(receipt["bytes"], bytes.len());
            assert_eq!(receipt["sha256"], format!("{:x}", Sha256::digest(&bytes)));
        }
        assert_eq!(a["playable"], false);
        assert_eq!(a["height_resolution"], 513);
        assert_eq!(
            std::fs::read(first.join("height.i16le")).unwrap().len(),
            513 * 513 * 2
        );
        let file = File::open(first.join("overview.png")).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(file));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, 513);
        assert!(save_bundle(&project, &result, &first, &cancel).is_err());
        let mut changed = project.clone();
        changed.shuffle_monuments();
        assert!(save_bundle(&changed, &result, &root.path().join("wrong"), &cancel).is_err());
        assert!(!root.path().join("wrong").exists());
        cancel.store(true, Ordering::Relaxed);
        assert!(save_bundle(&project, &result, &root.path().join("cancelled"), &cancel).is_err());
        assert!(!root.path().join("cancelled").exists());
    }
    #[test]
    fn missing_or_contradictory_native_asset_options_are_explicit_errors() {
        let project = Project::default();
        let cancel = AtomicBool::new(false);
        let options = Options {
            catalog: Some("catalog.json".into()),
            ..Default::default()
        };
        assert!(generate(&project, &options, &cancel, |_, _| {}).is_err());
        let options = Options {
            rust_root: Some("Rust".into()),
            ..Default::default()
        };
        assert!(generate(&project, &options, &cancel, |_, _| {}).is_err());
    }

    #[test]
    fn native_asset_failure_does_not_start_terrain_generation() {
        let isolated = tempfile::tempdir().unwrap();
        let project = Project {
            preview_engine: native_preview::VERSION.into(),
            installation: Some(crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: crate::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let options = Options {
            rust_root: Some(isolated.path().to_owned()),
            worker: Some(isolated.path().join("unused-worker.exe")),
            catalog: Some(isolated.path().join("absent-catalog.json")),
            bindings: Some(isolated.path().join("absent-bindings.json")),
            ..Default::default()
        };
        let mut stages = Vec::new();
        let outcome = generate(&project, &options, &AtomicBool::new(false), |label, _| {
            stages.push(label.to_owned());
        });
        assert!(outcome.is_err());
        assert_eq!(stages, ["Verifying monument assets"]);
    }

    #[test]
    fn earthwork_without_assets_fails_before_native_generation() {
        let mut project = Project {
            placement_engine: placement_preview::EARTHWORK_VERSION.into(),
            preview_engine: native_preview::VERSION.into(),
            installation: Some(crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: crate::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let options = Options {
            skip_composition: true,
            ..Default::default()
        };
        let error = generate(&project, &options, &AtomicBool::new(false), |_, _| {
            panic!("Must fail before invoking native processing")
        })
        .err()
        .unwrap();
        assert!(error.contains("Earthwork placement requires composition"));
        project.composition_engine = None;
        assert!(project.validate().is_err());
        project.composition_engine = Some(papyrus_terrain::monument_composition::VERSION.into());
        project.preview_engine = landscape::VERSION.into();
        assert!(project.validate().is_err());
    }

    #[test]
    fn water_aware_bundles_publish_matching_layers_and_legacy_bundles_stay_unprocessed() {
        use papyrus_terrain::{
            monument_composition,
            native_layers::{Layers, dimensions},
        };
        use std::sync::Arc;
        let mut native_profile: crate::native_profile::Profile = serde_json::from_str(
            include_str!("../../../compatibility/native/client-0f703d43.json"),
        )
        .unwrap();
        native_profile.native_sha256 = "a".repeat(64);
        let project = Project {
            native_profile: Some(native_profile),
            size: 1000,
            preview_engine: native_preview::VERSION.into(),
            installation: Some(crate::installation::Installation {
                kind: crate::installation::InstallationKind::Client,
                native: crate::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        };
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![16000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![i32::MIN; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[..cells].fill(255);
        layers.splat[..cells].fill(255);
        let alpha = vec![255; cells];
        let mut composed = monument_composition::compose_with_water(&layers, &alpha, &[], || false)
            .unwrap()
            .unwrap();
        let mut gameplay = layers.height.clone();
        gameplay[0] += 1;
        composed.gameplay_height = Some(papyrus_terrain::gameplay_height::Output {
            height: gameplay,
            report: papyrus_terrain::gameplay_height::Report {
                engine: papyrus_terrain::gameplay_height::VERSION,
                contributors: 1,
                changed_samples: 1,
                work: 1,
                physics_cooking_validated: false,
            },
        });
        let source = native_processing::Source {
            terrain: None,
            native_base: None,
            rivers: None,
            palette: None,
            key: project.terrain_key(),
            image: ColorImage::filled([513, 513], eframe::egui::Color32::GREEN),
            heights: Arc::new(layers.sample_heights(513).unwrap()),
            layers: Arc::new(layers),
        };
        let image = composition::render_overview(&source, &composed).unwrap();
        assert_eq!(
            image.pixels[0],
            crate::map_image::render(
                &composed.layers,
                Some(&composed.alpha),
                composed.ocean.as_ref(),
                513,
                || false
            )
            .unwrap()
            .unwrap()
            .pixels[0]
        );
        let mut generated = Generated {
            recipe: project.clone(),
            image,
            heights: source.heights.as_ref().clone(),
            resolution: 513,
            native: Some(native_processing::Processed {
                raw_source: None,
                source,
                ocean: None,
                reused_source: true,
                ocean_ms: 0.,
                cache_status: "Synthetic test".into(),
            }),
            composed: Some(composed),
            cables: None,
            independent_biomes: None,
            solution: Solution {
                outcomes: vec![],
                required_missing: 0,
                attempts: 0,
            },
            asset_receipts: None,
            warnings: vec!["Synthetic fixture".into()],
        };
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let current = dir.path().join("current");
        let manifest = save_bundle(&project, &generated, &current, &cancel).unwrap();
        assert_eq!(manifest["water"]["ocean_rebuilt_after_composition"], true);
        assert!(manifest.get("cables").is_none());
        generated.cables = Some(std::sync::Arc::new(crate::scene_wires::Scene {
            engine: crate::scene_wires::VERSION,
            source_sha256: "a".repeat(64),
            identities_sha256: "b".repeat(64),
            roots_sha256: "c".repeat(64),
            prefab_roots: 0,
            registration_order: "map-prefab-order/authored-component-order",
            nodes: Vec::new(),
            report: papyrus_terrain::powerline_wires::assemble(&[], Default::default(), || false)
                .unwrap()
                .unwrap(),
        }));
        let cable_dir = dir.path().join("cables");
        let cable_manifest = save_bundle(&project, &generated, &cable_dir, &cancel).unwrap();
        let cable_bytes = std::fs::read(cable_dir.join("cables.json")).unwrap();
        assert_eq!(cable_manifest["cables"]["file"], "cables.json");
        assert_eq!(
            cable_manifest["files"]["cables.json"]["sha256"],
            format!("{:x}", Sha256::digest(&cable_bytes)),
        );
        assert_eq!(cable_manifest["cables"]["game_runtime_validated"], false);
        generated.cables = None;
        assert_eq!(
            std::fs::read(current.join("ocean-classification.u8")).unwrap(),
            generated
                .composed
                .as_ref()
                .unwrap()
                .ocean
                .as_ref()
                .unwrap()
                .classification
        );
        assert_eq!(
            std::fs::metadata(current.join("local-water.i16le"))
                .unwrap()
                .len(),
            (hr * hr * 2) as u64
        );
        assert_eq!(
            std::fs::read(current.join("topology.i32le")).unwrap()[..4],
            (i32::MIN | papyrus_terrain::ocean::OCEAN_BIT).to_le_bytes()
        );
        assert_eq!(manifest["playable"], false);
        let gameplay = std::fs::read(current.join("gameplay-height.i16le")).unwrap();
        assert_eq!(&gameplay[..2], &16001_i16.to_le_bytes());
        assert_eq!(
            &std::fs::read(current.join("height.i16le")).unwrap()[..2],
            &16000_i16.to_le_bytes()
        );
        assert_eq!(
            manifest["gameplay_height"]["other_late_contributors_complete"],
            false
        );
        assert_eq!(
            manifest["files"]["gameplay-height.i16le"]["sha256"],
            format!("{:x}", Sha256::digest(&gameplay))
        );
        assert!(
            generated
                .native
                .as_ref()
                .unwrap()
                .source
                .layers
                .topology
                .iter()
                .all(|&t| t == i32::MIN)
        );
        let mut legacy = project.clone();
        legacy.composition_engine = Some(monument_composition::VERSION.into());
        generated.recipe = legacy.clone();
        generated.composed = Some(
            monument_composition::compose(
                &generated.native.as_ref().unwrap().source.layers,
                &alpha,
                &[],
                || false,
            )
            .unwrap()
            .unwrap(),
        );
        let legacy_dir = dir.path().join("legacy");
        let manifest = save_bundle(&legacy, &generated, &legacy_dir, &cancel).unwrap();
        assert_eq!(manifest["water"]["ocean_rebuilt_after_composition"], false);
        assert!(!legacy_dir.join("ocean-classification.u8").exists());
        assert!(!legacy_dir.join("local-water.i16le").exists());

        generated.composed = None;
        let base_dir = dir.path().join("base");
        let manifest = save_bundle(&legacy, &generated, &base_dir, &cancel).unwrap();
        assert_eq!(fs::read(base_dir.join("alpha.u8")).unwrap(), alpha);
        assert_eq!(manifest["files"]["alpha.u8"]["bytes"], cells);
    }
}
