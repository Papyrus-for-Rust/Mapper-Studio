use crate::{
    monument_attributes::Attributes,
    monument_source::{Prefab, Recipe},
};
use papyrus_terrain::{monument_eligibility::Check, native_layers::Layers, spawn};

pub const VERSION: &str = "papyrus-land-gradient-alignment-1";
pub const REQUIREMENT: &str = "DecorAlign requires terrain-relative rotation/scale";

/// The captured cave program turns its forward axis downhill while leaving
/// the root upright, at unit scale and at the original proposal position.
pub fn program<'a>(recipe: Option<&Recipe>, prefab: &'a Prefab) -> Option<&'a Attributes> {
    if recipe?.land_alignment.as_deref() != Some(VERSION)
        || !prefab
            .rules
            .unsupported
            .iter()
            .any(|rule| rule == REQUIREMENT)
    {
        return None;
    }
    let attributes = prefab.placement_attributes.as_ref()?;
    let [align] = attributes.root_decor.as_slice() else {
        return None;
    };
    (align.kind == "DecorAlign"
        && align.values == [0., 1., 0., 0., 0., 1., 1., 1.]
        && attributes.issues.is_empty()
        && attributes.water_checks.is_empty())
    .then_some(attributes)
}

pub fn yaw(layers: &Layers, x: f32, z: f32) -> Result<f32, String> {
    let [u, v] = [x, z].map(|value| value / layers.size as f32 + 0.5);
    let [nx, ny, nz] = papyrus_terrain::terrain_normal::sample(
        layers.size,
        layers.height_resolution,
        &layers.height,
        [u, v],
    )?;
    // Rust uses cross(normal, cross(normal, up)) as the forward direction.
    // On exactly flat terrain it substitutes forward for the inner cross.
    let forward = if nx == 0. && nz == 0. {
        [1., 0.]
    } else {
        [nx * ny, nz * ny]
    };
    Ok(-forward[0].atan2(forward[1]).to_degrees())
}

#[allow(clippy::too_many_arguments)]
pub fn check_terrain(
    layers: &Layers,
    attributes: &Attributes,
    filter: &spawn::Filter,
    pos: [f32; 3],
    yaw: f32,
    work: &mut usize,
    limit: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Check>, String> {
    for check in &attributes.terrain_checks {
        if cancelled() {
            return Ok(None);
        }
        if *work >= limit {
            return Ok(Some(Check::Budget));
        }
        *work += 1;
        let [x, y, z] = crate::coastal_placement::world(
            check.position,
            pos,
            if check.rotate { yaw } else { 0. },
        );
        let [u, v] = [x, z].map(|value| value / layers.size as f32 + 0.5);
        if !(0. ..=1.).contains(&u)
            || !(0. ..=1.).contains(&v)
            || spawn::sample_factor(layers, filter, u, v)? == 0.
            || (spawn::sample_height(layers, u, v)? - y).abs() > check.extents
        {
            return Ok(Some(Check::Rejected(
                "Authored aligned monument terrain check failed",
            )));
        }
    }
    Ok(Some(Check::Accepted(pos)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_gradient_alignment_preserves_upright_yaw_on_flat_and_sloped_terrain() {
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 0,
            biome_seed: 0,
            height_resolution: 3,
            height: vec![16_000; 9],
            surface_resolution: 0,
            splat: vec![],
            biome: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        assert_eq!(yaw(&layers, 0., 0.).unwrap(), -90.);
        for row in layers.height.chunks_exact_mut(3) {
            row.copy_from_slice(&[16_000, 16_500, 17_000]);
        }
        assert_eq!(yaw(&layers, 0., 0.).unwrap(), 90.);
        assert!(yaw(&layers, 1000., 0.).is_err());
    }
}
