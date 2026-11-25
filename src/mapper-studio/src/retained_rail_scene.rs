//! Binding and private copying of the prepared, pre-mesh rail revision.
use crate::rail_generation::Recipe;
use papyrus_terrain::rail_scene::Scene;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{BufWriter, Write};
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub scene_sha256: String,
}
struct Writer<'a, F> {
    hash: Sha256,
    bytes: usize,
    cancel: &'a F,
    interrupted: bool,
}
impl<F: Fn() -> bool> Write for Writer<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .filter(|&n| n <= 128 * 1024 * 1024)
            .ok_or_else(|| {
                std::io::Error::other("Retained rail scene receipt exceeds allowance")
            })?;
        if (self.bytes == 0 || self.bytes / 65536 != next / 65536) && (self.cancel)() {
            self.interrupted = true;
            return Err(std::io::Error::other("Cancelled"));
        }
        self.bytes = next;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Receipt {
    pub fn capture(
        scene: &Scene,
        recipe: &Recipe,
        resolution: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        recipe.validate()?;
        if !(2..=2048).contains(&resolution)
            || !scene.meshes.is_empty()
            || ["meshes", "gameplay_height", "corridor_before_objects"]
                .iter()
                .any(|key| scene.report.get(key).is_some_and(|v| !v.is_null()))
            || scene.report["composition_engine"] != recipe.engine
            || scene.report["source_sha256"] != recipe.source_sha256
            || scene.report["mesh_source_sha256"] != recipe.mesh_sha256
            || scene.tracks.len() > 4096
            || scene
                .tracks
                .iter()
                .map(|t| t.path.points().len())
                .sum::<usize>()
                > 1_048_576
            || scene.reservations.raster.len() != resolution * resolution
            || scene.reservations.samples.len() > 1_048_576
        {
            return Err("Retained rail scene is not the bound pre-mesh revision".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for track in &scene.tracks {
            if cancelled() {
                return Ok(None);
            }
            if track.id == 0
                || !ids.insert(track.id)
                || !track.maximum_discrete_grade.is_finite()
                || track.path.range() != (0, track.path.points().len() - 1)
                || track.path.length() <= 0.
            {
                return Err("Invalid retained rail scene track".into());
            }
        }
        for chunk in scene.reservations.raster.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            if chunk.iter().any(|&v| v > 1) {
                return Err("Invalid retained rail reservation raster".into());
            }
        }
        let mut writer = Writer {
            hash: Sha256::new(),
            bytes: 0,
            cancel: &cancelled,
            interrupted: false,
        };
        writer.hash.update(b"papyrus-pre-road-rail-scene-1");
        // Serde emits the reservation raster one number and separator at a time.
        // Feed the hash in chunks while preserving the exact receipt bytes.
        let mut buffer = BufWriter::with_capacity(65536, &mut writer);
        let result = serde_json::to_writer(
            &mut buffer,
            &(recipe, &scene.report, &scene.tracks, &scene.reservations),
        )
        .and_then(|()| buffer.flush().map_err(serde_json::Error::io));
        // An interrupted or failed write must not be retried by Drop.
        drop(buffer.into_parts());
        if writer.interrupted || cancelled() {
            return Ok(None);
        }
        result.map_err(|e| e.to_string())?;
        Ok(Some(Self {
            scene_sha256: format!("{:x}", writer.hash.finalize()),
        }))
    }
    pub fn check(
        &self,
        scene: &Scene,
        recipe: &Recipe,
        resolution: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<()>, String> {
        let Some(current) = Self::capture(scene, recipe, resolution, cancelled)? else {
            return Ok(None);
        };
        if &current != self {
            return Err("Retained rail scene or recipe revision changed".into());
        }
        Ok(Some(()))
    }
}
pub fn copy(
    scene: &Scene,
    recipe: &Recipe,
    resolution: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Scene>, String> {
    if Receipt::capture(scene, recipe, resolution, &cancelled)?.is_none() {
        return Ok(None);
    }
    let result = Scene {
        report: scene.report.clone(),
        tracks: scene.tracks.clone(),
        meshes: Vec::new(),
        reservations: scene.reservations.clone(),
    };
    Ok((!cancelled()).then_some(result))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture(resolution: usize) -> (Recipe, Scene) {
        let recipe = Recipe {
            engine: crate::rail_generation::VERSION.into(),
            source_sha256: "a".repeat(64),
            mesh_sha256: "b".repeat(64),
            settings: crate::rail_generation::initial_settings(1),
        };
        let path = papyrus_terrain::path_geometry::Path::from_controls(
            &[[-100., 20., 0.], [0., 20., 0.], [100., 20., 0.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let scene = Scene {
            report: serde_json::json!({"composition_engine":crate::rail_generation::VERSION,"source_sha256":recipe.source_sha256,"mesh_source_sha256":recipe.mesh_sha256}),
            tracks: vec![papyrus_terrain::rail_bed::FittedTrack {
                id: 1,
                preserve_start: false,
                path,
                maximum_discrete_grade: 0.,
            }],
            meshes: vec![],
            reservations: papyrus_terrain::rail_reservations::Output {
                raster: vec![0; resolution * resolution],
                samples: vec![],
                report: papyrus_terrain::rail_reservations::Report {
                    engine: papyrus_terrain::rail_reservations::VERSION,
                    tracks: 1,
                    sweeps: 0,
                    initial_blocked: 0,
                    newly_blocked: 0,
                    final_blocked: 0,
                    cell_visits: 0,
                    work: 0,
                },
            },
        };
        (recipe, scene)
    }
    #[test]
    fn versioned_recipe_requires_the_matching_scene_engine_without_upgrading_old_scenes() {
        let (mut recipe, mut scene) = fixture(32);
        let old = Receipt::capture(&scene, &recipe, 32, || false)
            .unwrap()
            .unwrap();
        recipe.engine = crate::rail_generation::BEFORE_ROADS_VERSION.into();
        assert!(Receipt::capture(&scene, &recipe, 32, || false).is_err());
        scene.report["composition_engine"] = recipe.engine.clone().into();
        let new = Receipt::capture(&scene, &recipe, 32, || false)
            .unwrap()
            .unwrap();
        assert_ne!(old, new);
    }
    #[test]
    fn receipt_rejects_changed_seed_or_settings_even_with_identical_geometry() {
        let (recipe, scene) = fixture(32);
        let receipt = Receipt::capture(&scene, &recipe, 32, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.check(&scene, &recipe, 32, || false).unwrap(),
            Some(())
        );
        for key in ["seed", "maximum_attempts"] {
            let mut changed = recipe.clone();
            let old = changed.settings["ring"][key].as_u64().unwrap();
            changed.settings["ring"][key] = (old + 1).into();
            changed.validate().unwrap();
            assert!(receipt.check(&scene, &changed, 32, || false).is_err());
        }
        assert_eq!(receipt.check(&scene, &recipe, 32, || true).unwrap(), None);
    }
    #[test]
    fn prepared_scene_receipt_covers_tracks_reservations_and_stage_and_copies_privately() {
        let (recipe, mut scene) = fixture(32);
        let original = Receipt::capture(&scene, &recipe, 32, || false)
            .unwrap()
            .unwrap();
        let mut copied = copy(&scene, &recipe, 32, || false).unwrap().unwrap();
        assert_eq!(
            Receipt::capture(&copied, &recipe, 32, || false)
                .unwrap()
                .unwrap(),
            original
        );
        copied.reservations.raster[0] = 1;
        assert_ne!(
            Receipt::capture(&copied, &recipe, 32, || false)
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(scene.reservations.raster[0], 0);
        scene.tracks[0].preserve_start = true;
        assert_ne!(
            Receipt::capture(&scene, &recipe, 32, || false)
                .unwrap()
                .unwrap(),
            original
        );
        scene.report["meshes"] = serde_json::json!({"work":1});
        assert!(Receipt::capture(&scene, &recipe, 32, || false).is_err());
        scene.report.as_object_mut().unwrap().remove("meshes");
        scene.reservations.raster[0] = 2;
        assert!(Receipt::capture(&scene, &recipe, 32, || false).is_err());
        assert!(
            Receipt::capture(&scene, &recipe, 32, || true)
                .unwrap()
                .is_none()
        );
    }
}
