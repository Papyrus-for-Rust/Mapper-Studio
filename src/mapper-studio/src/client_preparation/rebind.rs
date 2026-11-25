use super::{Bindings, Catalog, Project, ReadyRoads, Report};
use crate::{AssetIdentity, Presence, installation::Installation, monument_source};
use std::path::Path;

pub struct ReboundRecipe {
    pub project: Project,
    pub catalog: Catalog,
    pub bindings: Bindings,
    pub report: Report,
}

/// Resolve a saved recipe against a different installation before publishing the
/// new identity. The caller can keep the prepared assets for the first job.
pub fn rebind_recipe(
    root: &Path,
    installation: &Installation,
    saved: &Project,
    cancelled: impl Fn() -> bool,
) -> Result<ReboundRecipe, String> {
    saved.validate()?;
    if saved.installation.as_ref() == Some(installation) {
        let (catalog, bindings, report) =
            super::load_for_editor(root, installation, saved, cancelled)?;
        return Ok(ReboundRecipe {
            project: saved.clone(),
            catalog,
            bindings,
            report,
        });
    }
    if saved.topology_edits.is_some() {
        return Err(
            "This recipe contains topology paint tied to its saved Rust installation".into(),
        );
    }

    let mut next = saved.clone();
    next.installation = Some(installation.clone());
    next.native_profile = next
        .native_profile
        .take()
        .filter(|profile| profile.matches(installation).is_ok());
    if crate::native_preview::is_native(&next.preview_engine) {
        crate::native_profile::for_source(installation, next.native_profile.as_ref())?;
    }
    // Discovery uses the new installation without the previous installation's
    // fingerprints. Restore the requested stages onto their verified replacements.
    let discovery = Project {
        monument_assets: None,
        monument_rules: None,
        roads: None,
        rails: None,
        rivers: None,
        decoration: None,
        objects: None,
        ..next.clone()
    };
    let (catalog, bindings, mut report) =
        super::load_for_editor(root, installation, &discovery, &cancelled)?;
    let _progress = crate::loading_progress::begin("Rebinding saved recipe", None);
    for target in &next.targets {
        if target.presence == Presence::Required
            && let Some(path) = &target.prefab_path
            && !catalog.entries.contains_key(path)
        {
            return Err(format!(
                "The selected Rust installation is missing required monument {}",
                target.name
            ));
        }
    }
    if next.has_monument_addons() {
        let addons = crate::monument_addons::resolve(&next)?;
        report
            .identities
            .as_ref()
            .ok_or("Replacement prefab identities are unavailable")?
            .addons
            .check(&addons)?;
    }
    if next.monument_assets.is_some() {
        next.monument_assets = Some(AssetIdentity {
            catalog_sha256: catalog.sha256.clone(),
            bindings_sha256: bindings.sha256.clone(),
        });
    }
    if let Some(rules) = &mut next.monument_rules {
        let prepared = report
            .monuments
            .as_ref()
            .ok_or("Replacement monument rules are unavailable")?;
        rebind_monuments(rules, prepared.source().recipe())?;
    }
    if let Some(roads) = &mut next.roads {
        rebind_roads(
            roads,
            report
                .roads
                .as_ref()
                .ok_or("Replacement roads are unavailable")?,
        )?;
    }
    if let Some(rails) = &mut next.rails {
        let replacement = &report
            .rails
            .as_ref()
            .ok_or("Replacement rail templates are unavailable")?
            .prepared;
        if next
            .roads
            .as_ref()
            .map(|roads| roads.source_sha256.as_str())
            != Some(replacement.source.infrastructure_digest())
        {
            return Err("Replacement rails belong to different road infrastructure".into());
        }
        rails.source_sha256.clone_from(&replacement.source.sha256);
        rails.mesh_sha256.clone_from(&replacement.meshes.sha256);
    }
    if let Some(rivers) = &mut next.rivers {
        let replacement = report
            .rivers
            .as_ref()
            .ok_or("Replacement rivers are unavailable")?;
        rivers.source_sha256.clone_from(&replacement.source_sha256);
        rivers
            .source_document
            .clone_from(&replacement.source_document);
        replace_requested(&mut rivers.objects, &replacement.objects, "river objects")?;
    }
    if let Some(decoration) = &mut next.decoration {
        let replacement = report
            .decoration
            .as_ref()
            .ok_or("Replacement scenery is unavailable")?;
        decoration.source_sha256.clone_from(&replacement.sha256);
    }
    if let Some(objects) = &mut next.objects {
        objects.road_source_sha256 = report
            .road_object_sha256
            .take()
            .ok_or("Replacement road objects are unavailable")?;
        if objects.uniform_cliffs_source_sha256.is_some()
            || objects.coastal_cliffs_source_sha256.is_some()
            || objects.jungle_vines_source_sha256.is_some()
        {
            let cliffs = report
                .uniform_cliffs
                .as_ref()
                .ok_or("Replacement cliff data is unavailable")?;
            replace_requested(
                &mut objects.uniform_cliffs_source_sha256,
                &Some(cliffs.source.clone()),
                "hill cliffs",
            )?;
            replace_requested(
                &mut objects.coastal_cliffs_source_sha256,
                &cliffs.coastal,
                "coastal cliffs",
            )?;
            replace_requested(
                &mut objects.jungle_vines_source_sha256,
                &cliffs.vines,
                "vine trees",
            )?;
        }
    }
    if cancelled() {
        return Err("Recipe rebind cancelled".into());
    }
    next.validate()?;
    Ok(ReboundRecipe {
        project: next,
        catalog,
        bindings,
        report,
    })
}

