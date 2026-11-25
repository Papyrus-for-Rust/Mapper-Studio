//! Independent land-monument placement, preserving the retained C# baseline.
//! The host supplies ordered metadata and registry indices, never game assets.
use crate::erosion::decode;
use std::collections::HashSet;

pub const MAX_PREFABS: usize = 4096;
const MAX_ENTRIES: usize = 65536;
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Entry {
    pub group: usize,
    pub resolved: usize,
    pub land: bool,
    pub minimum_size: i32,
    pub extent_x: f32,
    pub extent_z: f32,
    pub priority: i32,
    pub minimum_yaw: f32,
    pub maximum_yaw: f32,
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub entry: usize,
    pub preference: u32,
    pub count: usize,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub enabled: bool,
    pub seed: i32,
    pub target: usize,
    pub include_large: bool,
    pub entries: Vec<Entry>,
    pub authored: Vec<usize>,
    pub catalog: Vec<usize>,
    pub policies: Vec<Policy>,
    pub stamps: crate::stamps::Request,
}

impl Request {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            seed: 0,
            target: 0,
            include_large: true,
            entries: vec![],
            authored: vec![],
            catalog: vec![],
            policies: vec![],
            stamps: crate::stamps::Request::default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.seed < 0
            || self.target > 100
            || self.entries.len() > MAX_ENTRIES
            || self.authored.len() > MAX_ENTRIES
            || self.catalog.len() > MAX_ENTRIES
            || self.policies.len() > MAX_PREFABS
        {
            return Err("invalid monument request limits".into());
        }
        for entry in &self.entries {
            if entry.group >= MAX_ENTRIES
                || entry.resolved >= self.entries.len()
                || entry.minimum_size < 0
                || !entry.extent_x.is_finite()
                || !entry.extent_z.is_finite()
                || entry.extent_x < 0.0
                || entry.extent_z < 0.0
                || !(entry.extent_x * entry.extent_x + entry.extent_z * entry.extent_z).is_finite()
                || !entry.minimum_yaw.is_finite()
                || !entry.maximum_yaw.is_finite()
                || !(entry.maximum_yaw - entry.minimum_yaw).is_finite()
            {
                return Err("invalid monument registry entry".into());
            }
            let resolved = &self.entries[entry.resolved];
            if resolved.resolved != entry.resolved || resolved.group != entry.group {
                return Err("invalid monument registry resolution".into());
            }
        }
        if self
            .authored
            .iter()
            .chain(&self.catalog)
            .any(|&i| i >= self.entries.len())
        {
            return Err("invalid monument candidate index".into());
        }
        let mut required = 0;
        for policy in &self.policies {
            if policy.entry >= self.entries.len()
                || policy.preference > 2
                || policy.count == 0
                || policy.count > MAX_PREFABS
            {
                return Err("invalid monument policy".into());
            }
            if policy.preference == 1 {
                required += policy.count;
            }
        }
        if required > MAX_PREFABS {
            return Err("required monument count exceeds the output budget".into());
        }
        self.stamps.validate(&self.entries)
    }

    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_INPUT_BYTES || bytes.get(..4) != Some(b"MAPM") {
            return Err("invalid monument input header".into());
        }
        let mut reader = Reader { bytes, cursor: 4 };
        if reader.word()? != 2 {
            return Err("unsupported monument input version".into());
        }
        let enabled = reader.boolean()?;
        let seed = reader.word()? as i32;
        let target = reader.word()? as usize;
        let include_large = reader.boolean()?;
        let count = reader.count(MAX_ENTRIES)?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(Entry {
                group: reader.word()? as usize,
                resolved: reader.word()? as usize,
                land: reader.boolean()?,
                minimum_size: reader.word()? as i32,
                extent_x: reader.float()?,
                extent_z: reader.float()?,
                priority: reader.word()? as i32,
                minimum_yaw: reader.float()?,
                maximum_yaw: reader.float()?,
            });
        }
        let authored = reader.indices()?;
        let catalog = reader.indices()?;
        let count = reader.count(MAX_PREFABS)?;
        let mut policies = Vec::with_capacity(count);
        for _ in 0..count {
            policies.push(Policy {
                entry: reader.word()? as usize,
                preference: reader.word()?,
                count: reader.word()? as usize,
            });
        }
        let stamps = crate::stamps::Request::read(&mut reader)?;
        if reader.cursor != bytes.len() {
            return Err("trailing monument input data".into());
        }
        let request = Self {
            enabled,
            seed,
            target,
            include_large,
            entries,
            authored,
            catalog,
            policies,
            stamps,
        };
        request.validate()?;
        Ok(request)
    }
}

