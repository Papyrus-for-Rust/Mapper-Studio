//! Source-bound triangle assembly from final rail-bed paths. No physics cooking
//! or runtime prefab/track registration is performed here.
use crate::{rail_mesh_source, rail_source};
use papyrus_terrain::{native_layers::Layers, path_geometry::Path, path_mesh};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub chunk_repetitions: usize,
    pub spline: bool,
    pub random_scale: f32,
    pub mesh_offset: f32,
    pub normal_smoothing: f32,
    pub snap_to_terrain: bool,
    pub snap_unmarked_endpoints: bool,
    pub scale_width_with_length: bool,
    pub top_aligned: bool,
    pub round_vertices: u32,
    pub ring_start_flag: bool,
    pub ring_end_flag: bool,
    pub approach_start_flag: bool,
    pub approach_end_flag: bool,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != "papyrus-rail-mesh-stage-1"
            || !(1..=32).contains(&self.chunk_repetitions)
            || !self.spline
            || self.random_scale != 1.
            || !self.mesh_offset.is_finite()
            || self.mesh_offset.abs() > 32.
            || self.normal_smoothing != 0.
            || self.snap_to_terrain
            || self.scale_width_with_length
            || self.round_vertices != 0
        {
            return Err("Unsupported rail mesh stage policy".into());
        }
        Ok(())
    }
    fn settings(&self, width: f32, work: usize, track: &Track<'_>) -> path_mesh::Settings {
        path_mesh::Settings {
            width,
            chunk_repetitions: self.chunk_repetitions,
            maximum_work: work,
            snap_start: self.snap_unmarked_endpoints
                && !track.path.is_circular()
                && !track.source_start_flag,
            snap_end: self.snap_unmarked_endpoints
                && !track.path.is_circular()
                && !track.source_end_flag,
            mesh_offset: self.mesh_offset,
            top_aligned: self.top_aligned,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_work: usize,
    pub maximum_chunks: usize,
    pub maximum_geometry_bytes: u64,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=32000000).contains(&self.maximum_work)
            || !(1..=8192).contains(&self.maximum_chunks)
            || !(1..=512 * 1024 * 1024).contains(&self.maximum_geometry_bytes)
        {
            return Err("Invalid rail mesh stage allowances".into());
        }
        Ok(())
    }
}
pub struct Track<'a> {
    pub id: u64,
    pub path: &'a Path,
    pub source_start_flag: bool,
    pub source_end_flag: bool,
}
pub use papyrus_terrain::rail_scene::Chunk as PlacedChunk;
pub struct Output {
    pub report: Value,
    pub chunks: Vec<PlacedChunk>,
}

