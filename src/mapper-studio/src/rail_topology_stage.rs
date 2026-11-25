//! Installation-bound rail topology after roadside stamps and road topology.
use crate::{
    infrastructure, rail_source, retained_rail_height::RetainedRails,
    road_surface_stage::HeightGeometry,
};
use papyrus_terrain::{
    native_layers::Layers, path_geometry::Path, path_surface, rail_topology, road_topology,
};
use serde::Serialize;
#[derive(Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub paths: crate::retained_rail_height::Receipt,
    pub height_geometry: [f32; 2],
    pub topology: rail_topology::Report,
    pub work: usize,
    pub runtime_validated: bool,
}

pub struct Output {
    pub topology: Vec<i32>,
    pub report: Report,
}
/// Captured stage policy and immutable curves, reused across private road trials.
pub struct Prepared<'a> {
    retained: RetainedRails<'a>,
    policy: rail_topology::Policy,
    size: u32,
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
    let Some(prepared) = prepare(source, parent, paths, layers.size, maximum_work, &cancelled)?
    else {
        return Ok(None);
    };
    prepared.apply(layers, &layers.topology, geometry, maximum_work, cancelled)
}
pub fn prepare<'a>(
    source: &rail_source::Source,
    parent: &infrastructure::Source,
    paths: &'a [Path],
    size: u32,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared<'a>>, String> {
    if cancelled() {
        return Ok(None);
    }
    if !(1..=512_000_000).contains(&maximum_work) {
        return Err("Invalid rail topology work grant".into());
    }
    let stage = source
        .stages
        .iter()
        .find(|s| s.kind == rail_source::Kind::GenerateRailTopology)
        .ok_or("Missing rail topology stage")?;
    if let Some(reason) = source.skip(stage, size, false, true) {
        return Err(format!("Rail topology stage unavailable: {reason}"));
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
        .ok_or("Missing rail topology sampling policy")?;
    let policy = rail_topology::Policy {
        width: shape.width,
        padding: [shape.inner_padding, shape.outer_padding],
        sample_spacing: bed.sample_spacing,
        random_scale: source
            .profile
            .mesh
            .as_ref()
            .ok_or("Missing rail radius policy")?
            .random_scale,
        rail_flag: shape.rail as i32,
        opacity_threshold: bed.topology_threshold,
        marking: road_topology::Policy {
            source_mask: (shape.rail | shape.railside) as i32,
            eligible_mask: shape.eligible_railside as i32,
            roadside_flag: shape.railside as i32,
            cliff_flag: shape.cliff as i32,
            radius_cells: bed.railside_radius,
            cliff_slope_degrees: bed.cliff_slope,
            normal_sampling: None,
        },
    };
    policy.validate()?;
    papyrus_terrain::native_layers::dimensions(size)?;
    Ok((!cancelled()).then_some(Prepared {
        retained,
        policy,
        size,
    }))
}
impl Prepared<'_> {
    pub fn retained(&self) -> &RetainedRails<'_> {
        &self.retained
    }
    /// Bind a final-surface request to the exact curves in its checked scene.
    pub fn check_scene(
        &self,
        scene: &papyrus_terrain::rail_scene::Scene,
        recipe: &crate::rail_generation::Recipe,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<usize>, String> {
        if cancelled() {
            return Ok(None);
        }
        let expected = &self.retained;
        if scene.report["changes"]["surface_paint_deferred"] != true {
            return Err("Final rail topology requires earthwork with deferred surfaces".into());
        }
        if expected.receipt().source_sha256 != recipe.source_sha256
            || expected.paths().len() != scene.tracks.len()
        {
            return Err("Final rail topology belongs to a different source or roster".into());
        }
        let mut work = 0usize;
        for (path, track) in expected.paths().iter().zip(&scene.tracks) {
            if cancelled() {
                return Ok(None);
            }
            work = work
                .checked_add(1)
                .filter(|&w| w <= maximum_work)
                .ok_or("Rail scene binding allowance exhausted")?;
            let (a, b) = (path.path, &track.path);
            if a.points().len() != b.points().len()
                || a.tangents().len() != b.tangents().len()
                || a.length().to_bits() != b.length().to_bits()
                || a.step_size().to_bits() != b.step_size().to_bits()
                || a.range() != b.range()
                || a.is_circular() != b.is_circular()
            {
                return Err("Final rail topology curve revision changed".into());
            }
            for (a, b) in a
                .points()
                .iter()
                .chain(a.tangents())
                .zip(b.points().iter().chain(b.tangents()))
            {
                if cancelled() {
                    return Ok(None);
                }
                work = work
                    .checked_add(3)
                    .filter(|&w| w <= maximum_work)
                    .ok_or("Rail scene binding allowance exhausted")?;
                if a.iter().zip(b).any(|(a, b)| a.to_bits() != b.to_bits()) {
                    return Err("Final rail topology curve data changed".into());
                }
            }
        }
        Ok(Some(work))
    }
    /// Paint over the current road-topology proposal, borrowing pre-road height.
    /// The grant/report conservatively include binding work on each application.
    pub fn apply(
        &self,
        layers: &Layers,
        topology: &[i32],
        geometry: HeightGeometry,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Output>, String> {
        if cancelled() {
            return Ok(None);
        }
        if layers.size != self.size || !(1..=512_000_000).contains(&maximum_work) {
            return Err("Retained rail topology grant or map size changed".into());
        }
        let remaining = maximum_work
            .checked_sub(self.retained.receipt().preparation_work)
            .filter(|&w| w > 0)
            .ok_or("Rail topology binding work exhausted")?;
        let tracks = self
            .retained
            .paths()
            .iter()
            .map(|p| p.path)
            .collect::<Vec<_>>();
        let Some(output) = rail_topology::paint(
            rail_topology::Input {
                layers,
                height: &layers.height,
                topology,
                tracks: &tracks,
                geometry: path_surface::Grid {
                    size: layers.size as f32,
                    resolution: layers.surface_resolution,
                    origin_y: geometry.origin_y,
                    vertical_span: geometry.vertical_span,
                },
            },
            self.policy,
            remaining,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let work = self.retained.receipt().preparation_work + output.report.work;
        Ok(Some(Output {
            topology: output.topology,
            report: Report {
                engine: "papyrus-source-rail-topology-stage-1",
                paths: self.retained.receipt().clone(),
                height_geometry: [geometry.origin_y, geometry.vertical_span],
                topology: output.report,
                work,
                runtime_validated: false,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_topology_stage_binds_parent_policy_and_private_output() {
        let (_root, mut parent, mut source) = crate::retained_rail_height::tests::source_fixture();
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![17000; hr * hr],
            topology: vec![1; n * n],
            splat: vec![0; n * n * 8],
            biome: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
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
        let result = paint(
            &source,
            &parent,
            &layers,
            &paths,
            geometry,
            32_000_000,
            || false,
        )
        .unwrap()
        .unwrap();
        let captured = source.profile.bed.as_ref().unwrap();
        assert_eq!(
            result.report.topology.policy.opacity_threshold,
            captured.topology_threshold
        );
        assert_eq!(
            result.report.topology.policy.marking.radius_cells,
            captured.railside_radius
        );
        assert_eq!(
            result.report.topology.policy.marking.cliff_slope_degrees,
            captured.cliff_slope
        );
        assert!(result.report.topology.changed_cells > 0);
        assert_eq!(
            result.report.work,
            result.report.paths.preparation_work + result.report.topology.work
        );
        assert!(layers.topology.iter().all(|&v| v == 1));
        assert!(layers.height.iter().all(|&v| v == 17000));
        assert!(layers.splat.iter().all(|&v| v == 0));
        assert!(!result.report.runtime_validated);
        source
            .stages
            .iter_mut()
            .find(|s| s.kind == rail_source::Kind::GenerateRailTopology)
            .unwrap()
            .disabled = Some(true);
        assert!(
            paint(
                &source,
                &parent,
                &layers,
                &paths,
                geometry,
                32_000_000,
                || false
            )
            .is_err()
        );
        source
            .stages
            .iter_mut()
            .find(|s| s.kind == rail_source::Kind::GenerateRailTopology)
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
                32_000_000,
                || false
            )
            .is_err()
        );
        assert!(
            paint(&source, &parent, &layers, &paths, geometry, 0, || true)
                .unwrap()
                .is_none()
        );
    }
}
