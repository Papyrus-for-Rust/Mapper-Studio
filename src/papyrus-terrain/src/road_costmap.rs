//! Source-configured road cost rules on Mapper's endpoint routing grid.
//! The sampler/router are independent; official SeedRandom/path parity is not claimed.
use crate::rail_costmap::{Rows, Terrain, sample_height_slope};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-road-costmap-1";
pub const CELL_VERSION: &str = "papyrus-road-costmap-cell-centres-2";
pub const NATIVE_SAMPLING_VERSION: &str = "papyrus-road-native-sampling-1";
/// Source lattice contract, separate from legacy endpoint/vertex grids.
/// The requested spacing is installation metadata, not a universal constant.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CellLattice {
    size: f32,
    requested_spacing: f32,
    resolution: usize,
}
impl CellLattice {
    pub fn new(size: f32, requested_spacing: f32) -> Result<Self, String> {
        if !size.is_finite()
            || !(1. ..=6000.).contains(&size)
            || !requested_spacing.is_finite()
            || !(0.1..=128.).contains(&requested_spacing)
        {
            return Err("Invalid source road lattice dimensions".into());
        }
        let resolution = (size / requested_spacing) as usize;
        if !(2..=1024).contains(&resolution) {
            return Err("Source road lattice exceeds supported resolution".into());
        }
        Ok(Self {
            size,
            requested_spacing,
            resolution,
        })
    }
    pub fn resolution(self) -> usize {
        self.resolution
    }
    pub fn size(self) -> f32 {
        self.size
    }
    pub fn spacing(self) -> f32 {
        self.size / self.resolution as f32
    }
    pub fn normalized(self, cell: [usize; 2]) -> Option<[f32; 2]> {
        cell.iter()
            .all(|&i| i < self.resolution)
            .then(|| cell.map(|i| (i as f32 + 0.5) / self.resolution as f32))
    }
    /// TerrainMeta.Denormalize uses origin + normalized * size, in that order.
    pub fn point(self, cell: [usize; 2]) -> Option<[f32; 2]> {
        self.normalized(cell)
            .map(|uv| uv.map(|v| -self.size * 0.5 + v * self.size))
    }
    /// Source PathFinder.GetPoint floors normalized cell coordinates and clamps
    /// finite, bounded positions, including markers outside the terrain edge.
    pub fn cell(self, position: [f32; 2]) -> Result<[usize; 2], String> {
        if position.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
            return Err("Invalid source road endpoint".into());
        }
        Ok(position.map(|v| {
            (((v + self.size * 0.5) * (1. / self.size)) * self.resolution as f32)
                .clamp(0., (self.resolution - 1) as f32) as usize
        }))
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct CellGrid {
    pub lattice: CellLattice,
    pub grid: Grid,
}
/// Additional conservative routing restriction; original source costs remain
/// the acceptance grid. Never converts an originally blocked cell to walkable.
pub fn guard_ring(
    source: &CellGrid,
    radius: usize,
    maximum_work: usize,
    cancel: impl Fn() -> bool,
) -> Result<Option<(CellGrid, usize, usize)>, String> {
    let n = source.lattice.resolution();
    if !(1..=2).contains(&radius)
        || source.grid.engine != CELL_VERSION
        || source.grid.resolution != n
        || source.grid.costs.len() != n * n
        || maximum_work == 0
    {
        return Err("Invalid source ring clearance guard".into());
    }
    let mut guarded = source.clone();
    let mut work = 0usize;
    let mut added = 0;
    for z in 0..n {
        for x in 0..n {
            if cancel() {
                return Ok(None);
            }
            work = work
                .checked_add(1)
                .filter(|&v| v <= maximum_work)
                .ok_or("Ring clearance work allowance exhausted")?;
            if source.grid.costs[z * n + x] == 0 {
                continue;
            }
            let mut blocked = x < radius || z < radius || x + radius >= n || z + radius >= n;
            'neighbours: for b in z.saturating_sub(radius)..=(z + radius).min(n - 1) {
                for a in x.saturating_sub(radius)..=(x + radius).min(n - 1) {
                    work = work
                        .checked_add(1)
                        .filter(|&v| v <= maximum_work)
                        .ok_or("Ring clearance work allowance exhausted")?;
                    if source.grid.costs[b * n + a] == 0 {
                        blocked = true;
                        break 'neighbours;
                    }
                }
            }
            if blocked {
                guarded.grid.costs[z * n + x] = 0;
                added += 1;
            }
        }
    }
    // Counts describe the original source sampler. Added mask cells are returned
    // separately rather than being mislabeled as source slope/topology results.
    Ok(Some((guarded, added, work)))
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<String>,
    pub near_radius: f32,
    pub clearance_radius: f32,
    pub maximum_slope: f32,
    pub blocked_topology: u32,
    pub clearance_topology: u32,
    pub water_topology: u32,
    pub cliff_topology: u32,
    pub expensive_cost: u32,
    pub normal_base: u32,
    pub noise_min: u32,
    pub noise_max_exclusive: u32,
    pub slope_scale: f32,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .sampling
            .as_deref()
            .is_some_and(|v| v != NATIVE_SAMPLING_VERSION)
            || [self.near_radius, self.clearance_radius]
                .iter()
                .any(|v| !v.is_finite() || !(0.1..=128.).contains(v))
            || !self.maximum_slope.is_finite()
            || !(0.1..=89.).contains(&self.maximum_slope)
            || [
                self.blocked_topology,
                self.clearance_topology,
                self.water_topology,
                self.cliff_topology,
            ]
            .contains(&0)
            || !(1..=1_000_000).contains(&self.expensive_cost)
            || !(1..=1000).contains(&self.normal_base)
            || self.noise_min >= self.noise_max_exclusive
            || self.noise_max_exclusive > 1000
            || !self.slope_scale.is_finite()
            || !(0. ..=100.).contains(&self.slope_scale)
        {
            return Err("Invalid source road cost policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counts {
    pub blocked_slope: usize,
    pub blocked_topology: usize,
    pub blocked_clearance: usize,
    pub blocked_trail_water: usize,
    pub water: usize,
    pub cliff: usize,
    pub placement: usize,
    pub normal: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Grid {
    pub engine: &'static str,
    pub resolution: usize,
    pub seed: u32,
    pub trail: bool,
    pub counts: Counts,
    pub work: usize,
    pub costs: Vec<u32>,
}

#[derive(Clone, Copy)]
enum TerrainCost {
    Slope,
    Topology,
    Clearance,
    Water,
    Cliff,
    Normal(u32),
}

#[derive(Clone, Copy)]
struct TerrainCell {
    cost: TerrainCost,
    work: u32,
}

struct TerrainCosts {
    setup_work: usize,
    cells: Vec<TerrainCell>,
}

/// Reuses slope and topology queries on a single immutable terrain revision.
/// Placement reservations and the noise stream still belong to each family.
pub struct CellTerrain<'a> {
    layers: &'a crate::native_layers::Layers,
    policy: Policy,
    lattice: CellLattice,
    terms: Option<TerrainCosts>,
}

impl<'a> CellTerrain<'a> {
    pub fn new(
        layers: &'a crate::native_layers::Layers,
        policy: Policy,
        lattice: CellLattice,
    ) -> Result<Self, String> {
        if layers.size as f32 != lattice.size {
            return Err("Road lattice and terrain size differ".into());
        }
        Ok(Self {
            layers,
            policy,
            lattice,
            terms: None,
        })
    }

    pub fn build(
        &mut self,
        placement: &[u8],
        seed: u32,
        trail: bool,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<CellGrid>, String> {
        let layers = self.layers;
        let lattice = self.lattice;
        let terrain = Terrain {
            size: layers.size as f32,
            height_resolution: layers.height_resolution,
            height: &layers.height,
            topology_resolution: layers.surface_resolution,
            topology: &layers.topology,
            placement_resolution: layers.surface_resolution,
            placement,
        };
        Ok(build_inner(
            terrain,
            &self.policy,
            lattice.resolution,
            Some(lattice),
            seed,
            trail,
            maximum_work,
            Some(&mut self.terms),
            cancelled,
        )?
        .map(|grid| CellGrid { lattice, grid }))
    }
}

/// Zero is blocked. Topology uses the inspected circular query convention;
/// placement uses its square query. Every cell consumes independent noise,
/// including blocked cells, so changing a filter cannot shift later randomness.
pub fn build(
    t: Terrain<'_>,
    p: &Policy,
    resolution: usize,
    seed: u32,
    trail: bool,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    build_inner(
        t,
        p,
        resolution,
        None,
        seed,
        trail,
        maximum_work,
        None,
        cancelled,
    )
}
/// Explicit opt-in cell-centred costs. The engine tag prevents an old fixed-grid
/// consumer from accepting these cells as vertex samples by accident.
pub fn build_cell_centred(
    t: Terrain<'_>,
    p: &Policy,
    lattice: CellLattice,
    seed: u32,
    trail: bool,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<CellGrid>, String> {
    if t.size != lattice.size {
        return Err("Road lattice and terrain size differ".into());
    }
    Ok(build_inner(
        t,
        p,
        lattice.resolution,
        Some(lattice),
        seed,
        trail,
        maximum_work,
        None,
        cancelled,
    )?
    .map(|grid| CellGrid { lattice, grid }))
}
#[allow(clippy::too_many_arguments)] // Explicit legacy/source coordinate contract.
fn build_inner(
    t: Terrain<'_>,
    p: &Policy,
    resolution: usize,
    lattice: Option<CellLattice>,
    seed: u32,
    trail: bool,
    maximum_work: usize,
    memo: Option<&mut Option<TerrainCosts>>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Grid>, String> {
    p.validate()?;
    let native = p.sampling.is_some();
    if native && (lattice.is_none() || t.size.fract() != 0. || !(1000. ..=6000.).contains(&t.size))
    {
        return Err("Native road sampling requires a cell-centred native map".into());
    }
    if !t.size.is_finite()
        || !(1. ..=6000.).contains(&t.size)
        || !(2..=4097).contains(&t.height_resolution)
        || t.height.len() != t.height_resolution.pow(2)
        || !(2..=2048).contains(&t.topology_resolution)
        || t.topology.len() != t.topology_resolution.pow(2)
        || !(2..=2048).contains(&t.placement_resolution)
        || t.placement.len() != t.placement_resolution.pow(2)
        || !(2..=1024).contains(&resolution)
        || !(1..=128_000_000).contains(&maximum_work)
    {
        return Err("Invalid road cost grids or work allowance".into());
    }
    let cached = memo.as_deref().and_then(Option::as_ref);
    let mut work = cached.map_or(0, |costs| costs.setup_work);
    if work > maximum_work {
        return Err("Road cost work allowance reached".into());
    }
    let topology = if cached.is_none() {
        for chunk in t.height.chunks(4096) {
            if cancelled() {
                return Ok(None);
            }
            work += chunk.len();
            if work > maximum_work {
                return Err("Road cost work allowance reached".into());
            }
            if chunk.iter().any(|v| !(0..=32766).contains(v)) {
                return Err("Invalid road height sample".into());
            }
        }
        let Some(rows) = Rows::new(
            t.topology_resolution,
            |i| {
                let f = t.topology[i] as u32;
                [
                    f & p.blocked_topology != 0,
                    f & p.clearance_topology != 0,
                    f & p.water_topology != 0,
                    f & p.cliff_topology != 0,
                ]
            },
            &mut work,
            maximum_work,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(rows.with_reciprocal_radius(native))
    } else {
        None
    };
    let mut recorded = (cached.is_none() && memo.is_some()).then(|| TerrainCosts {
        setup_work: work,
        cells: Vec::with_capacity(resolution * resolution),
    });
    let mut invalid = false;
    let Some(placement) = Rows::new(
        t.placement_resolution,
        |i| {
            invalid |= t.placement[i] > 1;
            [t.placement[i] != 0]
        },
        &mut work,
        maximum_work,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    if invalid {
        return Err("Invalid road placement cell".into());
    }
    let placement = placement.with_reciprocal_radius(native);
    let mut grid = Grid {
        engine: if lattice.is_some() {
            CELL_VERSION
        } else {
            VERSION
        },
        resolution,
        seed,
        trail,
        counts: Counts::default(),
        work: 0,
        costs: Vec::with_capacity(resolution * resolution),
    };
    let mut random = seed;
    for z in 0..resolution {
        if cancelled() {
            return Ok(None);
        }
        for x in 0..resolution {
            if work >= maximum_work {
                return Err("Road cost work allowance reached".into());
            }
            work += 1;
            if native {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
            } else {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            }
            let noise = p.noise_min + random % (p.noise_max_exclusive - p.noise_min);
            let uv = if let Some(lattice) = lattice {
                lattice
                    .normalized([x, z])
                    .ok_or("Invalid source road cell")?
            } else {
                [
                    x as f32 / (resolution - 1) as f32,
                    z as f32 / (resolution - 1) as f32,
                ]
            };
            let term = if let Some(cached) = cached {
                let term = cached.cells[z * resolution + x];
                work = work
                    .checked_add(term.work as usize)
                    .filter(|&v| v <= maximum_work)
                    .ok_or("Road cost work allowance reached")?;
                term.cost
            } else {
                let before = work;
                let term = terrain_cost(
                    &t,
                    p,
                    topology.as_ref().unwrap(),
                    uv,
                    &mut work,
                    maximum_work,
                )?;
                if let Some(recorded) = &mut recorded {
                    recorded.cells.push(TerrainCell {
                        cost: term,
                        work: (work - before) as u32,
                    });
                }
                term
            };
            let cost = match term {
                TerrainCost::Slope => {
                    grid.counts.blocked_slope += 1;
                    0
                }
                TerrainCost::Topology => {
                    grid.counts.blocked_topology += 1;
                    0
                }
                TerrainCost::Clearance => {
                    grid.counts.blocked_clearance += 1;
                    0
                }
                TerrainCost::Water if trail => {
                    grid.counts.blocked_trail_water += 1;
                    0
                }
                TerrainCost::Water => {
                    grid.counts.water += 1;
                    p.expensive_cost
                }
                TerrainCost::Cliff => {
                    grid.counts.cliff += 1;
                    p.expensive_cost
                }
                TerrainCost::Normal(_)
                    if placement.query(
                        uv,
                        p.near_radius,
                        t.size,
                        false,
                        &mut work,
                        maximum_work,
                    )?[0] =>
                {
                    grid.counts.placement += 1;
                    p.expensive_cost
                }
                TerrainCost::Normal(slope_cost) => {
                    grid.counts.normal += 1;
                    p.normal_base + slope_cost + noise
                }
            };
            grid.costs.push(cost);
        }
    }
    if cancelled() {
        return Ok(None);
    }
    grid.work = work;
    if let (Some(memo), Some(recorded)) = (memo, recorded) {
        *memo = Some(recorded);
    }
    Ok(Some(grid))
}

fn terrain_cost(
    terrain: &Terrain<'_>,
    policy: &Policy,
    topology: &Rows<4>,
    uv: [f32; 2],
    work: &mut usize,
    limit: usize,
) -> Result<TerrainCost, String> {
    let size = terrain.size;
    let slope = if policy.sampling.is_some() {
        // Four diagonal-corner normals read three heights each.
        *work = work
            .checked_add(12)
            .filter(|&v| v <= limit)
            .ok_or("Road cost work allowance reached")?;
        crate::terrain_normal::slope(size as u32, terrain.height_resolution, terrain.height, uv)?
    } else {
        sample_height_slope(size, terrain.height_resolution, terrain.height, uv).1
    };
    if slope > policy.maximum_slope {
        return Ok(TerrainCost::Slope);
    }
    let near = topology.query(uv, policy.near_radius, size, true, work, limit)?;
    Ok(if near[0] {
        TerrainCost::Topology
    } else if topology.query(uv, policy.clearance_radius, size, true, work, limit)?[1] {
        TerrainCost::Clearance
    } else if near[2] {
        TerrainCost::Water
    } else if near[3] {
        TerrainCost::Cliff
    } else {
        TerrainCost::Normal((slope * slope * policy.slope_scale) as u32)
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_cell_terrain_replays_work_but_uses_each_familys_placement_and_seed() {
        let layers = crate::native_layers::Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 9,
            surface_resolution: 8,
            height: (0..81).map(|i| 16000 + (i * 33 % 600) as i16).collect(),
            topology: (0..64)
                .map(|i| {
                    if i % 7 == 0 {
                        16
                    } else if i % 13 == 0 {
                        8
                    } else {
                        0
                    }
                })
                .collect(),
            biome: vec![],
            splat: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut policy = policy();
        policy.sampling = Some(NATIVE_SAMPLING_VERSION.into());
        policy.maximum_slope = 85.;
        let lattice = CellLattice::new(1000., 70.).unwrap();
        let mut shared = CellTerrain::new(&layers, policy.clone(), lattice).unwrap();
        for (seed, trail, placement) in [
            (1, false, [0; 64]),
            (55, true, [1; 64]),
            (77, false, [1; 64]),
        ] {
            let terrain = Terrain {
                size: 1000.,
                height_resolution: layers.height_resolution,
                height: &layers.height,
                topology_resolution: layers.surface_resolution,
                topology: &layers.topology,
                placement_resolution: layers.surface_resolution,
                placement: &placement,
            };
            let reference =
                build_cell_centred(terrain, &policy, lattice, seed, trail, 10_000, || false)
                    .unwrap()
                    .unwrap();
            let expected_work = reference.grid.work;
            let actual = shared
                .build(&placement, seed, trail, expected_work, || false)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_vec(&actual).unwrap(),
                serde_json::to_vec(&reference).unwrap()
            );
            assert!(
                shared
                    .build(&placement, seed, trail, expected_work - 1, || false)
                    .is_err()
            );
            assert!(
                shared
                    .build(&placement, seed, trail, expected_work, || true)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn guarded_ring_only_adds_blockers_and_preserves_source_costs_and_lattice() {
        let lattice = CellLattice::new(1000., 100.).unwrap();
        let mut source = CellGrid {
            lattice,
            grid: Grid {
                engine: CELL_VERSION,
                resolution: 10,
                seed: 3,
                trail: false,
                counts: Counts::default(),
                work: 0,
                costs: vec![101; 100],
            },
        };
        source.grid.costs[55] = 0;
        source.grid.costs[33] = 5000;
        let before = source.grid.costs.clone();
        let (guarded, added, work) = guard_ring(&source, 1, 10000, || false).unwrap().unwrap();
        assert_eq!(guarded.lattice, lattice);
        assert_eq!(added, 44);
        assert!(work > 100 && work < 10000);
        for z in 0usize..10 {
            for x in 0usize..10 {
                let blocked = x == 0
                    || z == 0
                    || x == 9
                    || z == 9
                    || (x.abs_diff(5) <= 1 && z.abs_diff(5) <= 1);
                assert_eq!(
                    guarded.grid.costs[z * 10 + x],
                    if blocked { 0 } else { before[z * 10 + x] }
                );
            }
        }
        assert!(guard_ring(&source, 1, 1, || false).is_err());
        assert!(guard_ring(&source, 1, 10000, || true).unwrap().is_none());
        assert!(guard_ring(&source, 0, 10000, || false).is_err());
        source.grid.engine = VERSION;
        assert!(guard_ring(&source, 1, 10000, || false).is_err());
        assert_eq!(source.grid.costs, before);
    }
    fn policy() -> Policy {
        Policy {
            sampling: None,
            near_radius: 5.,
            clearance_radius: 15.,
            maximum_slope: 20.,
            blocked_topology: 4,
            clearance_topology: 8,
            water_topology: 16,
            cliff_topology: 2,
            expensive_cost: 5000,
            normal_base: 1,
            noise_min: 100,
            noise_max_exclusive: 200,
            slope_scale: 10.,
        }
    }
    #[test]
    fn native_noise_advances_through_blocked_cells_and_retains_water_branch_order() {
        let heights = vec![16000; 25];
        let placement = vec![0; 16];
        let mut p = policy();
        p.sampling = Some(NATIVE_SAMPLING_VERSION.into());
        let run = |topology: &[i32], seed, trail| {
            build_cell_centred(
                Terrain {
                    size: 1000.,
                    height_resolution: 5,
                    height: &heights,
                    topology_resolution: 4,
                    topology,
                    placement_resolution: 4,
                    placement: &placement,
                },
                &p,
                CellLattice::new(1000., 125.).unwrap(),
                seed,
                trail,
                1000000,
                || false,
            )
            .unwrap()
            .unwrap()
            .grid
        };
        let flat = vec![0; 16];
        let baseline = run(&flat, 1, false);
        assert_eq!(&baseline.costs[..4], [170, 190, 162, 196]);
        assert_eq!(run(&flat, 0, false).costs, [101; 64]);
        #[derive(Deserialize)]
        struct Reference {
            source_sha256: String,
            cases: Vec<Case>,
        }
        #[derive(Deserialize)]
        struct Case {
            seed: u32,
            values: Vec<u32>,
        }
        let reference: Reference = serde_json::from_str(include_str!(
            "../tests/fixtures/road-noise-global-44dd8a13.json"
        ))
        .unwrap();
        assert_eq!(
            reference.source_sha256,
            "44dd8a1323b12d7dd55aece202d8f2a587c75bd825cd218509f47c326cc409f0"
        );
        assert_eq!(reference.cases.len(), 5);
        for case in reference.cases {
            assert_eq!(case.values.len(), 64);
            assert_eq!(
                run(&flat, case.seed, false).costs,
                case.values.iter().map(|v| v + 1).collect::<Vec<_>>()
            );
        }
        let mut topology = flat.clone();
        topology[5] = 4;
        let blocked = run(&topology, 1, false);
        assert!(blocked.counts.blocked_topology > 0);
        for (actual, expected) in blocked.costs.iter().zip(&baseline.costs) {
            assert!(*actual == 0 || actual == expected);
        }
        topology[5] = 16;
        let paved = run(&topology, 1, false);
        let trail = run(&topology, 1, true);
        assert!(paved.counts.water > 0);
        for ((a, b), expected) in paved.costs.iter().zip(&trail.costs).zip(&baseline.costs) {
            assert_eq!(
                (*a, *b),
                if *a == 5000 {
                    (5000, 0)
                } else {
                    (*expected, *expected)
                }
            );
        }
        topology[5] |= 4;
        assert_eq!(run(&topology, 1, false).costs, blocked.costs);
    }
    #[test]
    fn native_sampling_validates_frame_and_charges_height_reads_before_work() {
        let heights = vec![16000; 25];
        let topology = vec![0; 16];
        let placement = vec![0; 16];
        let mut p = policy();
        p.sampling = Some(NATIVE_SAMPLING_VERSION.into());
        let terrain = || Terrain {
            size: 1000.,
            height_resolution: 5,
            height: &heights,
            topology_resolution: 4,
            topology: &topology,
            placement_resolution: 4,
            placement: &placement,
        };
        let lattice = CellLattice::new(1000., 125.).unwrap();
        let result = build_cell_centred(terrain(), &p, lattice, 1, false, 1000000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.grid.work, 25 + 16 + 16 + 64 * 13 + 64 * 3);
        assert!(build_cell_centred(terrain(), &p, lattice, 1, false, 69, || false).is_err());
        assert!(
            build_cell_centred(terrain(), &p, lattice, 1, false, 1000000, || true)
                .unwrap()
                .is_none()
        );
        assert!(build(terrain(), &p, 2, 1, false, 1000000, || false).is_err());
        p.sampling = Some("unknown".into());
        assert!(p.validate().is_err());
    }
    #[test]
    fn source_lattice_matches_cell_centres_floor_resolution_and_boundary_clamping() {
        for (size, expected) in [(1000., 133), (4000., 533), (5000., 666), (6000., 800)] {
            let lattice = CellLattice::new(size, 7.5).unwrap();
            assert_eq!(lattice.resolution(), expected);
            assert_eq!(lattice.spacing(), size / expected as f32);
            for cell in [[0, 0], [expected - 1, expected - 1], [expected / 2, 3]] {
                let uv = lattice.normalized(cell).unwrap();
                assert_eq!(uv, cell.map(|i| (i as f32 + 0.5) / expected as f32));
                let point = lattice.point(cell).unwrap();
                assert_eq!(point, uv.map(|v| -size * 0.5 + v * size));
                assert_eq!(lattice.cell(point).unwrap(), cell);
                assert!(point.iter().all(|v| v.abs() < size * 0.5));
            }
            assert_eq!(lattice.cell([-10000., 10000.]).unwrap(), [0, expected - 1]);
            assert!(lattice.point([expected, 0]).is_none());
        }
        assert!(CellLattice::new(5000., f32::NAN).is_err());
        assert!(CellLattice::new(6000., 0.1).is_err());
        assert!(CellLattice::new(1., 7.5).is_err());
        assert!(
            CellLattice::new(1000., 7.5)
                .unwrap()
                .cell([f32::INFINITY, 0.])
                .is_err()
        );
    }
    #[test]
    fn source_costs_sample_centres_and_do_not_reinterpret_legacy_vertices() {
        let h = vec![crate::erosion::encode(20.); 17 * 17];
        let mut top = vec![0; 64 * 64];
        for z in 0..64 {
            top[z * 64] = 4;
        }
        let placement = vec![0; 64 * 64];
        let mut p = policy();
        p.near_radius = 0.1;
        p.clearance_radius = 0.1;
        let terrain = || Terrain {
            size: 64.,
            height_resolution: 17,
            height: &h,
            topology_resolution: 64,
            topology: &top,
            placement_resolution: 64,
            placement: &placement,
        };
        let lattice = CellLattice::new(64., 8.).unwrap();
        let legacy = build(terrain(), &p, 8, 42, false, 200000, || false)
            .unwrap()
            .unwrap();
        let source = build_cell_centred(terrain(), &p, lattice, 42, false, 200000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(legacy.engine, VERSION);
        assert_eq!(source.grid.engine, CELL_VERSION);
        assert_eq!(source.lattice, lattice);
        for z in 0..8 {
            assert_eq!(legacy.costs[z * 8], 0);
            assert!(source.grid.costs[z * 8] > 0);
            for x in 1..8 {
                assert_eq!(legacy.costs[z * 8 + x], source.grid.costs[z * 8 + x]);
            }
        }
        assert!(
            build_cell_centred(terrain(), &p, lattice, 42, false, 200000, || true)
                .unwrap()
                .is_none()
        );
        assert!(build_cell_centred(terrain(), &p, lattice, 42, false, 1, || false).is_err());
        assert!(
            build_cell_centred(
                terrain(),
                &p,
                CellLattice::new(65., 8.).unwrap(),
                42,
                false,
                200000,
                || false
            )
            .is_err()
        );
        assert!(h.iter().all(|&v| v == crate::erosion::encode(20.)));
    }
    #[test]
    fn precedence_trail_water_and_independent_noise_are_repeatable() {
        let h = vec![crate::erosion::encode(20.); 17 * 17];
        let placement = vec![0; 16 * 16];
        let mut top = vec![0; 16 * 16];
        let p = policy();
        let run = |top: &[i32], placement: &[u8], trail| {
            build(
                Terrain {
                    size: 1000.,
                    height_resolution: 17,
                    height: &h,
                    topology_resolution: 16,
                    topology: top,
                    placement_resolution: 16,
                    placement,
                },
                &p,
                17,
                42,
                trail,
                100_000,
                || false,
            )
            .unwrap()
            .unwrap()
        };
        let a = run(&top, &placement, false);
        let b = run(&top, &placement, false);
        assert_eq!(a.costs, b.costs);
        assert!(a.costs.iter().all(|v| (101..201).contains(v)));
        for (flags, expected, trail) in [
            (4 | 16, 0, false),
            (8 | 2, 0, false),
            (16 | 2, 5000, false),
            (16 | 2, 0, true),
            (2, 5000, true),
        ] {
            top.fill(flags);
            let out = run(&top, &placement, trail);
            assert!(out.costs.iter().all(|v| *v == expected));
        }
        top.fill(0);
        let occupied = vec![1; 256];
        assert!(run(&top, &occupied, false).costs.iter().all(|v| *v == 5000));
    }
    #[test]
    fn slope_budget_cancellation_and_bad_cells_do_not_publish() {
        let p = policy();
        let top = vec![0; 256];
        let placement = vec![0; 256];
        let h: Vec<_> = (0..17 * 17)
            .map(|i| crate::erosion::encode(-400. + (i % 17) as f32 * 50.))
            .collect();
        let terrain = |placement| Terrain {
            size: 1000.,
            height_resolution: 17,
            height: &h,
            topology_resolution: 16,
            topology: &top,
            placement_resolution: 16,
            placement,
        };
        let out = build(terrain(&placement), &p, 17, 1, false, 100_000, || false)
            .unwrap()
            .unwrap();
        assert!(out.counts.blocked_slope > 0);
        assert!(build(terrain(&placement), &p, 17, 1, false, 100, || false).is_err());
        let calls = std::cell::Cell::new(0);
        assert!(
            build(terrain(&placement), &p, 17, 1, false, 100_000, || {
                calls.set(calls.get() + 1);
                calls.get() > 34
            })
            .unwrap()
            .is_none()
        );
        let mut invalid = placement.clone();
        invalid[200] = 2;
        assert!(build(terrain(&invalid), &p, 17, 1, false, 100_000, || false).is_err());
    }
}
