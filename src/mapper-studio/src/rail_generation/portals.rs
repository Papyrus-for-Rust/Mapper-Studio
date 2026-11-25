//! Post-ring portal connectors retain their source pool and occurrence receipts.
use super::RingPlan;
use crate::{
    rail_preview::{GeneratedConnections, Socket, Sockets},
    tunnel_source,
};
use papyrus_terrain::{dungeon_base::Pose, native_layers::Layers};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize)]
pub struct Portal {
    /// World-global occurrence identity supplied by the dungeon coordinator.
    pub instance: u64,
    pub prefab: String,
    /// Final spawned frame, including the owning stage's rotation policy.
    pub pose: Pose,
}

impl RingPlan {
    /// Identify the first generated portal whose fixed connector cannot be
    /// supported by the stamped terrain within the bed's allowance.
    pub(crate) fn unsupported_portal_root(
        &self,
        portals: &[Portal],
        sockets: &Sockets,
        layers: &Layers,
    ) -> Result<Option<[f32; 3]>, String> {
        let shape = self.source.profile.bed_shape()?;
        let allowance = papyrus_terrain::rail_bed_fit::control_allowance(
            self.settings.bed.maximum_cut_fill,
            shape.terrain_offset,
        );
        let size = layers.size as f32;
        let view = layers.view();
        for portal in portals {
            for socket in sockets
                .sockets
                .iter()
                .filter(|s| s.monument == portal.instance)
            {
                let [x, y, z] = socket.position;
                let u = x / size + 0.5;
                let v = z / size + 0.5;
                if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
                    continue;
                }
                let height = papyrus_terrain::spawn::sample_height_view(view, u, v)?;
                if (f64::from(y) - f64::from(height)).abs() > allowance {
                    return Ok(Some(portal.pose.position));
                }
            }
        }
        Ok(None)
    }

    /// Add successfully published upward portals before resuming later rail
    /// stages. The ring remains fixed. Supply portals in world publication order.
    /// Rejected or cancelled additions leave this checkpoint unchanged.
    pub fn add_tunnel_portals(
        &mut self,
        source: &tunnel_source::Source,
        portals: &[Portal],
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        let Some(sockets) = self.sockets_with_tunnel_portals(source, portals, cancelled)? else {
            return Ok(false);
        };
        self.sockets = sockets;
        Ok(true)
    }

    pub(crate) fn sockets_with_tunnel_portals(
        &self,
        source: &tunnel_source::Source,
        portals: &[Portal],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Sockets>, String> {
        if cancelled() {
            return Ok(None);
        }
        if source.catalog_sha256 != self.infrastructure.catalog_sha256
            || source.monument_sha256 != self.monument_sha256
        {
            return Err("Tunnel portals differ from the rail checkpoint's source bindings".into());
        }
        let upward = source
            .selectors
            .get("UpwardsFolder")
            .ok_or("Tunnel source has no upward portal pool")?;
        let occupied = self.layout.outcomes.iter().map(|o| o.id).collect();
        let Some(sockets) = prepare(
            &self.sockets,
            occupied,
            portals,
            |portal| {
                if !upward.prefabs.contains(&portal.prefab)
                    || source
                        .prefabs
                        .get(&portal.prefab)
                        .is_none_or(|p| p.surface_offset.is_none())
                {
                    return Err(
                        "Generated rail monument is absent from the source's upward portal pool"
                            .into(),
                    );
                }
                source.path_sockets(
                    &portal.prefab,
                    portal.instance,
                    portal.pose,
                    self.source.profile.rail_type,
                )
            },
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let raw =
            serde_json::to_vec(&("papyrus-generated-rail-portals-1", &source.sha256, portals))
                .map_err(|e| e.to_string())?;
        let receipt = GeneratedConnections {
            source_sha256: source.sha256.clone(),
            occurrences_sha256: format!("{:x}", Sha256::digest(raw)),
            monuments: portals.len(),
        };
        if cancelled() {
            return Ok(None);
        }
        let mut combined = self.sockets.clone();
        combined.sockets.extend(sockets);
        combined.generated = Some(receipt);
        Ok(Some(combined))
    }
}

fn prepare(
    existing: &Sockets,
    mut occupied: BTreeSet<u64>,
    portals: &[Portal],
    resolve: impl Fn(&Portal) -> Result<Vec<Socket>, String>,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<Socket>>, String> {
    if existing.generated.is_some() || portals.len() > 128 || existing.sockets.len() > 256 {
        return Err("Tunnel portal handoff is already bound or exceeds its allowance".into());
    }
    occupied.extend(existing.sockets.iter().map(|s| s.monument));
    let mut additions = Vec::new();
    for portal in portals {
        if cancelled() {
            return Ok(None);
        }
        if portal.instance == 0 || !occupied.insert(portal.instance) {
            return Err("Generated tunnel portal reuses a world occurrence identity".into());
        }
        let sockets = resolve(portal)?;
        if existing.sockets.len() + additions.len() + sockets.len() > 256 {
            return Err("Rail connector count exceeds supported budget".into());
        }
        let mut components = BTreeSet::new();
        for socket in &sockets {
            if socket.monument != portal.instance
                || socket.prefab != portal.prefab
                || !components.insert(socket.connector)
            {
                return Err(
                    "Generated tunnel connectors have inconsistent occurrence identities".into(),
                );
            }
        }
        additions.extend(sockets);
    }
    Ok((!cancelled()).then_some(additions))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sockets() -> Sockets {
        Sockets {
            layout_sha256: "a".repeat(64),
            infrastructure_sha256: "b".repeat(64),
            sockets: vec![],
            generated: None,
        }
    }
    fn portal(instance: u64) -> Portal {
        Portal {
            instance,
            prefab: "portal.prefab".into(),
            pose: Pose {
                position: [0.; 3],
                rotation: [0., 0., 0., 1.],
            },
        }
    }
    fn resolve(portal: &Portal) -> Result<Vec<Socket>, String> {
        Ok([3, 1]
            .into_iter()
            .map(|connector| Socket {
                monument: portal.instance,
                connector,
                prefab: portal.prefab.clone(),
                position: [1., 2., 3.],
                forward: [0., 0., 1.],
            })
            .collect())
    }
    #[test]
    fn generated_connectors_preserve_publication_and_authored_component_order() {
        let existing = sockets();
        let result = prepare(
            &existing,
            [1].into_iter().collect(),
            &[portal(9), portal(2)],
            resolve,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            result
                .iter()
                .map(|s| (s.monument, s.connector))
                .collect::<Vec<_>>(),
            vec![(9, 3), (9, 1), (2, 3), (2, 1)]
        );
        assert!(existing.sockets.is_empty());
        assert!(existing.generated.is_none());
    }
    #[test]
    fn invalid_or_cancelled_handoff_never_changes_the_checkpoint() {
        let existing = sockets();
        let before = serde_json::to_vec(&existing).unwrap();
        for portals in [
            vec![portal(0)],
            vec![portal(1)],
            vec![portal(2), portal(2)],
            vec![portal(2); 129],
        ] {
            assert!(
                prepare(
                    &existing,
                    [1].into_iter().collect(),
                    &portals,
                    resolve,
                    &|| false
                )
                .is_err()
            );
        }
        assert!(
            prepare(
                &existing,
                BTreeSet::new(),
                &[portal(2)],
                |_| Err("missing prefab".into()),
                &|| false
            )
            .is_err()
        );
        assert!(
            prepare(&existing, BTreeSet::new(), &[portal(2)], resolve, &|| true)
                .unwrap()
                .is_none()
        );
        assert!(
            prepare(
                &existing,
                BTreeSet::new(),
                &[portal(2)],
                |p| {
                    let mut s = resolve(p)?;
                    s[1].connector = s[0].connector;
                    Ok(s)
                },
                &|| false
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&existing).unwrap(), before);
    }
    #[test]
    fn combined_connector_allowance_counts_existing_and_new_occurrences() {
        let mut existing = sockets();
        existing.sockets = (1..=127)
            .flat_map(|id| resolve(&portal(id)).unwrap())
            .collect();
        assert_eq!(
            prepare(&existing, BTreeSet::new(), &[portal(128)], resolve, &|| {
                false
            })
            .unwrap()
            .unwrap()
            .len(),
            2
        );
        assert!(
            prepare(
                &existing,
                BTreeSet::new(),
                &[portal(128), portal(129)],
                resolve,
                &|| false
            )
            .is_err()
        );
    }
    #[test]
    fn legacy_socket_reports_load_without_generated_metadata() {
        let existing = sockets();
        let raw = serde_json::to_vec(&existing).unwrap();
        let decoded: Sockets = serde_json::from_slice(&raw).unwrap();
        assert!(decoded.generated.is_none());
        assert!(!String::from_utf8(raw).unwrap().contains("generated"));
    }
    #[test]
    fn cancellation_after_resolving_a_portal_discards_its_connectors() {
        let cancelled = std::cell::Cell::new(false);
        let existing = sockets();
        let result = prepare(
            &existing,
            BTreeSet::new(),
            &[portal(1), portal(2)],
            |p| {
                cancelled.set(true);
                resolve(p)
            },
            &|| cancelled.get(),
        )
        .unwrap();
        assert!(result.is_none());
        assert!(existing.sockets.is_empty());
        assert!(existing.generated.is_none());
    }
}
