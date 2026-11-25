//! Owned, failed-fit revision used only for subsequent geometry guidance.
//! Terrain ownership moves out of a rejected transaction; nothing is published.
use crate::road_portal_mesh::Failure;
use papyrus_terrain::path_terrain::{self, Grid, Layout};
pub struct Revision {
    road: usize,
    round: usize,
    layout: Layout,
    height: Vec<i16>,
    controls: Vec<[f32; 3]>,
    original: Vec<[f32; 3]>,
}
impl std::fmt::Debug for Revision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailedFitRevision")
            .field("road", &self.road)
            .field("round", &self.round)
            .field("height_samples", &self.height.len())
            .field("controls", &self.controls.len())
            .field("scope", &"guidance_only")
            .finish()
    }
}
pub(crate) struct Projection {
    pub controls: Vec<[f32; 3]>,
    pub work: usize,
    pub changed_controls: usize,
}
impl Revision {
    pub(crate) fn new(
        road: usize,
        round: usize,
        layout: Layout,
        height: Vec<i16>,
        controls: Vec<[f32; 3]>,
        original: Vec<[f32; 3]>,
    ) -> Self {
        Self {
            road,
            round,
            layout,
            height,
            controls,
            original,
        }
    }
    pub(crate) fn bind_original(&mut self, roads: &papyrus_terrain::road_network::Report) {
        if let Some(road) = roads
            .roads
            .get(self.road)
            .filter(|r| r.points.len() == self.controls.len())
        {
            self.original = road.points.clone();
        } else {
            self.original.clear();
        }
    }
    pub(crate) fn round(&self) -> usize {
        self.round
    }
    pub(crate) fn grid(&self) -> Grid<'_> {
        Grid {
            size: self.layout.size,
            resolution: self.layout.resolution,
            origin_y: self.layout.origin_y,
            vertical_span: self.layout.vertical_span,
            height: &self.height,
        }
    }
    pub(crate) fn matches(&self, road: usize, layout: Grid<'_>, original: &[[f32; 3]]) -> bool {
        self.road == road
            && self.layout.size.to_bits() == layout.size.to_bits()
            && self.layout.resolution == layout.resolution
            && self.layout.origin_y.to_bits() == layout.origin_y.to_bits()
            && self.layout.vertical_span.to_bits() == layout.vertical_span.to_bits()
            && self.original.len() == original.len()
            && self.controls.len() == original.len()
            && self
                .original
                .iter()
                .flatten()
                .zip(original.iter().flatten())
                .all(|(a, b)| a.to_bits() == b.to_bits())
    }
    /// Transfer plan displacement onto the already straightened/painted controls.
    /// Unchanged controls retain their exact fitted XYZ. Changed controls retain
    /// their prior vertical offset relative to the failed revision's terrain.
    pub(crate) fn project(
        &self,
        original: &[[f32; 3]],
        candidate: &[[f32; 3]],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Projection>, Failure> {
        if cancelled() {
            return Ok(None);
        }
        let mut work = original
            .len()
            .checked_mul(8)
            .filter(|v| *v <= maximum_work)
            .ok_or(Failure::Budget)?;
        if !self.matches(self.road, self.grid(), original) || candidate.len() != original.len() {
            return Err("Stale failed-fit portal revision".into());
        }
        let mut controls = self.controls.clone();
        let mut changed = 0;
        for ((out, before), after) in controls.iter_mut().zip(original).zip(candidate) {
            if cancelled() {
                return Ok(None);
            }
            if before[0].to_bits() == after[0].to_bits()
                && before[2].to_bits() == after[2].to_bits()
            {
                continue;
            }
            work = work
                .checked_add(16)
                .filter(|v| *v <= maximum_work)
                .ok_or(Failure::Budget)?;
            let old = *out;
            let x = old[0] + (after[0] - before[0]);
            let z = old[2] + (after[2] - before[2]);
            if !x.is_finite()
                || !z.is_finite()
                || x.abs() > self.layout.size * 0.5
                || z.abs() > self.layout.size * 0.5
            {
                return Err("Projected portal control leaves terrain".into());
            }
            let old_ground = path_terrain::sample_height(self.grid(), old)?;
            let new_ground = path_terrain::sample_height(self.grid(), [x, 0., z])?;
            let y = old[1] + (new_ground - old_ground);
            if !y.is_finite() || y.abs() > 10000. {
                return Err("Invalid projected portal height".into());
            }
            *out = [x, y, z];
            changed += 1;
        }
        Ok(Some(Projection {
            controls,
            work,
            changed_controls: changed,
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Revision {
        let height = (0..25).map(|i| 16000 + (i % 5) * 10).collect::<Vec<i16>>();
        Revision::new(
            7,
            0,
            Layout {
                size: 4.,
                resolution: 5,
                origin_y: -500.,
                vertical_span: 1000.,
            },
            height,
            vec![[-1., 2., 0.], [0.25, 3., 0.], [1., 4., 0.]],
            vec![[-1., 1., 0.], [0., 1., 0.], [1., 1., 0.]],
        )
    }
    #[test]
    fn projection_preserves_straightening_vertical_offsets_and_unchanged_controls() {
        let revision = fixture();
        let original = revision.original.clone();
        let mut candidate = original.clone();
        candidate[1][0] = 0.5;
        let projected = revision
            .project(&original, &candidate, 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(projected.controls[0], revision.controls[0]);
        assert_eq!(projected.controls[2], revision.controls[2]);
        assert_eq!(projected.controls[1][0], 0.75);
        let expected = 3.
            + (path_terrain::sample_height(revision.grid(), [0.75, 0., 0.]).unwrap()
                - path_terrain::sample_height(revision.grid(), [0.25, 0., 0.]).unwrap());
        assert_eq!(projected.controls[1][1].to_bits(), expected.to_bits());
        assert_eq!(projected.changed_controls, 1);
        assert!(
            revision
                .project(&original, &candidate, projected.work, || false)
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            revision.project(&original, &candidate, projected.work - 1, || false),
            Err(Failure::Budget)
        ));
        assert_eq!(revision.controls[1], [0.25, 3., 0.]);
    }
    #[test]
    fn stale_originals_layouts_and_cancellation_cannot_reuse_a_revision() {
        let revision = fixture();
        let mut wrong = revision.original.clone();
        wrong[1][1] += 0.1;
        assert!(revision.project(&wrong, &wrong, 1000, || false).is_err());
        assert!(!revision.matches(8, revision.grid(), &revision.original));
        assert!(!revision.matches(
            7,
            Grid {
                size: 5.,
                ..revision.grid()
            },
            &revision.original
        ));
        assert!(
            revision
                .project(&revision.original, &revision.original, 1000, || true)
                .unwrap()
                .is_none()
        );
        let unchanged = revision
            .project(&revision.original, &revision.original, 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.controls, revision.controls);
        assert!(format!("{revision:?}").len() < 256);
    }
}