pub(crate) struct Reader<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) cursor: usize,
}
impl Reader<'_> {
    pub(crate) fn word(&mut self) -> Result<u32, String> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 4)
            .ok_or("truncated monument input")?;
        self.cursor += 4;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| "invalid monument word")?,
        ))
    }
    pub(crate) fn float(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.word()?))
    }
    pub(crate) fn boolean(&mut self) -> Result<bool, String> {
        match self.word()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("invalid monument boolean".into()),
        }
    }
    pub(crate) fn count(&mut self, maximum: usize) -> Result<usize, String> {
        let value = self.word()? as usize;
        if value > maximum {
            return Err("monument input count exceeds limit".into());
        }
        Ok(value)
    }
    fn indices(&mut self) -> Result<Vec<usize>, String> {
        let count = self.count(MAX_ENTRIES)?;
        (0..count)
            .map(|_| self.word().map(|v| v as usize))
            .collect()
    }
    pub(crate) fn text(&mut self) -> Result<String, String> {
        let count = self.count(4096)?;
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + count)
            .ok_or("truncated stamp path")?;
        self.cursor += count;
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid stamp path encoding".into())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub entry: usize,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
}

pub fn generate(
    request: &Request,
    height: &[i16],
    resolution: usize,
    size: usize,
    world_seed: i32,
) -> Result<Vec<Placement>, String> {
    request.validate()?;
    if !(33..=4097).contains(&resolution)
        || !(resolution - 1).is_power_of_two()
        || height.len() != resolution * resolution
        || !(1000..=6000).contains(&size)
        || world_seed < 1
        || height.iter().any(|&h| !(0..=32766).contains(&h))
    {
        return Err("invalid monument height grid or world settings".into());
    }
    let mut placer = Placer {
        request,
        height,
        resolution,
        size,
        random: (if request.seed == 0 {
            world_seed
        } else {
            request.seed
        }) as u32
            ^ 0xC0FFEE,
        placed: vec![],
    };
    for policy in &request.policies {
        if policy.preference == 1 {
            placer.place(policy.entry, policy.count)?;
        }
    }
    if !request.enabled {
        return Ok(placer.placed);
    }
    let blocked: HashSet<usize> = request
        .policies
        .iter()
        .filter(|p| p.preference == 2)
        .map(|p| request.entries[p.entry].group)
        .collect();
    let authored: Vec<usize> = request
        .authored
        .iter()
        .copied()
        .filter(|&i| request.entries[i].land)
        .collect();
    let source = if authored.is_empty() {
        &request.catalog
    } else {
        &authored
    };
    let mut candidates: Vec<usize> = source
        .iter()
        .copied()
        .filter(|&i| {
            let entry = &request.entries[i];
            entry.minimum_size <= size as i32
                && (request.include_large || entry.extent_x.max(entry.extent_z) < 100.0)
                && !blocked.contains(&entry.group)
        })
        .collect();
    let target = if request.target > 0 {
        request.target
    } else {
        (size / 250).clamp(4, 24)
    };
    while placer.placed.len() < target && !candidates.is_empty() {
        let total: u32 = candidates
            .iter()
            .map(|&i| 1_u32 << request.entries[i].priority.clamp(0, 8))
            .sum();
        let mut draw = (next(&mut placer.random) % total) as i32;
        let mut chosen = *candidates.last().ok_or("empty monument candidates")?;
        for &index in &candidates {
            draw -= 1 << request.entries[index].priority.clamp(0, 8);
            if draw < 0 {
                chosen = index;
                break;
            }
        }
        let before = placer.placed.len();
        placer.place(chosen, 1)?;
        if placer.placed.len() == before {
            let group = request.entries[chosen].group;
            candidates.retain(|&index| request.entries[index].group != group);
        }
    }
    Ok(placer.placed)
}

