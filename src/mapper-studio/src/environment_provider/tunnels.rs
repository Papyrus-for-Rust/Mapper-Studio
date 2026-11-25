//! Joint publication of a fresh dungeon spawn roster and its terrain effects.
use super::{Lifecycle, Request, Source, Update, scene};
use papyrus_terrain::{
    dungeon_base::Pose,
    environment::Verdict,
    native_layers::Layers,
    prefab_terrain_effects::{self, Edit, Instance},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub struct TunnelLimits {
    /// Includes occurrence binding, publication receipt and environment index.
    pub environment_work: usize,
    pub terrain_work: usize,
}
#[derive(Debug)]
pub enum TunnelError {
    Invalid(String),
    Environment(Verdict),
    Terrain(prefab_terrain_effects::Error),
}
#[derive(Debug, Serialize)]
pub struct TunnelReport {
    pub publication_sha256: String,
    pub publication_work: usize,
    pub environment: super::Report,
    pub terrain: prefab_terrain_effects::Report,
}

impl Source {
    /// Publish one fresh World.AddPrefab roster in its supplied spawn order.
    /// Actors carry the final runtime frame and authoritative activation state.
    /// Only generated tunnel prefabs with fully modeled terrain effects qualify.
    /// The caller supplies the complete preceding stage prefix.
    pub fn publish_tunnels(
        &self,
        scene: &mut scene::Scene,
        request: &Request,
        prefix: &scene::Prefix,
        layers: &mut Layers,
        limits: TunnelLimits,
        cancelled: impl Fn() -> bool,
    ) -> Result<TunnelReport, TunnelError> {
        self.publish_tunnels_recording(scene, request, prefix, layers, limits, None, cancelled)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_tunnels_recording(
        &self,
        scene: &mut scene::Scene,
        request: &Request,
        prefix: &scene::Prefix,
        layers: &mut Layers,
        limits: TunnelLimits,
        undo: Option<&mut prefab_terrain_effects::Undo>,
        cancelled: impl Fn() -> bool,
    ) -> Result<TunnelReport, TunnelError> {
        if cancelled() {
            return Err(TunnelError::Environment(Verdict::Cancelled));
        }
        if !(1..=super::MAX_WORK).contains(&limits.environment_work) {
            return Err(TunnelError::Invalid(
                "Invalid tunnel publication work allowance".into(),
            ));
        }
        // Reserve bounded identity lookup and ordered-receipt serialization
        // before allocating or binding the roster. Registry work shares this cap.
        let publication_work = request.actors.len() * 128 + 1;
        if publication_work >= limits.environment_work {
            return Err(TunnelError::Environment(Verdict::Budget));
        }
        let tunnels = self
            .tunnels
            .as_ref()
            .ok_or_else(|| TunnelError::Invalid("Tunnel environment source is not bound".into()))?;
        let mut instances = Vec::with_capacity(request.actors.len());
        for id in &request.publication_order {
            if cancelled() {
                return Err(TunnelError::Environment(Verdict::Cancelled));
            }
            let index = request
                .actors
                .binary_search_by_key(id, |actor| actor.id)
                .map_err(|_| TunnelError::Invalid("Missing tunnel publication identity".into()))?;
            let actor = &request.actors[index];
            if actor.pose.scale != [1.; 3]
                || !matches!(
                    actor.lifecycle,
                    Lifecycle::ActivatedAuthored
                        | Lifecycle::RootInactive
                        | Lifecycle::ActivationUnknown
                )
            {
                return Err(TunnelError::Invalid(
                    "Tunnel publication requires fresh unit-scale spawn occurrences".into(),
                ));
            }
            let prefab = tunnels.prefabs.get(&actor.prefab).ok_or_else(|| {
                TunnelError::Invalid("Spawned tunnel is absent from the bound source".into())
            })?;
            let effects = prefab.effects.as_ref().ok_or_else(|| {
                TunnelError::Invalid(
                    "Spawned prefab has no complete generated tunnel terrain adapter".into(),
                )
            })?;
            instances.push(Instance {
                prefab_id: prefab.id,
                pose: Pose {
                    position: actor.pose.position,
                    rotation: actor.pose.rotation,
                },
                effects,
            });
        }
        let raw = serde_json::to_vec(&(
            "papyrus-tunnel-publication-1",
            &request.context.request_sha256,
            &request.context.sources_sha256,
            &request.occurrences_sha256,
            &request.publication_order,
        ))
        .map_err(|e| TunnelError::Invalid(e.to_string()))?;
        let publication_sha256 = format!("{:x}", Sha256::digest(raw));
        let snapshot = scene.snapshot();
        if snapshot.summary().completed_stages.contains(&request.stage) {
            return Err(TunnelError::Invalid(
                "Tunnel terrain stage has already been published in this candidate".into(),
            ));
        }
        let update = self
            .prepare_update_from_prefix(
                &snapshot,
                request,
                prefix,
                limits.environment_work - publication_work,
                &cancelled,
            )
            .map_err(TunnelError::Invalid)?
            .map_err(TunnelError::Environment)?;
        let edit =
            prefab_terrain_effects::prepare(layers, &instances, limits.terrain_work, &cancelled)
                .map_err(TunnelError::Terrain)?;
        commit(
            scene,
            update,
            edit,
            publication_sha256,
            publication_work,
            undo,
            cancelled,
        )
    }
}

fn commit(
    scene: &mut scene::Scene,
    update: Update,
    edit: Edit<'_>,
    publication_sha256: String,
    publication_work: usize,
    undo: Option<&mut prefab_terrain_effects::Undo>,
    cancelled: impl Fn() -> bool,
) -> Result<TunnelReport, TunnelError> {
    let (update, environment) = update.into_parts();
    if !scene
        .publish(update, cancelled)
        .map_err(TunnelError::Invalid)?
    {
        return Err(TunnelError::Environment(Verdict::Cancelled));
    }
    // The scene's final cancellation and revision checks succeeded. Ownership
    // transfers finish this exclusive stage publication without another callback.
    let terrain = match undo {
        Some(undo) => edit.commit_recording(undo),
        None => edit.commit(),
    };
    Ok(TunnelReport {
        publication_sha256,
        publication_work,
        environment,
        terrain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::{environment::Coverage, native_layers::dimensions};

    fn scene() -> scene::Scene {
        scene::Scene::new(
            scene::Context {
                request_sha256: "a".repeat(64),
                sources_sha256: "b".repeat(64),
            },
            vec![scene::Requirement {
                stage: 25,
                inputs_sha256: "c".repeat(64),
                dependencies: vec![],
            }],
            Coverage::Complete,
        )
        .unwrap()
    }
    fn update(scene: &scene::Scene) -> Update {
        let snapshot = scene.snapshot();
        let update = snapshot
            .prepare(
                snapshot.context(),
                &[scene::Replacement {
                    stage: 25,
                    inputs_sha256: "c".repeat(64),
                    instances: vec![],
                }],
                10000,
                || false,
            )
            .unwrap()
            .unwrap();
        Update {
            update,
            report: super::super::Report {
                engine: super::super::VERSION,
                sources_sha256: "b".repeat(64),
                inputs_sha256: "c".repeat(64),
                occurrences_sha256: "d".repeat(64),
                stage: 25,
                actors: 0,
                volumes: 0,
                unresolved_geometry: 0,
                preparation_work_reserved: 0,
                index_work: 0,
            },
        }
    }
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16383; hr * hr],
            topology: vec![0; n * n],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn effects() -> prefab_terrain_effects::Prepared {
        prefab_terrain_effects::Prepared::new(vec![
            prefab_terrain_effects::Effect::RaiseHeight {
                position: [0., 20., 0.],
                opacity: 1.,
                radius: 20.,
                fade: 4.,
            },
            prefab_terrain_effects::Effect::SetTopology {
                position: [0.; 3],
                mask: 4,
                radius: 20.,
                fade: 4.,
            },
        ])
        .unwrap()
    }
    #[test]
    fn final_cancellation_and_stale_or_foreign_scene_discard_terrain_edits() {
        for stop in [1, 2] {
            let mut scene = scene();
            let update = update(&scene);
            let mut layers = layers();
            let original = layers.clone();
            let effects = effects();
            let instances = [Instance {
                prefab_id: 1,
                pose: Pose {
                    position: [0.; 3],
                    rotation: [0., 0., 0., 1.],
                },
                effects: &effects,
            }];
            let edit = prefab_terrain_effects::prepare(&mut layers, &instances, 10000000, || false)
                .unwrap();
            let calls = std::cell::Cell::new(0);
            let result = commit(&mut scene, update, edit, "e".repeat(64), 0, None, || {
                calls.set(calls.get() + 1);
                calls.get() == stop
            });
            assert!(matches!(
                result,
                Err(TunnelError::Environment(Verdict::Cancelled))
            ));
            assert_eq!(scene.snapshot().revision(), 0);
            assert_eq!(layers.height, original.height);
            assert_eq!(layers.topology, original.topology);
        }
        for foreign in [false, true] {
            let mut scene = scene();
            let update = update(&scene);
            let expected_revision = if foreign {
                scene = self::scene();
                0
            } else {
                let replacement = self::update(&scene);
                assert!(scene.publish(replacement.into_parts().0, || false).unwrap());
                1
            };
            let mut layers = layers();
            let original = layers.clone();
            let effects = effects();
            let instances = [Instance {
                prefab_id: 1,
                pose: Pose {
                    position: [0.; 3],
                    rotation: [0., 0., 0., 1.],
                },
                effects: &effects,
            }];
            let edit = prefab_terrain_effects::prepare(&mut layers, &instances, 10000000, || false)
                .unwrap();
            assert!(matches!(
                commit(&mut scene, update, edit, "e".repeat(64), 0, None, || false),
                Err(TunnelError::Invalid(_))
            ));
            assert_eq!(scene.snapshot().revision(), expected_revision);
            assert_eq!(layers.height, original.height);
            assert_eq!(layers.topology, original.topology);
        }
    }
    #[test]
    fn accepted_publication_commits_both_outputs_without_a_later_cancellation_check() {
        let mut scene = scene();
        let update = update(&scene);
        let mut layers = layers();
        let original = layers.clone();
        let effects = effects();
        let instances = [Instance {
            prefab_id: 1,
            pose: Pose {
                position: [0.; 3],
                rotation: [0., 0., 0., 1.],
            },
            effects: &effects,
        }];
        let edit =
            prefab_terrain_effects::prepare(&mut layers, &instances, 10000000, || false).unwrap();
        let calls = std::cell::Cell::new(0);
        let report = commit(&mut scene, update, edit, "e".repeat(64), 0, None, || {
            calls.set(calls.get() + 1);
            calls.get() > 2
        })
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(scene.snapshot().revision(), 1);
        assert_ne!(layers.height, original.height);
        assert_ne!(layers.topology, original.topology);
        assert!(report.terrain.height_writes > 0 && report.terrain.topology_writes > 0);
    }
}
