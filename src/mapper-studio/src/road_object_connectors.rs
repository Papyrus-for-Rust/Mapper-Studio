//! TerrainPathConnect is a scene socket, not a terrain modifier. Preserve its
//! source type and transformed position without inventing a connected network.
use crate::road_object_source::Prefab;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    #[serde(rename = "Type")]
    infrastructure_type: i32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attribute {
    kind: String,
    root: bool,
    position: [f32; 3],
    local_to_root: Option<[[f32; 4]; 4]>,
    fields: Fields,
    #[serde(default)]
    references: Vec<serde_json::Value>,
}
#[derive(Debug)]
pub(crate) struct Local {
    pub attribute: usize,
    pub infrastructure_type: i32,
    pub position: [f32; 3],
}
#[derive(Debug, Serialize)]
pub struct Connector {
    pub fit_pose: usize,
    pub prefab: String,
    pub attribute: usize,
    pub infrastructure_type: i32,
    pub position: [f32; 3],
}
pub(crate) fn local(prefab: &Prefab) -> Result<Vec<Local>, String> {
    let mut out = Vec::new();
    for (attribute, raw) in prefab.attributes.iter().enumerate() {
        if raw.get("kind").and_then(|v| v.as_str()) != Some("TerrainPathConnect") {
            continue;
        }
        if out.len() == 256 {
            return Err("Road-object prefab exceeds the connector allowance".into());
        }
        let a: Attribute = serde_json::from_value(raw.clone())
            .map_err(|e| format!("Invalid road-object connector: {e}"))?;
        if a.kind != "TerrainPathConnect"
            || !a.references.is_empty()
            || a.position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || (!a.root && a.local_to_root.is_none())
        {
            return Err("Unsupported road-object connector metadata".into());
        }
        if let Some(m) = a.local_to_root
            && (m
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
                || m[3] != [0., 0., 0., 1.]
                || (0..3).any(|i| (m[i][3] - a.position[i]).abs() > 0.001))
        {
            return Err("Road-object connector position/transform mismatch".into());
        }
        // Type remains an opaque source enum value. Routing must resolve its
        // role from the selected installation, not a table of current game types.
        out.push(Local {
            attribute,
            infrastructure_type: a.fields.infrastructure_type,
            position: a.position,
        });
    }
    Ok(out)
}
pub(crate) fn world(
    fit_pose: usize,
    prefab: &str,
    connector: &Local,
    pose: &papyrus_terrain::decor_fit::OrientedPose,
) -> Result<Connector, String> {
    let offset = papyrus_terrain::decor_fit::rotate(
        pose.rotation,
        std::array::from_fn(|i| connector.position[i] * pose.scale[i]),
    );
    let position: [f32; 3] = std::array::from_fn(|i| pose.position[i] + offset[i]);
    if position.iter().any(|v| !v.is_finite()) {
        return Err("Non-finite road-object connector transform".into());
    }
    Ok(Connector {
        fit_pose,
        prefab: prefab.into(),
        attribute: connector.attribute,
        infrastructure_type: connector.infrastructure_type,
        position,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prefab() -> Prefab {
        Prefab {
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
            attributes: vec![
                serde_json::json!({"kind":"TerrainPathConnect","root":false,"position":[2.,3.,4.],"local_to_root":[[1.,0.,0.,2.],[0.,1.,0.,3.],[0.,0.,1.,4.],[0.,0.,0.,1.]],"fields":{"Type":12345},"references":[]}),
            ],
            components: Default::default(),
            issues: vec![],
        }
    }
    #[test]
    fn sockets_preserve_opaque_source_types_and_follow_full_fitted_pose() {
        let p = prefab();
        let local = local(&p).unwrap();
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let pose = papyrus_terrain::decor_fit::OrientedPose {
            position: [10., 20., 30.],
            rotation: [0., q, 0., q],
            scale: [2., 3., 4.],
        };
        let socket = world(7, "test", &local[0], &pose).unwrap();
        assert_eq!(socket.fit_pose, 7);
        assert_eq!(socket.attribute, 0);
        assert_eq!(socket.infrastructure_type, 12345);
        for (got, expected) in socket.position.into_iter().zip([26., 29., 26.]) {
            assert!((got - expected).abs() < 0.00001);
        }
    }
    #[test]
    fn incomplete_or_extended_socket_contracts_are_rejected_instead_of_dropped() {
        for pointer in ["/fields/Type", "/root", "/position", "/local_to_root"] {
            let mut p = prefab();
            *p.attributes[0].pointer_mut(pointer).unwrap() = serde_json::Value::Null;
            assert!(local(&p).is_err(), "{pointer}");
        }
        let mut p = prefab();
        p.attributes[0]["local_to_root"][0][3] = 99.into();
        assert!(local(&p).is_err());
        let mut p = prefab();
        p.attributes[0]["fields"]["Unknown"] = 1.into();
        assert!(local(&p).is_err());
        let mut p = prefab();
        p.attributes[0]["references"] = serde_json::json!([{}]);
        assert!(local(&p).is_err());
    }
}
