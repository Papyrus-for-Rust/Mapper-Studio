//! Selected-installation late rail material painting. Not a complete rail stage.
use crate::{
    infrastructure, rail_source, retained_rail_height::RetainedRails,
    road_surface_stage::HeightGeometry,
};
use papyrus_terrain::{native_layers::Layers, path_geometry::Path, path_surface, rail_texture};
use serde::Serialize;
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub paths: crate::retained_rail_height::Receipt,
    pub height_geometry: [f32; 2],
    pub texture: rail_texture::Report,
    pub work: usize,
    pub runtime_validated: bool,
}
pub struct Output {
    pub splat: Vec<u8>,
    pub report: Report,
}
pub fn paint(
    source: &rail_source::Source,
    parent: &infrastructure::Source,
    layers: &Layers,
    paths: &[Path],
    geometry: HeightGeometry,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    if cancelled() {
        return Ok(None);
    }
    if !(1..=512_000_000).contains(&maximum_work) {
        return Err("Invalid rail texture work grant".into());
    }
    let (_, n) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if layers.surface_resolution != n {
        return Err("Rail texture surface dimensions differ".into());
    }
    let stage = source
        .stages
        .iter()
        .find(|s| s.kind == rail_source::Kind::GenerateRailTexture)
        .ok_or("Missing rail texture stage")?;
    if let Some(reason) = source.skip(stage, layers.size, false, true) {
        return Err(format!("Rail texture stage unavailable: {reason}"));
    }
    let Some(retained) = RetainedRails::prepare(
        source,
        parent,
        paths,
        maximum_work.min(32_000_000),
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let shape = source.profile.bed_shape()?;
    let bed = source
        .profile
        .bed
        .as_ref()
        .ok_or("Missing captured rail sampling policy")?;
    let policy = rail_texture::Policy {
        width: shape.width,
        padding: [shape.inner_padding, shape.outer_padding],
        sample_spacing: bed.sample_spacing,
        random_scale: source
            .profile
            .mesh
            .as_ref()
            .ok_or("Missing rail radius policy")?
            .random_scale,
        splat_channel: shape.splat_channel,
    };
    let remaining = maximum_work
        .checked_sub(retained.receipt().preparation_work)
        .filter(|&v| v > 0)
        .ok_or("Rail texture binding work exhausted")?;
    let tracks = paths.iter().collect::<Vec<_>>();
    let Some(result) = rail_texture::paint(
        path_surface::Grid {
            size: layers.size as f32,
            resolution: n,
            origin_y: geometry.origin_y,
            vertical_span: geometry.vertical_span,
        },
        &layers.splat,
        &tracks,
        policy,
        remaining,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let work = result.report.work + retained.receipt().preparation_work;
    Ok(Some(Output {
        splat: result.splat,
        report: Report {
            engine: "papyrus-source-rail-texture-stage-1",
            paths: retained.receipt().clone(),
            height_geometry: [geometry.origin_y, geometry.vertical_span],
            texture: result.report,
            work,
            runtime_validated: false,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_texture_binds_stage_and_parent_without_modifying_other_layers() {
        let (_root, mut parent, mut source) = crate::retained_rail_height::tests::source_fixture();
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = n * n;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16000; hr * hr],
            biome: vec![],
            topology: vec![0; cells],
            splat: vec![0; cells * 8],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.splat[..cells].fill(255);
        let paths = [Path::from_controls(
            &[[-100., 20., 0.], [0., 20., 0.], [100., 20., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let geometry = HeightGeometry {
            origin_y: -500.,
            vertical_span: 1000.,
        };
        let original = (
            layers.height.clone(),
            layers.topology.clone(),
            layers.splat.clone(),
        );
        let result = paint(
            &source,
            &parent,
            &layers,
            &paths,
            geometry,
            4_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(result.report.texture.changed_cells > 0);
        assert_eq!(result.report.paths.source_sha256, source.sha256);
        assert_eq!(
            (&layers.height, &layers.topology, &layers.splat),
            (&original.0, &original.1, &original.2)
        );
        assert!(result.report.work <= 4_000_000);
        source
            .stages
            .iter_mut()
            .find(|s| s.kind == rail_source::Kind::GenerateRailTexture)
            .unwrap()
            .disabled = Some(true);
        assert!(
            paint(
                &source,
                &parent,
                &layers,
                &paths,
                geometry,
                4_000_000,
                || false
            )
            .is_err()
        );
        source
            .stages
            .iter_mut()
            .find(|s| s.kind == rail_source::Kind::GenerateRailTexture)
            .unwrap()
            .disabled = Some(false);
        parent.sha256 = "f".repeat(64);
        assert!(
            paint(
                &source,
                &parent,
                &layers,
                &paths,
                geometry,
                4_000_000,
                || false
            )
            .is_err()
        );
    }
}