struct Placer<'a> {
    request: &'a Request,
    height: &'a [i16],
    resolution: usize,
    size: usize,
    random: u32,
    placed: Vec<Placement>,
}
impl Placer<'_> {
    fn place(&mut self, index: usize, count: usize) -> Result<(), String> {
        let resolved = self.request.entries[index].resolved;
        let entry = &self.request.entries[resolved];
        if entry.minimum_size > self.size as i32 || (!entry.land && self.request.enabled) {
            return Ok(());
        }
        for _ in 0..count {
            let Some(placement) = self.find(resolved) else {
                return Ok(());
            };
            if self.placed.len() >= MAX_PREFABS {
                return Err("monument output exceeds the prefab budget".into());
            }
            self.placed.push(placement);
        }
        Ok(())
    }

    fn find(&mut self, index: usize) -> Option<Placement> {
        let entry = &self.request.entries[index];
        for _ in 0..10000 {
            let x = 16 + (next(&mut self.random) % (self.resolution - 32).max(1) as u32) as usize;
            let z = 16 + (next(&mut self.random) % (self.resolution - 32).max(1) as u32) as usize;
            let cell = z * self.resolution + x;
            let y = decode(self.height[cell]);
            let slope = (decode(self.height[cell + 1]) - decode(self.height[cell - 1])).abs()
                + (decode(self.height[(z + 1) * self.resolution + x])
                    - decode(self.height[(z - 1) * self.resolution + x]))
                .abs();
            if y <= 4.0 || slope > 0.6 {
                continue;
            }
            let world_x = (x as f32 / (self.resolution - 1) as f32 - 0.5) * self.size as f32;
            let world_z = (z as f32 / (self.resolution - 1) as f32 - 0.5) * self.size as f32;
            let yaw = entry.minimum_yaw
                + (entry.maximum_yaw - entry.minimum_yaw)
                    * (next(&mut self.random) as f32 / u32::MAX as f32);
            let radius = (entry.extent_x * entry.extent_x + entry.extent_z * entry.extent_z).sqrt();
            if self.placed.iter().any(|existing| {
                let other = &self.request.entries[existing.entry];
                let other_radius =
                    (other.extent_x * other.extent_x + other.extent_z * other.extent_z).sqrt();
                let spacing = if other.group == entry.group {
                    500.0_f32.min(self.size as f32 / 4.0)
                } else {
                    0.0
                };
                let dx = existing.x - world_x;
                let dz = existing.z - world_z;
                dx * dx + dz * dz < (radius + other_radius + spacing).powf(2.0)
            }) {
                continue;
            }
            return Some(Placement {
                entry: index,
                x: world_x,
                y,
                z: world_z,
                yaw,
            });
        }
        None
    }
}

