//! Exact projected terrain checks against immutable pre-road rail support.
use crate::retained_rail_height::RetainedHeight;
use papyrus_terrain::{
    monument_composition::projection::Projected,
    native_layers::Layers,
    roadside_selection::{Check, Failure},
};
pub(crate) fn assess(
    guard: &RetainedHeight,
    layers: &Layers,
    projection: &Projected<'_, '_, '_>,
    mut spend: impl FnMut(usize) -> Result<(), Failure>,
    cancelled: &impl Fn() -> bool,
) -> Result<Check, Failure> {
    let n = layers.height_resolution;
    if n < 2
        || layers.height.len() != n * n
        || projection.height_resolution() != n
        || projection.size() != layers.size
    {
        return Err(Failure::Evaluation(
            "Rail stamp protection grid differs".into(),
        ));
    }
    let Some(bounds) = projection.world_bounds() else {
        return Ok(Check::Accepted { height: 0. });
    };
    let vertex =
        |p: f32| ((p / layers.size as f32 + 0.5) * (n - 1) as f32).clamp(0., (n - 1) as f32);
    let [x0, z0, x1, z1] = [
        vertex(bounds[0]).floor() as usize,
        vertex(bounds[1]).floor() as usize,
        vertex(bounds[2]).ceil() as usize,
        vertex(bounds[3]).ceil() as usize,
    ];
    let samples = guard.samples();
    let search_cost = 2 * (samples.len().max(1).ilog2() as usize + 1);
    for z in z0..=z1 {
        if cancelled() {
            return Ok(Check::Cancelled);
        }
        spend(search_cost)?;
        let lo = samples.partition_point(|&i| i < z * n + x0);
        let hi = samples.partition_point(|&i| i <= z * n + x1);
        for &i in &samples[lo..hi] {
            if cancelled() {
                return Ok(Check::Cancelled);
            }
            spend(64)?;
            let after = projection.encoded_height(i % n, i / n).ok_or_else(|| {
                Failure::Evaluation("Invalid projected rail support vertex".into())
            })?;
            if after != layers.height[i] {
                return Ok(Check::Rejected(
                    "Roadside stamp changes retained rail terrain",
                ));
            }
        }
    }
    Ok(Check::Accepted { height: 0. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::{
        monument_composition::{self, Image, Stamp},
        path_terrain,
    };
    #[test]
    fn exact_rotated_height_stamps_reject_but_paint_only_and_unaffected_patches_remain_eligible() {
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![17000; hr * hr],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[..n * n].fill(255);
        layers.splat[..n * n].fill(255);
        let guard = RetainedHeight::test_capture(
            path_terrain::Grid {
                size: 1000.,
                resolution: hr,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &layers.height,
            },
            &[0, (hr / 2) * hr + hr / 2],
        );
        let alpha = vec![255; n * n];
        let source = monument_composition::projection::Terrain::new(&layers, &alpha, || false)
            .unwrap()
            .unwrap();
        let image = Image {
            width: 1,
            height: 1,
            rgba: &[0; 4],
        };
        let mut stamp = Stamp {
            source_cliff_generation: false,
            source_quad_raster: false,
            source_height_blending: false,
            source_texture_sampling: false,
            position: [0., 100., 0.],
            yaw_degrees: 0.,
            size: [100., 100., 100.],
            offset: [0.; 3],
            radius: 50.,
            fade: 0.,
            height: Some(image),
            blend: Some(Image {
                rgba: &[0, 0, 0, 255],
                ..image
            }),
            splat: None,
            splat_mask: 0,
            alpha: None,
            biome: None,
            biome_mask: 0,
            topology: None,
            topology_mask: 0,
            replace_topology: false,
            auto_cliff_splat: false,
            auto_cliff_topology: false,
        };
        for yaw in [0., 37., 90., 180., 270.] {
            stamp.yaw_degrees = yaw;
            let projection = source.projected(&stamp).unwrap();
            let committed = monument_composition::compose(
                &layers,
                &alpha,
                std::slice::from_ref(&stamp),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                projection.encoded_height(hr / 2, hr / 2),
                Some(committed.layers.height[(hr / 2) * hr + hr / 2])
            );
            assert!(matches!(
                assess(&guard, &layers, &projection, |_| Ok(()), &|| false).unwrap(),
                Check::Rejected(_)
            ));
        }
        stamp.height = None;
        stamp.blend = None;
        stamp.splat = Some([
            Image {
                rgba: &[0, 255, 0, 0],
                ..image
            },
            image,
        ]);
        stamp.splat_mask = 2;
        let projection = source.projected(&stamp).unwrap();
        assert!(matches!(
            assess(&guard, &layers, &projection, |_| Ok(()), &|| false).unwrap(),
            Check::Accepted { .. }
        ));
        assert!(matches!(
            assess(
                &guard,
                &layers,
                &projection,
                |_| Err(Failure::Budget),
                &|| false
            ),
            Err(Failure::Budget)
        ));
        assert!(matches!(
            assess(
                &guard,
                &layers,
                &projection,
                |_| panic!("Cancelled check spent work"),
                &|| true
            )
            .unwrap(),
            Check::Cancelled
        ));
        stamp.height = Some(image);
        stamp.blend = Some(Image {
            rgba: &[0, 0, 0, 255],
            ..image
        });
        stamp.position = [300., 100., 300.];
        let projection = source.projected(&stamp).unwrap();
        assert!(matches!(
            assess(&guard, &layers, &projection, |_| Ok(()), &|| false).unwrap(),
            Check::Accepted { .. }
        ));
        assert!(layers.height.iter().all(|&h| h == 17000));
        assert_eq!(projection.encoded_height(hr, hr), None);
    }
}