fn replace_requested<T: Clone>(
    saved: &mut Option<T>,
    replacement: &Option<T>,
    stage: &str,
) -> Result<(), String> {
    if let Some(value) = saved {
        let source = replacement
            .as_ref()
            .ok_or_else(|| format!("Replacement {stage} are unavailable"))?;
        value.clone_from(source);
    }
    Ok(())
}

fn rebind_monuments(
    saved: &mut monument_source::Recipe,
    replacement: monument_source::Recipe,
) -> Result<(), String> {
    saved.source_sha256 = replacement.source_sha256;
    replace_requested(
        &mut saved.coastal_profile,
        &replacement.coastal_profile,
        "coastal placement rules",
    )?;
    replace_requested(
        &mut saved.dungeon_source,
        &replacement.dungeon_source,
        "dungeon modules",
    )?;
    replace_requested(
        &mut saved.offshore_source,
        &replacement.offshore_source,
        "offshore monuments",
    )?;
    if let Some(stages) = &mut saved.stage_rules {
        let replacement = replacement
            .stage_rules
            .ok_or("Replacement monument stages are unavailable")?;
        stages.source_sha256 = replacement.source_sha256;
    }
    Ok(())
}

fn rebind_roads(
    saved: &mut crate::infrastructure::Recipe,
    ready: &ReadyRoads,
) -> Result<(), String> {
    saved.source_sha256.clone_from(&ready.source.sha256);
    if let Some(surface) = &mut saved.surface_sha256 {
        surface.clone_from(&ready.surface);
    }
    for profile in [&mut saved.access_costmap, &mut saved.ring_costmap]
        .into_iter()
        .flatten()
    {
        let replacement = ready
            .joined
            .as_ref()
            .ok_or("Replacement road costs are unavailable")?;
        let maximum_work = profile.maximum_work;
        let sampling = profile.policy.sampling.clone();
        profile.clone_from(replacement);
        profile.maximum_work = maximum_work;
        profile.policy.sampling = sampling;
    }
    if let Some(staged) = &mut saved.staged {
        staged.roadside_source_sha256.clone_from(&ready.roadside);
        if let Some(layout) = &mut staged.layout {
            layout
                .assets_sha256
                .clone_from(&ready.ordered.assets_sha256);
            layout.roles_sha256.clone_from(&ready.ordered.roles_sha256);
        }
    }
    Ok(())
}