fn next(value: &mut u32) -> u32 {
    *value ^= *value << 13;
    *value ^= *value >> 17;
    *value ^= *value << 5;
    *value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(group: usize, resolved: usize) -> Entry {
        Entry {
            group,
            resolved,
            land: true,
            minimum_size: 1000,
            extent_x: 20.0,
            extent_z: 20.0,
            priority: 1,
            minimum_yaw: -180.0,
            maximum_yaw: 180.0,
        }
    }
    fn request() -> Request {
        Request {
            enabled: true,
            seed: 0,
            target: 4,
            include_large: true,
            entries: vec![entry(0, 0), entry(1, 1)],
            authored: vec![0, 1],
            catalog: vec![],
            policies: vec![],
            stamps: crate::stamps::Request::default(),
        }
    }
    fn flat() -> Vec<i16> {
        vec![20000; 65 * 65]
    }

    #[test]
    fn placement_is_deterministic_and_has_an_independent_seed() {
        let request = request();
        let one = generate(&request, &flat(), 65, 4000, i32::MAX).unwrap();
        assert_eq!(one.len(), 4);
        assert_eq!(
            one,
            generate(&request, &flat(), 65, 4000, i32::MAX).unwrap()
        );
        assert_ne!(one, generate(&request, &flat(), 65, 4000, 12345).unwrap());
        let independent = Request {
            seed: 42,
            ..request
        };
        assert_eq!(
            generate(&independent, &flat(), 65, 4000, 1).unwrap(),
            generate(&independent, &flat(), 65, 4000, i32::MAX).unwrap()
        );
    }

    #[test]
    fn required_and_blocked_policies_preserve_baseline_precedence() {
        let mut request = request();
        request.enabled = false;
        request.entries[1].land = false;
        request.policies = vec![
            Policy {
                entry: 0,
                preference: 1,
                count: 1,
            },
            Policy {
                entry: 0,
                preference: 2,
                count: 1,
            },
            Policy {
                entry: 1,
                preference: 1,
                count: 1,
            },
        ];
        let placed = generate(&request, &flat(), 65, 4000, 12345).unwrap();
        assert_eq!(
            placed.iter().map(|p| p.entry).collect::<Vec<_>>(),
            vec![0, 1]
        );
        request.enabled = true;
        assert_eq!(
            generate(&request, &flat(), 65, 4000, 12345)
                .unwrap()
                .iter()
                .map(|p| p.entry)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn candidate_source_switches_before_size_and_large_filters() {
        let mut request = request();
        request.authored = vec![0];
        request.catalog = vec![1];
        request.entries[0].land = false;
        assert!(
            generate(&request, &flat(), 65, 4000, 12345)
                .unwrap()
                .iter()
                .all(|p| p.entry == 1)
        );
        request.entries[0].land = true;
        request.entries[0].minimum_size = 5000;
        assert!(
            generate(&request, &flat(), 65, 4000, 12345)
                .unwrap()
                .is_empty()
        );
        request.entries[0].minimum_size = 1000;
        request.entries[0].extent_x = 100.0;
        request.include_large = false;
        assert!(
            generate(&request, &flat(), 65, 4000, 12345)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn failed_terrain_candidates_are_exhausted_without_placing_underwater() {
        assert!(
            generate(&request(), &vec![0; 65 * 65], 65, 4000, 12345)
                .unwrap()
                .is_empty()
        );
        assert!(
            generate(&Request::disabled(), &flat(), 65, 4000, 12345)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn registry_and_policy_limits_reject_invalid_inputs() {
        let mut invalid = request();
        invalid.entries[0].resolved = 2;
        assert!(invalid.validate().is_err());
        invalid = request();
        invalid.entries[0].extent_x = f32::NAN;
        assert!(invalid.validate().is_err());
        invalid = request();
        invalid.entries[0].maximum_yaw = f32::INFINITY;
        assert!(invalid.validate().is_err());
        invalid = request();
        invalid.entries[0].resolved = 1;
        assert!(invalid.validate().is_err());
        invalid = request();
        invalid.target = 101;
        assert!(invalid.validate().is_err());
        invalid = request();
        invalid.policies = vec![
            Policy {
                entry: 0,
                preference: 1,
                count: 4096,
            },
            Policy {
                entry: 1,
                preference: 1,
                count: 1,
            },
        ];
        assert!(invalid.validate().is_err());
        let bytes: Vec<u8> = b"MAPM"
            .iter()
            .copied()
            .chain(
                [
                    2_u32,
                    0,
                    0,
                    0,
                    1,
                    0,
                    0,
                    0,
                    0,
                    u32::from_le_bytes(*b"MAPS"),
                    1,
                    0,
                    0,
                ]
                .into_iter()
                .flat_map(u32::to_le_bytes),
            )
            .collect();
        assert!(Request::read(&bytes).is_ok());
        assert!(Request::read(&bytes[..20]).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(Request::read(&trailing).is_err());
    }
}