/// Late AddToHeightMap contribution from the generated collision LOD only.
/// Preserves terrain and does not emulate PhysX mesh cooking.
pub fn gameplay_height(
    layers: &Layers,
    output: &Output,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<papyrus_terrain::gameplay_height::Output>, String> {
    use papyrus_terrain::{decor_collider::Contributor, height_mesh::Mesh};
    if cancelled() {
        return Ok(None);
    }
    if output.report["collision_lod"] != 0 || output.chunks.len() > 8192 {
        return Err("Unsupported rail collision LOD or geometry roster".into());
    }
    let tracks = output.report["tracks"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 257)
        .ok_or("Missing rail collision track roster")?;
    let mut expected = BTreeSet::new();
    let mut track_ids = BTreeSet::new();
    for track in tracks {
        let id = track["track_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or("Invalid rail collision track ID")?;
        let count = track["plan"]["chunks"]
            .as_u64()
            .filter(|c| (1..=8192).contains(c))
            .ok_or("Invalid rail collision chunk count")?;
        if !track_ids.insert(id) {
            return Err("Duplicate rail collision track ID".into());
        }
        for index in 0..count {
            if expected.len() >= 4096 {
                return Err("Rail collision contributor limit reached".into());
            }
            expected.insert((id, index as usize));
        }
    }
    let mut seen = BTreeSet::new();
    let mut contributors = Vec::with_capacity(expected.len());
    for chunk in &output.chunks {
        if cancelled() {
            return Ok(None);
        }
        if !track_ids.contains(&chunk.track_id) {
            return Err("Rail geometry references an unknown track".into());
        }
        if chunk.lod != 0 {
            continue;
        }
        let id = (chunk.track_id, chunk.chunk_index);
        if !expected.contains(&id) || !seen.insert(id) {
            return Err("Duplicate or unexpected rail collision chunk".into());
        }
        chunk.geometry.mesh.validate()?;
        let origin = chunk.geometry.origin;
        contributors.push(Contributor {
            mesh: Mesh {
                kind: "raw-triangle-mesh".into(),
                enabled: true,
                trigger: false,
                convex: false,
                cooking_options: 0,
                backfaces: false,
                vertices: chunk.geometry.mesh.vertices.clone(),
                triangles: chunk.geometry.mesh.triangles.clone(),
            },
            local_to_root: [
                [1., 0., 0., origin[0]],
                [0., 1., 0., origin[1]],
                [0., 0., 1., origin[2]],
                [0., 0., 0., 1.],
            ],
        });
    }
    if seen != expected {
        return Err("Incomplete rail collision chunk roster".into());
    }
    papyrus_terrain::gameplay_height::generate(
        &layers.height,
        layers.height_resolution,
        layers.size as f32,
        &contributors,
        maximum_work,
        cancelled,
    )
}

pub fn build(
    rail: &rail_source::Source,
    meshes: &rail_mesh_source::Source,
    layers: &Layers,
    tracks: &[Track<'_>],
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    let policy = rail
        .profile
        .mesh
        .as_ref()
        .ok_or("This rail source snapshot has no mesh-stage adapter; prepare a new snapshot")?;
    let stage = rail
        .stages
        .iter()
        .find(|s| s.kind == rail_source::Kind::GenerateRailMeshes)
        .ok_or("Source has no rail mesh generation stage")?;
    if let Some(reason) = rail.skip(stage, layers.size, false, true) {
        return Err(format!("Source rail mesh stage is not selected: {reason}"));
    }
    if meshes.parent_digest() != rail.sha256 {
        return Err("Rail mesh templates differ from bed source".into());
    }
    let Some(mut output) = assemble(
        &meshes.meshes.iter().map(|m| &m.buffer).collect::<Vec<_>>(),
        layers,
        tracks,
        policy,
        rail.profile.mesh_width(),
        settings,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    output.report["source_sha256"] = json!(rail.sha256);
    output.report["mesh_source_sha256"] = json!(meshes.sha256);
    Ok(Some(output))
}
fn assemble(
    templates: &[&path_mesh::Template],
    layers: &Layers,
    tracks: &[Track<'_>],
    policy: &Policy,
    width: f32,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    policy.validate()?;
    settings.validate()?;
    let (hr, _) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if layers.height_resolution != hr
        || layers.height.len() != hr * hr
        || !(1..=257).contains(&tracks.len())
        || !(1..=16).contains(&templates.len())
    {
        return Err("Invalid final rail mesh inputs".into());
    }
    let mut ids = BTreeSet::new();
    let mut total_work = 0usize;
    let mut total_chunks = 0usize;
    let mut bytes = 0u64;
    let mut schedules = Vec::new();
    for template in templates {
        template.validate()?;
    }
    let last = templates[templates.len() - 1];
    for track in tracks {
        if track.id == 0 || !ids.insert(track.id) {
            return Err("Invalid or duplicate final rail mesh track ID".into());
        }
        let cfg = policy.settings(width, settings.maximum_work, track);
        let plan = path_mesh::Plan::new(track.path, last, cfg)
            .map_err(|e| format!("Final rail mesh planning failed: {e:?}"))?;
        for template in templates {
            let vertices = plan
                .repetitions
                .checked_mul(template.vertices.len())
                .ok_or("Rail mesh vertex overflow")?;
            let faces = plan
                .repetitions
                .checked_mul(template.triangles.len())
                .ok_or("Rail mesh index overflow")?;
            total_work = total_work
                .checked_add(vertices)
                .and_then(|v| v.checked_add(faces))
                .ok_or("Rail mesh work overflow")?;
            total_chunks = total_chunks
                .checked_add(plan.chunks)
                .ok_or("Rail mesh chunk overflow")?;
            bytes = bytes
                .checked_add(vertices as u64 * 48 + faces as u64 * 12)
                .ok_or("Rail mesh byte overflow")?;
        }
        schedules.push(plan);
    }
    if total_work > settings.maximum_work
        || total_chunks > settings.maximum_chunks
        || bytes > settings.maximum_geometry_bytes
    {
        return Err("Final rail mesh stage work/chunk/memory allowance exceeded".into());
    }
    // All resource and total-size checks finish before allocating any mesh chunks.
    let mut chunks = Vec::with_capacity(total_chunks);
    let mut work = 0;
    let mut entries = Vec::new();
    let height = |x: f32, z: f32| {
        let u = x / layers.size as f32 + 0.5;
        let v = z / layers.size as f32 + 0.5;
        if !u.is_finite() || !v.is_finite() || !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v)
        {
            return None;
        }
        let x = u * (hr - 1) as f32;
        let z = v * (hr - 1) as f32;
        let a = x.floor() as usize;
        let b = z.floor() as usize;
        let c = (a + 1).min(hr - 1);
        let d = (b + 1).min(hr - 1);
        let at = |x, z| papyrus_terrain::erosion::decode(layers.height[z * hr + x]);
        let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
        let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
        Some(lo * (1. - z.fract()) + hi * z.fract())
    };
    for (track, plan) in tracks.iter().zip(schedules) {
        let cfg = policy.settings(width, settings.maximum_work, track);
        entries.push(
            json!({"track_id":track.id,"path_length":track.path.length(),"plan":plan,
            "source_start_flag":track.source_start_flag,"source_end_flag":track.source_end_flag,
            "snap_start":cfg.snap_start,"snap_end":cfg.snap_end}),
        );
        for lod in 0..templates.len() {
            for index in 0..plan.chunks {
                let mut cfg = cfg;
                cfg.maximum_work = settings.maximum_work - work;
                let Some(geometry) = path_mesh::deform_chunk(
                    track.path, templates, lod, cfg, index, height, &cancelled,
                )
                .map_err(|e| format!("Final rail mesh deformation failed: {e:?}"))?
                else {
                    return Ok(None);
                };
                work += geometry.work;
                chunks.push(PlacedChunk {
                    track_id: track.id,
                    lod,
                    chunk_index: index,
                    geometry,
                });
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let files=chunks.iter().map(|c|json!({"file":c.filename(),"track_id":c.track_id,"lod":c.lod,"chunk_index":c.chunk_index,
        "origin":c.geometry.origin,"vertices":c.geometry.mesh.vertices.len(),"triangles":c.geometry.mesh.triangles.len()})).collect::<Vec<_>>();
    Ok(Some(Output {
        report: json!({"engine":policy.engine,"policy":policy,"settings":settings,"tracks":entries,
        "lods":templates.len(),"collision_lod":0,"deformation_bounds_lod":templates.len()-1,"files":files,
        "chunks":chunks.len(),"work":work,"geometry_buffer_bytes":bytes,"path_source":"final fitted bed controls (no resmoothing)",
        "source_stage_policy_bound":true,"endpoint_height_sampling":"Papyrus bilinear (n-1) grid; deep sea unsupported",
        "raw_collision_contributors_applied":false,"physics_cooking_validated":false,"runtime_assembly_complete":false}),
        chunks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layers() -> Layers {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17203; hr * hr],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn template() -> path_mesh::Template {
        path_mesh::Template {
            vertices: vec![[-1., 0., 0.], [1., 1., 0.], [-1., 0., 2.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., -1.]; 3],
            uv: vec![[0., 0.], [1., 0.], [0., 1.]],
            triangles: vec![[0, 1, 2]],
            center: [0., 0.5, 1.],
            extents: [1., 0.5, 1.],
        }
    }
    fn policy() -> Policy {
        Policy {
            engine: "papyrus-rail-mesh-stage-1".into(),
            chunk_repetitions: 5,
            spline: true,
            random_scale: 1.,
            mesh_offset: 0.,
            normal_smoothing: 0.,
            snap_to_terrain: false,
            snap_unmarked_endpoints: true,
            scale_width_with_length: false,
            top_aligned: false,
            round_vertices: 0,
            ring_start_flag: false,
            ring_end_flag: false,
            approach_start_flag: true,
            approach_end_flag: false,
        }
    }
    fn settings() -> Settings {
        Settings {
            maximum_work: 1000,
            maximum_chunks: 100,
            maximum_geometry_bytes: 100000,
        }
    }
    #[test]
    fn collision_height_uses_complete_lod_zero_roster_without_changing_terrain() {
        let layers = layers();
        let before = layers.height.clone();
        let mut low = template();
        low.triangles[0] = [0, 2, 1];
        let mut high = template();
        high.triangles[0] = [0, 2, 1];
        // Preserve the shared last-LOD minimum Y while lifting a visual vertex.
        // Every LOD is deformed using those shared bounds.
        high.vertices[1][1] = 51.;
        high.center[1] = 25.5;
        high.extents[1] = 25.5;
        let path = Path::from_controls(&[[0., 40., 0.], [0., 40., 20.]], 1000, || false)
            .unwrap()
            .unwrap();
        let tracks = [Track {
            id: 7,
            path: &path,
            source_start_flag: true,
            source_end_flag: true,
        }];
        let mut output = assemble(
            &[&low, &high],
            &layers,
            &tracks,
            &policy(),
            2.,
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        let height = gameplay_height(&layers, &output, 100000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(height.report.contributors, 2);
        assert!(height.report.changed_samples > 0);
        assert!(
            height
                .height
                .iter()
                .all(|h| papyrus_terrain::erosion::decode(*h) < 42.)
        );
        assert_eq!(layers.height, before);
        assert!(gameplay_height(&layers, &output, 1, || false).is_err());
        assert!(
            gameplay_height(&layers, &output, 100000, || true)
                .unwrap()
                .is_none()
        );
        output
            .chunks
            .retain(|c| !(c.lod == 0 && c.chunk_index == 1));
        assert!(gameplay_height(&layers, &output, 100000, || false).is_err());
        assert_eq!(layers.height, before);
    }
    #[test]
    fn final_paths_generate_all_lods_with_terrain_snap_and_immutable_inputs() {
        let layers = layers();
        let before = layers.height.clone();
        let a = template();
        let b = template();
        let path = Path::from_controls(&[[0., 10., 0.], [0., 10., 20.]], 1000, || false)
            .unwrap()
            .unwrap();
        let points = path.points().to_vec();
        let tracks = [Track {
            id: 7,
            path: &path,
            source_start_flag: false,
            source_end_flag: true,
        }];
        let output = assemble(
            &[&a, &b],
            &layers,
            &tracks,
            &policy(),
            2.,
            settings(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.chunks.len(), 4);
        assert_eq!(output.report["work"], 80);
        assert_eq!(output.report["geometry_buffer_bytes"], 3120);
        assert_eq!(output.report["tracks"][0]["snap_start"], true);
        assert_eq!(output.report["tracks"][0]["snap_end"], false);
        let first = &output.chunks[0].geometry;
        assert!(
            (first.mesh.vertices[0][1] + first.origin[1]
                - papyrus_terrain::erosion::decode(layers.height[0]))
            .abs()
                < 0.0001
        );
        assert_eq!(layers.height, before);
        assert_eq!(path.points(), points);
        assert_eq!(output.report["raw_collision_contributors_applied"], false);
        assert_eq!(output.report["physics_cooking_validated"], false);
        let names = output
            .chunks
            .iter()
            .map(PlacedChunk::filename)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 4);
    }
    #[test]
    fn budgets_unsupported_policies_and_cancellation_cannot_publish_partial_geometry() {
        let layers = layers();
        let a = template();
        let path = Path::from_controls(&[[0., 10., 0.], [0., 10., 20.]], 1000, || false)
            .unwrap()
            .unwrap();
        let tracks = [Track {
            id: 1,
            path: &path,
            source_start_flag: true,
            source_end_flag: true,
        }];
        for invalid in 0..3 {
            let mut cfg = settings();
            match invalid {
                0 => cfg.maximum_work = 1,
                1 => cfg.maximum_chunks = 1,
                _ => cfg.maximum_geometry_bytes = 1,
            };
            assert!(assemble(&[&a], &layers, &tracks, &policy(), 2., cfg, || false).is_err());
        }
        for invalid in 0..6 {
            let mut p = policy();
            match invalid {
                0 => p.random_scale = 0.5,
                1 => p.normal_smoothing = 0.5,
                2 => p.snap_to_terrain = true,
                3 => p.spline = false,
                4 => p.round_vertices = 2,
                _ => p.chunk_repetitions = 0,
            };
            assert!(p.validate().is_err());
        }
        let calls = std::cell::Cell::new(0);
        assert!(
            assemble(&[&a], &layers, &tracks, &policy(), 2., settings(), || {
                calls.set(calls.get() + 1);
                calls.get() > 12
            })
            .unwrap()
            .is_none()
        );
    }
}
