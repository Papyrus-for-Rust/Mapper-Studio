//! Dungeon-grid coordinates and authored cell compatibility.
//! Routing, burial depth and entrance-link assembly consume these same frames.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    North = 0,
    South = 1,
    West = 2,
    East = 3,
}
impl Direction {
    pub const ALL: [Self; 4] = [Self::North, Self::South, Self::West, Self::East];

    pub fn opposite(self) -> Self {
        match self {
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Grid {
    count: usize,
    cell_size: f32,
    inverse: f32,
    offset: f32,
}
impl Grid {
    pub fn new(world_size: f32, cell_size: f32) -> Result<Self, String> {
        if !world_size.is_finite()
            || !(1. ..=10000.).contains(&world_size)
            || !cell_size.is_finite()
            || cell_size <= 0.
        {
            return Err("Invalid dungeon-grid dimensions".into());
        }
        let inverse = 1. / cell_size;
        let count = (world_size.max(1000.) * inverse).floor();
        if !(1. ..=256.).contains(&count) {
            return Err("Dungeon-grid dimensions exceed the supported allocation".into());
        }
        Ok(Self {
            count: count as usize,
            cell_size,
            inverse,
            offset: count * 0.5 - 0.5,
        })
    }

    pub fn count(self) -> usize {
        self.count
    }

    pub fn world(self, cell: [usize; 2]) -> Result<[f32; 3], String> {
        if cell.iter().any(|&value| value >= self.count) {
            return Err("Dungeon-grid cell is outside its lattice".into());
        }
        Ok([
            (cell[0] as f32 - self.offset) * self.cell_size,
            0.,
            (cell[1] as f32 - self.offset) * self.cell_size,
        ])
    }

    pub fn cell(self, position: [f32; 3]) -> Result<[usize; 2], String> {
        if position.iter().any(|value| !value.is_finite()) {
            return Err("Invalid dungeon-grid world position".into());
        }
        Ok([position[0], position[2]].map(|value| {
            (value * self.inverse + self.offset)
                .round_ties_even()
                .clamp(0., (self.count - 1) as f32) as usize
        }))
    }

    pub fn neighbour(self, cell: [usize; 2], direction: Direction) -> Option<[usize; 2]> {
        if cell.iter().any(|&value| value >= self.count) {
            return None;
        }
        let [x, z] = cell;
        match direction {
            Direction::North => z.checked_add(1).filter(|&z| z < self.count).map(|z| [x, z]),
            Direction::South => z.checked_sub(1).map(|z| [x, z]),
            Direction::West => x.checked_sub(1).map(|x| [x, z]),
            Direction::East => x.checked_add(1).filter(|&x| x < self.count).map(|x| [x, z]),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub id: u32,
    /// North, south, west and east, in that order. Zero closes a side.
    pub connections: [u8; 4],
    pub variants: [u8; 4],
    pub avoid_neighbours: Vec<u32>,
}
impl Cell {
    pub fn matches(&self, required: [bool; 4], neighbours: [Option<&Self>; 4]) -> bool {
        Direction::ALL.into_iter().all(|direction| {
            let side = direction as usize;
            let connection = self.connections[side];
            match neighbours[side] {
                Some(other) => {
                    connection == other.connections[direction.opposite() as usize]
                        && (connection == 0 || !self.avoid_neighbours.contains(&other.id))
                }
                None => (connection != 0) == required[side],
            }
        })
    }
}

/// The caller supplies the deterministic shuffled order for this cell. Keep
/// a compatible fallback while preferring matching west/south visual variants.
pub fn choose<'a>(
    candidates: impl IntoIterator<Item = &'a Cell>,
    required: [bool; 4],
    neighbours: [Option<&Cell>; 4],
) -> Option<&'a Cell> {
    choose_checked(candidates, required, neighbours, |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {})
}

/// Visit each considered candidate before checking its authored constraints.
/// A visit failure discards the compatible fallback and stops selection.
pub fn choose_checked<'a, E>(
    candidates: impl IntoIterator<Item = &'a Cell>,
    required: [bool; 4],
    neighbours: [Option<&Cell>; 4],
    mut visit: impl FnMut(&Cell) -> Result<(), E>,
) -> Result<Option<&'a Cell>, E> {
    let mut fallback = None;
    for candidate in candidates {
        visit(candidate)?;
        if !candidate.matches(required, neighbours) {
            continue;
        }
        fallback = Some(candidate);
        if [Direction::West, Direction::South]
            .into_iter()
            .all(|direction| {
                neighbours[direction as usize].is_none_or(|other| {
                    candidate.variants[direction as usize]
                        == other.variants[direction.opposite() as usize]
                })
            })
        {
            break;
        }
    }
    Ok(fallback)
}

/// Transition modules occupy the boundary between two unlike visual variants.
pub fn transition_matches(
    candidate: &Cell,
    first: &Cell,
    second: &Cell,
    direction: Direction,
) -> bool {
    let side = direction as usize;
    let opposite = direction.opposite() as usize;
    candidate.connections[opposite] == first.connections[side]
        && candidate.connections[side] == second.connections[opposite]
        && candidate.variants[opposite] == first.variants[side]
        && candidate.variants[side] == second.variants[opposite]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straight(id: u32, variant: u8) -> Cell {
        Cell {
            id,
            connections: [0, 0, 1, 1],
            variants: [variant; 4],
            avoid_neighbours: Vec::new(),
        }
    }

    #[test]
    fn centred_downward_lattice_roundtrips_and_matches_even_ties() {
        let grid = Grid::new(4000., 216.).unwrap();
        assert_eq!(grid.count(), 18);
        assert_eq!(grid.world([0, 0]).unwrap(), [-1836., 0., -1836.]);
        assert_eq!(grid.cell([0., 0., 0.]).unwrap(), [8, 8]);
        assert_eq!(grid.cell([216., 0., 216.]).unwrap(), [10, 10]);
        assert_eq!(grid.cell([-1e10, 0., 1e10]).unwrap(), [0, 17]);
        for z in 0..grid.count() {
            for x in 0..grid.count() {
                assert_eq!(grid.cell(grid.world([x, z]).unwrap()).unwrap(), [x, z]);
            }
        }
        assert_eq!(Grid::new(4250., 216.).unwrap().count(), 19);
        assert_eq!(Grid::new(500., 216.).unwrap().count(), 4);
        assert!(Grid::new(4000., 0.).is_err());
        assert!(Grid::new(f32::NAN, 216.).is_err());
        assert!(Grid::new(4000., 1.).is_err());
        assert!(grid.world([18, 0]).is_err());
        assert!(grid.cell([0., f32::NAN, 0.]).is_err());
        assert_eq!(grid.neighbour([0, 0], Direction::West), None);
        assert_eq!(grid.neighbour([0, 0], Direction::North), Some([0, 1]));
    }

    #[test]
    fn cell_selection_respects_connection_types_exclusions_and_variant_fallback() {
        let west = straight(1, 1);
        let first = straight(2, 0);
        let second = straight(3, 1);
        let required = [false, false, true, true];
        let neighbours = [None, None, Some(&west), None];
        assert_eq!(
            choose([&first, &second], required, neighbours).unwrap().id,
            3
        );
        assert_eq!(choose([&first], required, neighbours).unwrap().id, 2);
        let mut excluded = second.clone();
        excluded.avoid_neighbours.push(1);
        assert!(!excluded.matches(required, neighbours));
        let mut incompatible = second.clone();
        incompatible.connections[2] = 2;
        assert!(!incompatible.matches(required, neighbours));
        assert!(choose([&second], [false; 4], [None; 4]).is_none());
        let mut transition = first.clone();
        transition.variants[3] = 1;
        assert!(transition_matches(
            &transition,
            &first,
            &second,
            Direction::East
        ));
        assert!(!transition_matches(
            &transition,
            &second,
            &first,
            Direction::East
        ));
    }
}
