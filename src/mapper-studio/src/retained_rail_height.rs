//! Borrowed source-bound rail inputs for the later road-terrain sweeps.
//! This does not plan tracks or certify a combined road/rail world.
use crate::{infrastructure, rail_source};
use papyrus_terrain::{path_geometry::Path, path_terrain::Shape, road_terrain::InputPath};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    pub engine: &'static str,
    pub source_sha256: String,
    pub infrastructure_sha256: String,
    pub controls_sha256: String,
    pub tracks: usize,
    pub controls: usize,
    pub preparation_work: usize,
    pub complete_world_validated: bool,
}
pub struct RetainedRails<'a> {
    paths: Vec<InputPath<'a>>,
    receipt: Receipt,
}
/// Bilinear height dependencies below the retained rail driving strip. These
/// are bound to one immutable terrain revision, not to the wider bed fade.
pub struct RetainedHeight {
    mask: papyrus_terrain::height_reservations::HeightReservations,
    samples: Vec<usize>,
    values: Vec<i16>,
    pub preparation_work: usize,
}
impl RetainedHeight {
    #[cfg(feature = "road-diagnostics")]
    pub(crate) fn captured_samples(&self) -> Vec<(usize, i16)> {
        self.samples
            .iter()
            .copied()
            .zip(self.values.iter().copied())
            .collect()
    }
    #[cfg(feature = "road-diagnostics")]
    pub(crate) fn from_capture(
        grid: papyrus_terrain::path_terrain::Grid<'_>,
        samples: &[(usize, i16)],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        if samples.len() > 262144
            || samples.windows(2).any(|s| s[0].0 >= s[1].0)
            || samples.iter().any(|&(i, v)| grid.height.get(i) != Some(&v))
        {
            return Err("Invalid retained height capture samples or original values".into());
        }
        let Some(mut mask) =
            papyrus_terrain::height_reservations::HeightReservations::new(grid, &cancelled)?
        else {
            return Ok(None);
        };
        for &(i, _) in samples {
            if cancelled() {
                return Ok(None);
            };
            mask.reserve(i)?;
        }
        Ok(Some(Self {
            mask,
            samples: samples.iter().map(|s| s.0).collect(),
            values: samples.iter().map(|s| s.1).collect(),
            preparation_work: 0,
        }))
    }
    #[cfg(test)]
    pub(crate) fn test_capture(
        grid: papyrus_terrain::path_terrain::Grid<'_>,
        samples: &[usize],
    ) -> Self {
        let mut mask = papyrus_terrain::height_reservations::HeightReservations::new(grid, || false)
            .unwrap()
            .unwrap();
        let mut samples = samples.to_vec();
        samples.sort_unstable();
        samples.dedup();
        for &i in &samples {
            mask.reserve(i).unwrap();
        }
        let values = samples.iter().map(|&i| grid.height[i]).collect();
        Self {
            mask,
            samples,
            values,
            preparation_work: 0,
        }
    }
    pub fn samples(&self) -> &[usize] {
        &self.samples
    }
    pub fn validate(
        &self,
        layers: &papyrus_terrain::native_layers::Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        self.mask.validate_binding(layers, cancelled)
    }
    /// Later road rounds may change unreserved terrain. Verify the retained
    /// values against their original baseline, rather than rebinding to each
    /// new proposal and accidentally accepting drift under the rails.
    pub fn validate_retained_grid(
        &self,
        grid: papyrus_terrain::path_terrain::Grid<'_>,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if !self.mask.layout_matches(grid) || grid.height.len() != grid.resolution * grid.resolution
        {
            return Err("Foreign retained rail fitting grid".into());
        }
        for (position, (&i, &value)) in self.samples.iter().zip(&self.values).enumerate() {
            if position % 4096 == 0 && cancelled() {
                return Ok(false);
            }
            if grid.height[i] != value {
                return Err("Retained rail height drifted during road fitting".into());
            }
        }
        Ok(!cancelled())
    }
    pub fn fit_rebuild(
        &self,
        input: papyrus_terrain::road_bed::Input<'_>,
        mut settings: papyrus_terrain::road_bed::Settings,
        probes: &[papyrus_terrain::road_bed::ClearanceProbe],
        pass: papyrus_terrain::road_bed::RebuildPass<'_>,
        progress: impl FnMut(papyrus_terrain::road_bed::Progress),
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<papyrus_terrain::road_bed::RebuildCandidate>, String> {
        if cancelled() {
            return Ok(None);
        }
        settings
            .validate()
            .map_err(|e| format!("Retained road-bed settings: {e:?}"))?;
        let checks = self
            .samples
            .len()
            .checked_mul(2)
            .ok_or("Retained road-bed validation work overflow")?;
        settings.maximum_work = settings
            .maximum_work
            .checked_sub(checks)
            .filter(|&v| v > 0)
            .ok_or("Retained road-bed validation work exhausted")?;
        if !self.validate_retained_grid(input.grid, &cancelled)? {
            return Ok(None);
        }
        match papyrus_terrain::road_bed::fit_retaining_for_rebuild(
            input,
            settings,
            probes,
            pass,
            &self.samples,
            progress,
            &cancelled,
        ) {
            Ok(mut output) => {
                if !self.validate_retained_grid(
                    papyrus_terrain::path_terrain::Grid {
                        height: &output.height,
                        ..input.grid
                    },
                    &cancelled,
                )? {
                    return Ok(None);
                }
                output.report.work += checks;
                output.report.settings.maximum_work += checks;
                Ok(Some(output))
            }
            Err(papyrus_terrain::road_bed::Failure::Cancelled) => Ok(None),
            Err(error) => Err(format!("Retained road mesh bed: {error:?}")),
        }
    }
    /// Bind the retained strip to the exact initial fit grid, then share one
    /// work grant across binding and all coupled/quantized control repairs.
    pub fn fit_initial_bed(
        &self,
        input: papyrus_terrain::road_grade_terrain::Input<'_>,
        maximum_grade: f32,
        maximum_cut_fill: f32,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<papyrus_terrain::road_grade_terrain::Output>, String> {
        self.fit_initial(
            input,
            maximum_grade,
            maximum_cut_fill,
            maximum_work,
            true,
            cancelled,
        )
    }
    pub fn fit_pinned_initial_bed(
        &self,
        input: papyrus_terrain::road_grade_terrain::Input<'_>,
        maximum_grade: f32,
        maximum_cut_fill: f32,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<papyrus_terrain::road_grade_terrain::Output>, String> {
        self.fit_initial(
            input,
            maximum_grade,
            maximum_cut_fill,
            maximum_work,
            false,
            cancelled,
        )
    }
    fn fit_initial(
        &self,
        input: papyrus_terrain::road_grade_terrain::Input<'_>,
        maximum_grade: f32,
        maximum_cut_fill: f32,
        maximum_work: usize,
        coupled: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<papyrus_terrain::road_grade_terrain::Output>, String> {
        if cancelled() {
            return Ok(None);
        }
        if !(1..=512_000_000).contains(&maximum_work) {
            return Err("Invalid retained road-bed work grant".into());
        }
        let binding_work = input.grid.height.len();
        let remaining = maximum_work
            .checked_sub(binding_work)
            .filter(|&v| v > 0)
            .ok_or("Retained road-bed binding work exhausted")?;
        if !self.mask.validate_grid_binding(input.grid, &cancelled)? {
            return Ok(None);
        }
        let result = if coupled {
            papyrus_terrain::road_grade_terrain::correct_coupled_retaining_samples(
                input,
                &self.samples,
                maximum_grade,
                maximum_cut_fill,
                remaining,
                &cancelled,
            )
        } else {
            papyrus_terrain::road_grade_terrain::correct_retaining_samples(
                input,
                &self.samples,
                maximum_grade,
                maximum_cut_fill,
                remaining,
                &cancelled,
            )
        };
        match result {
            Ok(mut output) => {
                output.report.work += binding_work;
                Ok(Some(output))
            }
            Err(papyrus_terrain::road_grade_terrain::Failure::Cancelled) => Ok(None),
            Err(error) => Err(format!("Retained initial road bed: {error:?}")),
        }
    }
}
impl<'a> RetainedRails<'a> {
    /// `source` and `parent` must come from the verified installation loaders.
    /// A borrowed immutable Path retains control indexing, tangents and Y: the
    /// server's road pass sweeps rails but does not resample their controls.
    pub fn prepare(
        source: &rail_source::Source,
        parent: &infrastructure::Source,
        paths: &'a [Path],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let valid_hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        if !valid_hash(&source.sha256)
            || !valid_hash(&parent.sha256)
            || source.infrastructure_digest() != parent.sha256
            || source.source_receipts().get(..3) != parent.source_receipts().get(..3)
            || source.source_receipts().len() < 3
            || parent.source_receipts().len() < 3
            || paths.len() > 4096
            || !(1..=32_000_000).contains(&maximum_work)
        {
            return Err(
                "Retained rail height inputs differ from the road source or exceed limits".into(),
            );
        }
        let mesh = source
            .profile
            .mesh
            .as_ref()
            .ok_or("Retained rail height needs the captured PathList scale policy")?;
        mesh.validate()?;
        let shape = source.profile.bed_shape()?;
        let terrain_shape = Shape {
            width: shape.width,
            padding: [shape.inner_padding, shape.outer_padding],
            fade: [shape.inner_fade, shape.outer_fade],
        };
        terrain_shape.validate()?;
        if !shape.terrain_offset.is_finite() || shape.terrain_offset.abs() > 64. {
            return Err("Invalid retained rail terrain offset".into());
        }
        let mut work = 0usize;
        let mut controls = 0usize;
        let mut hasher = Sha256::new();
        let mut retained = Vec::with_capacity(paths.len());
        for path in paths {
            if cancelled() {
                return Ok(None);
            }
            if path.range() != (0, path.points().len() - 1)
                || !path.length().is_finite()
                || path.length() <= 0.
                || path.length() > 40000.
            {
                return Err("Invalid retained rail curve".into());
            }
            controls = controls
                .checked_add(path.points().len())
                .filter(|n| *n <= 1_048_576)
                .ok_or("Retained rail control limit reached")?;
            // Include control boundaries, not a flattened ambiguous stream.
            hasher.update((path.points().len() as u64).to_le_bytes());
            for p in path.points() {
                if cancelled() {
                    return Ok(None);
                }
                work = work
                    .checked_add(3)
                    .filter(|v| *v <= maximum_work)
                    .ok_or("Retained rail height work exhausted")?;
                for value in p {
                    hasher.update(value.to_bits().to_le_bytes());
                }
            }
            retained.push(InputPath {
                path,
                shape: terrain_shape,
                terrain_offset: shape.terrain_offset,
                random_scale: mesh.random_scale,
            });
        }
        Ok(Some(Self {
            paths: retained,
            receipt: Receipt {
                engine: "papyrus-retained-rail-height-inputs-1",
                source_sha256: source.sha256.clone(),
                infrastructure_sha256: parent.sha256.clone(),
                controls_sha256: format!("{:x}", hasher.finalize()),
                tracks: paths.len(),
                controls,
                preparation_work: work,
                complete_world_validated: false,
            },
        }))
    }
    pub(crate) fn paths(&self) -> &[InputPath<'a>] {
        &self.paths
    }
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }
    /// Capture full Hermite-strip dependencies after the source road/rail
    /// height loops, before Mapper's numerical road-bed repair. Keeping only
    /// rail control-point supports would miss bends and intervening samples.
    pub fn height_support(
        &self,
        layers: &papyrus_terrain::native_layers::Layers,
        geometry: crate::road_surface_stage::HeightGeometry,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<RetainedHeight>, String> {
        self.height_support_on_grid(layers, &layers.height, geometry, maximum_work, cancelled)
    }
    pub fn height_support_on_grid(
        &self,
        layers: &papyrus_terrain::native_layers::Layers,
        height: &[i16],
        geometry: crate::road_surface_stage::HeightGeometry,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<RetainedHeight>, String> {
        if cancelled() {
            return Ok(None);
        }
        let (hr, n) = papyrus_terrain::native_layers::dimensions(layers.size)?;
        if layers.height_resolution != hr
            || layers.surface_resolution != n
            || height.len() != hr * hr
            || !(1..=128_000_000).contains(&maximum_work)
        {
            return Err("Invalid retained rail height-support grid or limits".into());
        }
        let mut work = (hr * hr)
            .checked_mul(2)
            .filter(|&v| v <= maximum_work)
            .ok_or("Retained rail height-support work exhausted")?;
        let Some(mut mask) = papyrus_terrain::height_reservations::HeightReservations::new(
            papyrus_terrain::path_terrain::Grid {
                size: layers.size as f32,
                resolution: hr,
                origin_y: geometry.origin_y,
                vertical_span: geometry.vertical_span,
                height,
            },
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        for rail in &self.paths {
            let remaining = maximum_work
                .checked_sub(work)
                .filter(|&v| v > 0)
                .ok_or("Retained rail height-support work exhausted")?
                .min(64_000_000);
            let Some(used) =
                mask.reserve_path(rail.path, rail.shape.width * 0.5, remaining, &cancelled)?
            else {
                return Ok(None);
            };
            work = work
                .checked_add(used)
                .filter(|&v| v <= maximum_work)
                .ok_or("Retained rail height-support work exhausted")?;
        }
        if mask.samples() > 262144 {
            return Err("Retained rail height-support sample allowance reached".into());
        }
        work = work
            .checked_add(mask.samples())
            .filter(|&v| v <= maximum_work)
            .ok_or("Retained rail height-support work exhausted")?;
        let mut samples = Vec::with_capacity(mask.samples());
        let mut values = Vec::with_capacity(mask.samples());
        for (position, index) in mask.indices().enumerate() {
            if position % 4096 == 0 && cancelled() {
                return Ok(None);
            }
            samples.push(index);
            values.push(height[index]);
        }
        Ok((!cancelled()).then_some(RetainedHeight {
            mask,
            samples,
            values,
            preparation_work: work,
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn source_fixture() -> (
        tempfile::TempDir,
        infrastructure::Source,
        rail_source::Source,
    ) {
        let (root, catalog, parent, mut doc) = crate::rail_source::tests::fixture();
        let mesh: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/rails/client-0f703d43-meshes.json"
        ))
        .unwrap();
        doc["profile"]["mesh"] = mesh["mesh"].clone();
        doc["profile"]["bed"] = mesh["bed"].clone();
        let path = root.path().join("retained-rails.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let source =
            rail_source::Source::load(root.path(), &path, &catalog, &parent, || false).unwrap();
        (root, parent, source)
    }
    #[test]
    fn rail_driving_support_covers_curves_and_rejects_stale_height() {
        let (_root, parent, source) = source_fixture();
        let paths = [Path::from_controls(
            &[[-100., 20., 0.], [0., 20., 100.], [100., 20., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let retained = RetainedRails::prepare(&source, &parent, &paths, 10000, || false)
            .unwrap()
            .unwrap();
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut layers = papyrus_terrain::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16000; hr * hr],
            biome: Vec::new(),
            topology: Vec::new(),
            splat: Vec::new(),
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let geometry = crate::road_surface_stage::HeightGeometry {
            origin_y: -500.,
            vertical_span: 1000.,
        };
        let support = retained
            .height_support(&layers, geometry, 32_000_000, || false)
            .unwrap()
            .unwrap();
        assert!(!support.samples().is_empty());
        assert!(support.samples().windows(2).all(|v| v[0] < v[1]));
        for i in 0..=100 {
            let p = paths[0].point(paths[0].length() * i as f32 / 100.).unwrap();
            let layout = papyrus_terrain::path_terrain::Layout {
                size: 1000.,
                resolution: hr,
                origin_y: -500.,
                vertical_span: 1000.,
            };
            for sample in papyrus_terrain::path_terrain::height_support(layout, p)
                .unwrap()
                .indices
            {
                assert!(support.samples().binary_search(&sample).is_ok());
            }
        }
        assert!(!support.samples().contains(&0));
        assert!(support.validate(&layers, || false).unwrap());
        let topology = vec![0; n * n];
        let input = papyrus_terrain::road_grade_terrain::Input {
            grid: papyrus_terrain::path_terrain::Grid {
                size: 1000.,
                resolution: hr,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &layers.height,
            },
            ground: &layers.height,
            topology: &topology,
            surface_resolution: n,
            protected_topology: 1024,
            roads: &[],
            junctions: &[],
        };
        let fit = support
            .fit_initial_bed(input, 0.2, 8., 32_000_000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(fit.height, layers.height);
        assert_eq!(
            fit.report.retained_sample_entries,
            Some(support.samples().len())
        );
        assert!(fit.report.work >= hr * hr && fit.report.work <= 32_000_000);
        assert!(
            support
                .fit_initial_bed(input, 0.2, 8., hr * hr, || false)
                .is_err()
        );
        assert!(
            support
                .fit_initial_bed(input, 0.2, 8., 32_000_000, || true)
                .unwrap()
                .is_none()
        );
        let mut changed_height = layers.height.clone();
        changed_height[0] += 1;
        let stale = papyrus_terrain::road_grade_terrain::Input {
            grid: papyrus_terrain::path_terrain::Grid {
                height: &changed_height,
                ..input.grid
            },
            ground: &changed_height,
            ..input
        };
        assert!(
            support
                .fit_initial_bed(stale, 0.2, 8., 32_000_000, || false)
                .is_err()
        );
        // Refinement may change unreserved terrain without re-anchoring the
        // retained values. A later change under the same strip must still fail.
        assert!(
            support
                .validate_retained_grid(stale.grid, || false)
                .unwrap()
        );
        assert!(!support.validate_retained_grid(stale.grid, || true).unwrap());
        assert!(
            support
                .validate_retained_grid(
                    papyrus_terrain::path_terrain::Grid {
                        origin_y: 0.,
                        ..stale.grid
                    },
                    || false
                )
                .is_err()
        );
        changed_height[support.samples()[0]] += 1;
        assert!(
            support
                .validate_retained_grid(
                    papyrus_terrain::path_terrain::Grid {
                        height: &changed_height,
                        ..input.grid
                    },
                    || false
                )
                .is_err()
        );
        layers.height[0] += 1;
        assert!(support.validate(&layers, || false).is_err());
        assert!(
            retained
                .height_support(&layers, geometry, 1, || false)
                .is_err()
        );
        assert!(
            retained
                .height_support(&layers, geometry, 32_000_000, || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn retained_rail_inputs_bind_source_shape_controls_work_and_cancellation() {
        let (_root, mut parent, mut source) = source_fixture();
        let paths = [Path::from_controls(
            &[[-100., 20., 0.], [0., 21., 20.], [100., 20., 0.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap()];
        let prepared = RetainedRails::prepare(&source, &parent, &paths, 1000, || false)
            .unwrap()
            .unwrap();
        let shape = source.profile.bed_shape().unwrap();
        assert_eq!(prepared.paths()[0].shape.width, shape.width);
        assert_eq!(prepared.paths()[0].shape.fade[1], 32.);
        assert_eq!(prepared.paths()[0].random_scale, 1.);
        assert!(std::ptr::eq(prepared.paths()[0].path, &paths[0]));
        assert_eq!(prepared.receipt().tracks, 1);
        assert_eq!(prepared.receipt().controls, 3);
        assert_eq!(prepared.receipt().preparation_work, 9);
        assert!(!prepared.receipt().complete_world_validated);
        let old_digest = prepared.receipt().controls_sha256.clone();
        drop(prepared);
        assert!(RetainedRails::prepare(&source, &parent, &paths, 1, || false).is_err());
        assert!(
            RetainedRails::prepare(&source, &parent, &paths, 1000, || true)
                .unwrap()
                .is_none()
        );
        let moved = [Path::from_controls(
            &[[-100., 21., 0.], [0., 22., 20.], [100., 21., 0.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap()];
        assert_ne!(
            RetainedRails::prepare(&source, &parent, &moved, 1000, || false)
                .unwrap()
                .unwrap()
                .receipt()
                .controls_sha256,
            old_digest
        );
        let mut clipped = paths[0].clone();
        clipped.set_range(0, 1).unwrap();
        assert!(RetainedRails::prepare(&source, &parent, &[clipped], 1000, || false).is_err());
        parent.sha256 = "f".repeat(64);
        assert!(RetainedRails::prepare(&source, &parent, &paths, 1000, || false).is_err());
        parent.sha256 = source.infrastructure_digest().into();
        source.profile.mesh.as_mut().unwrap().random_scale = 0.5;
        assert!(RetainedRails::prepare(&source, &parent, &paths, 1000, || false).is_err());
    }
}
