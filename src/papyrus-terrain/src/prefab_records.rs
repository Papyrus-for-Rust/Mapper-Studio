//! Bounded prefab records for the inspected version-10 map codec.
use serde::Serialize;
pub const VERSION: &str = "papyrus-prefab-records-1";
pub struct Instance<'a> {
    pub category: &'a str,
    pub path: &'a str,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub category: String,
    pub path: String,
    pub id: u32,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}
/// Decode a map-record rotation using Unity's Y * X * Z Euler convention.
pub fn quaternion(angles: [f32; 3]) -> Result<[f32; 4], String> {
    if angles.iter().any(|angle| !angle.is_finite()) {
        return Err("Invalid prefab Euler rotation".into());
    }
    Ok(crate::decor_fit::euler(angles))
}

/// Unity's Quaternion.Euler convention is Y * X * Z. The singular branch keeps
/// the same orientation with Z=0; Euler representations are not unique.
pub fn euler(q: [f32; 4]) -> Result<[f32; 3], String> {
    if q.iter().any(|v| !v.is_finite()) {
        return Err("Invalid prefab quaternion".into());
    }
    let length = q.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    if (length - 1.).abs() > 0.001 {
        return Err("Prefab rotation must be a unit quaternion".into());
    }
    let [x, y, z, w] = q.map(|v| f64::from(v) / length.sqrt());
    let m12 = 2. * (y * z - x * w);
    let sine = (-m12).clamp(-1., 1.);
    let pitch = sine.asin();
    let (yaw, roll) = if (1. - sine * sine).sqrt() > 1e-6 {
        (
            (2. * (x * z + y * w)).atan2(1. - 2. * (x * x + y * y)),
            (2. * (x * y + z * w)).atan2(1. - 2. * (x * x + z * z)),
        )
    } else {
        ((-2. * (x * z - y * w)).atan2(1. - 2. * (y * y + z * z)), 0.)
    };
    Ok([pitch, yaw, roll].map(|v| (v.to_degrees().rem_euclid(360.) as f32).rem_euclid(360.)))
}
pub fn assemble(
    inputs: &[Instance<'_>],
    resolve: impl Fn(&str) -> Option<u32>,
    cancel: impl Fn() -> bool,
) -> Result<Option<Vec<Record>>, String> {
    if inputs.len() > crate::content::MAX_PREFABS {
        return Err("Prefab record allowance exceeded".into());
    }
    let mut out = Vec::with_capacity(inputs.len());
    for p in inputs {
        if cancel() {
            return Ok(None);
        }
        if p.category.is_empty()
            || p.category.len() > 128
            || p.category.chars().any(char::is_control)
            || p.position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || p.scale
                .iter()
                .any(|v| !v.is_finite() || *v == 0. || v.abs() > 100.)
        {
            return Err("Invalid prefab record transform or category".into());
        }
        let id = resolve(p.path).filter(|v| *v != 0).ok_or_else(|| {
            format!(
                "Prefab has no authoritative installation identity: {}",
                p.path
            )
        })?;
        out.push(Record {
            category: p.category.into(),
            path: p.path.into(),
            id,
            position: p.position,
            rotation: euler(p.rotation)?,
            scale: p.scale,
        });
    }
    Ok((!cancel()).then_some(out))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn quaternion([x, y, z]: [f32; 3]) -> [f32; 4] {
        let (sx, cx) = (x.to_radians() * 0.5).sin_cos();
        let (sy, cy) = (y.to_radians() * 0.5).sin_cos();
        let (sz, cz) = (z.to_radians() * 0.5).sin_cos();
        [
            cy * sx * cz + sy * cx * sz,
            sy * cx * cz - cy * sx * sz,
            cy * cx * sz - sy * sx * cz,
            cy * cx * cz + sy * sx * sz,
        ]
    }
    #[test]
    fn rotations_roundtrip_arbitrary_pitch_yaw_roll_and_both_gimbal_poles() {
        for x in [
            -180., -90.001, -90., -89.999, 0., 32.5, 89.999, 90., 90.001, 180.,
        ] {
            for y in [-130., 0., 81., 270.] {
                for z in [-177., 0., 49., 182.] {
                    let q = quaternion([x, y, z]);
                    let actual = quaternion(euler(q).unwrap());
                    let decoded = super::quaternion(euler(q).unwrap()).unwrap();
                    let decoded_dot = q.iter().zip(decoded).map(|(a,b)|a*b).sum::<f32>().abs();
                    assert!((1. - decoded_dot).abs() < 0.000002, "decoded {x} {y} {z}: {decoded_dot}");
                    let dot = q.iter().zip(actual).map(|(a, b)| a * b).sum::<f32>().abs();
                    assert!((1. - dot).abs() < 0.000002, "{x} {y} {z}: {dot}");
                    assert_eq!(euler(q).unwrap(), euler(q.map(|v| -v)).unwrap());
                }
            }
        }
    }
    #[test]
    fn map_rotation_decoder_rejects_nonfinite_angles() {
        for angle in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(super::quaternion([angle, 0., 0.]).is_err());
        }
    }
    #[test]
    fn identity_resolution_errors_and_cancel_never_publish_partial_records() {
        let p = || Instance {
            category: "Decor",
            path: "assets/a.prefab",
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        };
        let r = assemble(&[p()], |_| Some(123), || false).unwrap().unwrap();
        assert_eq!(r[0].id, 123);
        assert!(assemble(&[p(), p()], |_| None, || false).is_err());
        assert!(assemble(&[p()], |_| Some(0), || false).is_err());
        let calls = std::cell::Cell::new(0);
        assert!(
            assemble(
                &[p(), p()],
                |_| Some(123),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() == 2
                }
            )
            .unwrap()
            .is_none()
        );
        let mut bad = p();
        bad.rotation = [0.; 4];
        assert!(assemble(&[bad], |_| Some(123), || false).is_err());
    }
}
