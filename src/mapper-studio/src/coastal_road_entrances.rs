//! Versioned dry-ground admission at coastal road connectors.
use crate::{Project, infrastructure, monument_attributes::Attributes, prefab_assets::Catalog};
use papyrus_terrain::{erosion::decode, monument_eligibility::Check, native_layers::Layers};

#[derive(Clone)]
pub(crate) struct Prepared {
    projected: bool,
    lattice_margin: f32,
    lattice: Option<papyrus_terrain::road_costmap::CellLattice>,
    support_grid: bool,
    families: Vec<(i32, f32, f32)>,
    minimum_height: f32,
    spacing: f32,
}
impl Prepared {
    pub fn land_access(
        &self,
        layers: &Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<papyrus_terrain::road_land_access::Access>, String> {
        let Some(lattice) = self.lattice else {
            return Ok(None);
        };
        papyrus_terrain::road_land_access::Access::prepare(
            layers,
            lattice,
            self.minimum_height,
            64_000_000,
            cancelled,
        )
    }
    pub fn links_land(&self, attributes: &Attributes) -> bool {
        attributes.path_connectors.iter().any(|connector| {
            self.families
                .iter()
                .any(|(kind, _, _)| *kind == connector.infrastructure_type)
        })
    }
    /// Coastal pivots may be offshore. Admit their authored road exits against
    /// the shared base-terrain land network before accepting a local stamp.
    pub fn check_land_access(
        &self,
        access: &papyrus_terrain::road_land_access::Access,
        attributes: &Attributes,
        position: [f32; 3],
        yaw: f32,
        work: &mut usize,
        maximum_work: usize,
    ) -> Check {
        for connector in &attributes.path_connectors {
            if *work >= maximum_work {
                return Check::Budget;
            }
            *work += 1;
            if self
                .families
                .iter()
                .any(|(kind, _, _)| *kind == connector.infrastructure_type)
            {
                let point = crate::coastal_placement::world(connector.position, position, yaw);
                if !access.admits([point[0], point[2]]) {
                    return Check::Rejected(
                        "Coastal road entrance is outside the main land network",
                    );
                }
            }
        }
        Check::Accepted(position)
    }
    pub fn projects_height(&self) -> bool {
        self.projected
    }
    pub fn prepare(
        project: &Project,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let Some(recipe) = &project.roads else {
            return Ok(None);
        };
        let Some(costs) = recipe
            .access_costmap
            .as_ref()
            .filter(|c| c.coastal_entrances.is_some())
        else {
            return Ok(None);
        };
        costs.validate()?;
        let source = infrastructure::Source::cached_shared(catalog, recipe, &cancelled)?;
        costs.check(&source)?;
        let mut families = vec![(
            source.profile.road_type,
            source.profile.width,
            source.profile.shoulder,
        )];
        if recipe.include_trails.unwrap_or(true)
            && let Some(trails) = &source.profile.trails
        {
            families.push((trails.infrastructure_type, trails.width, trails.shoulder));
        }
        let lattice = if matches!(
            costs.coastal_entrances.as_deref(),
            Some(
                crate::road_cost_source::LATTICE_COASTAL_ENTRANCES_VERSION
                    | crate::road_cost_source::GROUND_GUIDED_COASTAL_ENTRANCES_VERSION
            )
        ) {
            Some(papyrus_terrain::road_costmap::CellLattice::new(
                project.size as f32,
                costs
                    .joins
                    .as_ref()
                    .ok_or("Missing coastal road source lattice")?
                    .source_grid_spacing,
            )?)
        } else {
            None
        };
        let projected = lattice.is_some()
            || costs.coastal_entrances.as_deref()
                == Some(crate::road_cost_source::PROJECTED_COASTAL_ENTRANCES_VERSION);
        let lattice_margin = if projected {
            costs
                .joins
                .as_ref()
                .ok_or("Missing coastal road source lattice")?
                .source_grid_spacing
                * std::f32::consts::FRAC_1_SQRT_2
        } else {
            0.
        };
        Ok(Some(Self {
            projected,
            lattice_margin,
            lattice,
            support_grid: costs.coastal_entrances.as_deref()
                == Some(crate::road_cost_source::GROUND_GUIDED_COASTAL_ENTRANCES_VERSION),
            families,
            minimum_height: source
                .profile
                .entrances
                .ok_or("Missing coastal road entrance height policy")?
                .minimum_height,
            spacing: recipe
                .entrance_launch
                .ok_or("Missing coastal road entrance sampling policy")?
                .sample_spacing,
        }))
    }
    #[allow(clippy::too_many_arguments)] // Explicit candidate, terrain and shared sampling allowance.
    pub fn check(
        &self,
        layers: &Layers,
        attributes: &Attributes,
        changes_height: bool,
        position: [f32; 3],
        yaw: f32,
        work: &mut usize,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Check>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = layers.height_resolution;
        if layers.size == 0
            || !(2..=4097).contains(&n)
            || layers.height.len() != n * n
            || !yaw.is_finite()
            || position.iter().any(|v| !v.is_finite())
        {
            return Err("Invalid coastal road entrance terrain or pose".into());
        }
        if changes_height {
            return Ok(Some(Check::Accepted(position)));
        }
        self.check_sampled(
            layers,
            attributes,
            position,
            yaw,
            work,
            maximum_work,
            1,
            |x, z| Ok(decode(layers.height[z * layers.height_resolution + x])),
            cancelled,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn check_projected(
        &self,
        layers: &Layers,
        attributes: &Attributes,
        projection: &papyrus_terrain::monument_composition::ProjectedHeight<'_, '_, '_>,
        position: [f32; 3],
        yaw: f32,
        work: &mut usize,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Check>, String> {
        if !self.projected {
            return Ok((!cancelled()).then_some(Check::Accepted(position)));
        }
        self.check_sampled(
            layers,
            attributes,
            position,
            yaw,
            work,
            maximum_work,
            4,
            |x, z| {
                let size = layers.size as f32;
                let n = (layers.height_resolution - 1) as f32;
                projection.sample_source(
                    [(x as f32 / n - 0.5) * size, 0., (z as f32 / n - 0.5) * size],
                    -500.,
                    1000.,
                )
            },
            cancelled,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn check_sampled(
        &self,
        layers: &Layers,
        attributes: &Attributes,
        position: [f32; 3],
        yaw: f32,
        work: &mut usize,
        maximum_work: usize,
        sample_work: usize,
        sample: impl Fn(usize, usize) -> Result<f32, String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Check>, String> {
        if cancelled() {
            return Ok(None);
        }
        let n = layers.height_resolution;
        if layers.size == 0
            || !(2..=4097).contains(&n)
            || layers.height.len() != n * n
            || (self.support_grid
                && (!(2..=2048).contains(&layers.surface_resolution)
                    || !(n - 1).is_multiple_of(layers.surface_resolution)))
            || !yaw.is_finite()
            || position.iter().any(|v| !v.is_finite())
        {
            return Err("Invalid coastal road entrance terrain or pose".into());
        }
        let size = layers.size as f32;
        let cell = size / (n - 1) as f32;
        let coordinate = |v: f32| {
            ((v / size + 0.5) * (n - 1) as f32)
                .floor()
                .clamp(0., (n - 1) as f32) as usize
        };
        for connector in &attributes.path_connectors {
            if *work >= maximum_work {
                return Ok(Some(Check::Budget));
            }
            *work += 1;
            let Some((_, width, shoulder)) = self
                .families
                .iter()
                .find(|(kind, _, _)| *kind == connector.infrastructure_type)
            else {
                continue;
            };
            let mut point = crate::coastal_placement::world(connector.position, position, yaw);
            let radius = if let Some(lattice) = self.lattice {
                // The source road begins at a cell centre. Scan its actual
                // endpoint tube, without requiring every possible cell offset
                // and sampling margin to be dry around the authored marker.
                let centre = lattice
                    .point(lattice.cell([point[0], point[2]])?)
                    .ok_or("Invalid coastal road source cell")?;
                point[0] = centre[0];
                point[2] = centre[1];
                width * 0.5 + shoulder
            } else {
                // Preserve the historical marker-centred, padded footprint.
                width * 0.5
                    + self.spacing
                    + cell
                    + self.lattice_margin
                    + if self.projected { *shoulder } else { 0. }
            };
            if point[0].abs() + radius > size * 0.5 || point[2].abs() + radius > size * 0.5 {
                return Ok(Some(Check::Rejected(
                    "Coastal road entrance reaches outside terrain",
                )));
            }
            let support_cell = size / layers.surface_resolution as f32;
            let scan_radius = radius
                + if self.support_grid {
                    support_cell * std::f32::consts::SQRT_2
                } else {
                    0.
                };
            for z in coordinate(point[2] - scan_radius)
                ..=(coordinate(point[2] + scan_radius) + 1).min(n - 1)
            {
                for x in coordinate(point[0] - scan_radius)
                    ..=(coordinate(point[0] + scan_radius) + 1).min(n - 1)
                {
                    if cancelled() {
                        return Ok(None);
                    }
                    if sample_work > maximum_work.saturating_sub(*work) {
                        return Ok(Some(Check::Budget));
                    }
                    *work += sample_work;
                    let wx = (x as f32 / (n - 1) as f32 - 0.5) * size;
                    let wz = (z as f32 / (n - 1) as f32 - 0.5) * size;
                    let touches = if self.support_grid {
                        // A low native vertex marks every source surface cell
                        // it touches in the routing support raster. Query that
                        // same closed rectangle without allocating a full raster
                        // for each projected monument candidate.
                        let sr = layers.surface_resolution;
                        let distance = |i: usize, p: f32| {
                            let low = (i.saturating_sub(1) * sr / (n - 1)) as f32 * support_cell
                                - size * 0.5;
                            let high = ((i * sr / (n - 1)).min(sr - 1) + 1) as f32 * support_cell
                                - size * 0.5;
                            (low - p).max(p - high).max(0.)
                        };
                        distance(x, point[0]).hypot(distance(z, point[2])) <= radius
                    } else {
                        (wx - point[0]).hypot(wz - point[2]) <= radius
                    };
                    if touches && sample(x, z)? <= self.minimum_height {
                        return Ok(Some(Check::Rejected(
                            "Coastal road entrance needs dry supporting terrain",
                        )));
                    }
                }
            }
        }
        Ok(Some(Check::Accepted(position)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coastal_pivots_use_rotated_road_exits_and_reject_isolated_land() {
        let n = 17;
        let encoded = |height: f32| ((height + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: n,
            surface_resolution: 16,
            height: vec![encoded(-10.); n * n],
            splat: vec![],
            biome: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        for z in 1..16 {
            for x in 1..7 {
                layers.height[z * n + x] = encoded(20.);
            }
        }
        for z in 6..11 {
            for x in 11..14 {
                layers.height[z * n + x] = encoded(20.);
            }
        }
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 25.).unwrap();
        let access =
            papyrus_terrain::road_land_access::Access::prepare(&layers, lattice, 1., 100000, || false)
                .unwrap()
                .unwrap();
        let mut attributes: Attributes = serde_json::from_value(serde_json::json!({
            "version":1,"root_decor":[],"terrain_checks":[],"water_checks":[],
            "path_connectors":[{"object_id":1,"active":true,
                "position":[-250,0,0],"infrastructure_type":9}],"issues":[]
        }))
        .unwrap();
        let data = Prepared {
            projected: true,
            lattice_margin: 0.,
            lattice: Some(lattice),
            support_grid: true,
            families: vec![(9, 4., 8.)],
            minimum_height: 1.,
            spacing: 1.,
        };
        assert!(!access.admits([0., 0.]));
        let check = |attributes: &Attributes, yaw, limit| {
            data.check_land_access(&access, attributes, [0.; 3], yaw, &mut 0, limit)
        };
        assert!(matches!(check(&attributes, 0., 1), Check::Accepted(_)));
        assert!(matches!(check(&attributes, 180., 1), Check::Rejected(_)));
        assert!(matches!(check(&attributes, 0., 0), Check::Budget));
        attributes.path_connectors[0].infrastructure_type = 17;
        assert!(matches!(check(&attributes, 180., 1), Check::Accepted(_)));
    }
    #[test]
    fn dry_admission_follows_authored_roles_rotation_height_stamps_and_work_limits() {
        let (n, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let encoded = |height: f32| ((height + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: n,
            surface_resolution: sr,
            height: vec![encoded(20.); n * n],
            splat: vec![],
            biome: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let attributes: Attributes = serde_json::from_value(serde_json::json!({
            "version": 1,
            "root_decor": [],
            "terrain_checks": [],
            "water_checks": [],
            "path_connectors": [{
                "object_id": 1,
                "active": false,
                "position": [40, 0, 0],
                "infrastructure_type": 9
            }],
            "issues": []
        }))
        .unwrap();
        let data = Prepared {
            projected: false,
            lattice_margin: 0.,
            lattice: None,
            support_grid: false,
            families: vec![(9, 4., 8.)],
            minimum_height: 1.,
            spacing: 1.,
        };
        let z = ((40. / 1000. + 0.5) * (n - 1) as f32) as usize;
        layers.height[z * n + n / 2] = encoded(-1.);
        let check = |yaw, height, limit, cancel| {
            data.check(
                &layers,
                &attributes,
                height,
                [0.; 3],
                yaw,
                &mut 0,
                limit,
                || cancel,
            )
            .unwrap()
        };
        assert!(matches!(
            check(0., false, 1000, false),
            Some(Check::Accepted(_))
        ));
        assert!(matches!(
            check(90., false, 1000, false),
            Some(Check::Rejected(_))
        ));
        assert!(matches!(
            check(90., true, 1000, false),
            Some(Check::Accepted(_))
        ));
        assert!(matches!(check(90., false, 0, false), Some(Check::Budget)));
        assert!(check(90., false, 1000, true).is_none());
        let terrain =
            papyrus_terrain::monument_composition::HeightTerrain::new(&layers.height, n, 1000).unwrap();
        let pixel = [0u8; 4];
        let mut stamp = papyrus_terrain::monument_composition::Stamp {
            source_texture_sampling: false,
            source_cliff_generation: false,
            source_quad_raster: false,
            source_height_blending: false,
            position: [0., 20., 0.],
            yaw_degrees: 90.,
            size: [200., 1., 200.],
            offset: [0.; 3],
            radius: 100.,
            fade: 0.,
            height: Some(papyrus_terrain::monument_composition::Image {
                width: 1,
                height: 1,
                rgba: &pixel,
            }),
            blend: None,
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
        let current = Prepared {
            projected: true,
            lattice_margin: 7.5 * std::f32::consts::FRAC_1_SQRT_2,
            lattice: None,
            support_grid: false,
            families: vec![(9, 4., 8.)],
            minimum_height: 1.,
            spacing: 1.,
        };
        let run = |stamp: &papyrus_terrain::monument_composition::Stamp<'_>, data: &Prepared, limit| {
            let projected = terrain.projected(stamp).unwrap();
            data.check_projected(
                &layers,
                &attributes,
                &projected,
                [0.; 3],
                90.,
                &mut 0,
                limit,
                || false,
            )
            .unwrap()
        };
        assert!(matches!(
            run(&stamp, &current, 10000),
            Some(Check::Accepted(_))
        ));
        stamp.position[1] = 0.;
        assert!(matches!(
            run(&stamp, &current, 10000),
            Some(Check::Rejected(_))
        ));
        assert!(matches!(
            run(&stamp, &data, 10000),
            Some(Check::Accepted(_))
        ));
        assert!(matches!(run(&stamp, &current, 1), Some(Check::Budget)));
        let ignored = Prepared {
            projected: false,
            lattice_margin: 0.,
            lattice: None,
            support_grid: false,
            families: vec![(17, 10., 8.)],
            minimum_height: 1.,
            spacing: 1.,
        };
        assert!(matches!(
            ignored
                .check(
                    &layers,
                    &attributes,
                    false,
                    [0.; 3],
                    90.,
                    &mut 0,
                    1000,
                    || false
                )
                .unwrap(),
            Some(Check::Accepted(_))
        ));
    }
    #[test]
    fn lattice_admission_checks_the_endpoint_tube_without_worst_case_padding() {
        let (n, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let encoded = |height: f32| ((height + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: n,
            surface_resolution: sr,
            height: vec![encoded(20.); n * n],
            splat: vec![],
            biome: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let attributes: Attributes = serde_json::from_value(serde_json::json!({
            "version":1,"root_decor":[],"terrain_checks":[],"water_checks":[],
            "path_connectors":[{"object_id":1,"active":true,
                "position":[40,0,0],"infrastructure_type":9}],"issues":[]
        }))
        .unwrap();
        let lattice = papyrus_terrain::road_costmap::CellLattice::new(1000., 7.5).unwrap();
        let data = Prepared {
            projected: true,
            lattice_margin: 7.5 * std::f32::consts::FRAC_1_SQRT_2,
            lattice: Some(lattice),
            support_grid: false,
            families: vec![(9, 4., 8.)],
            minimum_height: 1.,
            spacing: 1.,
        };
        let historical = Prepared {
            lattice: None,
            ..data.clone()
        };
        let vertex = |v: f32| ((v / 1000. + 0.5) * (n - 1) as f32).round() as usize;
        // Water inside the historical padded disk, outside the source endpoint
        // tube. The nondivisible lattice shifts this rotated marker as well.
        layers.height[vertex(40.) * n + vertex(14.)] = encoded(-1.);
        let check = |layers: &Layers, data: &Prepared, limit, cancel| {
            data.check(
                layers,
                &attributes,
                false,
                [0.; 3],
                90.,
                &mut 0,
                limit,
                || cancel,
            )
            .unwrap()
        };
        assert!(matches!(
            check(&layers, &historical, 10000, false),
            Some(Check::Rejected(_))
        ));
        assert!(matches!(
            check(&layers, &data, 10000, false),
            Some(Check::Accepted(_))
        ));
        let centre = lattice.point(lattice.cell([0., 40.]).unwrap()).unwrap();
        let support = Prepared {
            support_grid: true,
            ..data.clone()
        };
        let low = vertex(centre[1]) * n + vertex(10.75);
        layers.height[low] = encoded(-1.);
        assert!(matches!(
            check(&layers, &data, 10000, false),
            Some(Check::Accepted(_))
        ));
        assert!(matches!(
            check(&layers, &support, 10000, false),
            Some(Check::Rejected(_))
        ));
        layers.height[low] = encoded(20.);
        layers.height[vertex(centre[1]) * n + vertex(centre[0])] = encoded(-1.);
        assert!(matches!(
            check(&layers, &data, 10000, false),
            Some(Check::Rejected(_))
        ));
        assert!(matches!(
            check(&layers, &data, 0, false),
            Some(Check::Budget)
        ));
        assert!(check(&layers, &data, 10000, true).is_none());
    }
    #[test]
    fn historical_recipes_do_not_read_road_metadata_for_admission() {
        let (catalog, _) = crate::infrastructure::tests::fixture();
        assert!(
            Prepared::prepare(&Project::default(), &catalog, || false)
                .unwrap()
                .is_none()
        );
    }
}
