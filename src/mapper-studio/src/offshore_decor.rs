//! Inspected offshore root-decorator subset. Draws and water sampling are supplied
//! by the owning installation adapter; this is not complete site acceptance.
use crate::monument_attributes::Attributes;
use papyrus_terrain::{environment::Transform, offshore_search::Pose};

pub const VERSION: &str = "papyrus-offshore-decor-1";

enum Operation {
    Rotate { minimum: f32, maximum: f32 },
    Swim,
}
pub struct Program {
    rotation: [f32; 4],
    scale: [f32; 3],
    operations: Vec<Operation>,
}
impl Program {
    /// Capture validation does not establish gameplay-code compatibility. The
    /// caller must select this behavior only for an inspected implementation.
    pub fn new(attributes: &Attributes) -> Result<Self, String> {
        attributes.validate()?;
        if !attributes.issues.is_empty() {
            return Err("Offshore decorators have unresolved capture issues".into());
        }
        let root = attributes
            .environment
            .as_ref()
            .and_then(|e| e.root_transform.as_ref())
            .ok_or("Offshore root transform was not captured")?;
        Self::from_parts(root.rotation, root.scale, &attributes.root_decor)
    }
    pub fn from_parts(
        rotation: [f32; 4],
        scale: [f32; 3],
        records: &[crate::monument_attributes::RootDecor],
    ) -> Result<Self, String> {
        Transform {
            position: [0.; 3],
            rotation,
            scale,
        }
        .validate()?;
        if records.len() > 4096 {
            return Err("Too many offshore decorators".into());
        }
        let mut identities = std::collections::BTreeSet::new();
        let mut operations = Vec::with_capacity(records.len());
        for decor in records {
            if decor.object_id == 0
                || decor.object_id == i64::MIN
                || !identities.insert(decor.object_id)
                || decor
                    .values
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
            {
                return Err("Invalid offshore decorator".into());
            }
            // The inspected extension invokes root records regardless of their
            // behaviour enabled flag. The captured component order is retained.
            operations.push(match decor.kind.as_str() {
                "DecorRotate"
                    if decor.values.len() == 6
                        && [0, 2, 3, 5].into_iter().all(|i| decor.values[i] == 0.) =>
                {
                    Operation::Rotate {
                        minimum: decor.values[1],
                        maximum: decor.values[4],
                    }
                }
                "DecorSwim" if decor.values.is_empty() => Operation::Swim,
                _ => return Err("Unsupported offshore root decorator program".into()),
            });
        }
        Ok(Self {
            rotation,
            scale,
            operations,
        })
    }
    pub fn required_work(&self) -> usize {
        self.operations.len() + 2
    }
    /// Source-shaped program with caller-supplied Y draws in [0,1]. This does not
    /// reproduce the game's position-seeded RNG or claim its seed parity. A draw
    /// is requested even for a fixed Y range. X/Z draws cannot affect this subset.
    /// A game-RNG adapter must still consume its X draw before supplying Y and
    /// its Z draw afterwards; skipping fixed-axis draws would change that stream.
    /// Water callback represents GetWaterSurface(position, waves:false,
    /// volumes:false); it is not implicitly a constant ocean plane. Sampling work
    /// belongs to the caller in addition to this bounded operation allowance.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        position: [f32; 3],
        terrain_minimum_y: f32,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
        mut draw: impl FnMut([f32; 3], usize) -> Result<f32, String>,
        mut water_surface: impl FnMut([f32; 3]) -> Result<f32, String>,
    ) -> Result<Result<Pose, papyrus_terrain::environment::Verdict>, String> {
        use papyrus_terrain::environment::{Verdict, multiply};
        let mut pose = Transform {
            position,
            rotation: self.rotation,
            scale: self.scale,
        };
        pose.validate()?;
        if !terrain_minimum_y.is_finite()
            || terrain_minimum_y.abs() > 1_000_000.
            || !(1..=128_000_000).contains(&maximum_work)
        {
            return Err("Invalid offshore decorator context".into());
        }
        if cancelled() {
            return Ok(Err(Verdict::Cancelled));
        }
        if self.required_work() > maximum_work {
            return Ok(Err(Verdict::Budget));
        }
        for (index, operation) in self.operations.iter().enumerate() {
            if cancelled() {
                return Ok(Err(Verdict::Cancelled));
            }
            match *operation {
                Operation::Rotate { minimum, maximum } => {
                    let value = draw(pose.position, index)?;
                    if !value.is_finite() || !(0. ..=1.).contains(&value) {
                        return Err("Invalid offshore decorator random draw".into());
                    }
                    let angle = (minimum + (maximum - minimum) * value).to_radians() * 0.5;
                    let (s, c) = angle.sin_cos();
                    pose.rotation = multiply([0., s, 0., c], pose.rotation);
                }
                Operation::Swim => {
                    let height = water_surface(pose.position)?;
                    if !height.is_finite() || height.abs() > 1_000_000. {
                        return Err("Invalid offshore water surface".into());
                    }
                    pose.position[1] = if height <= terrain_minimum_y + 1. {
                        0.
                    } else {
                        height
                    };
                }
            }
        }
        if cancelled() {
            return Ok(Err(Verdict::Cancelled));
        }
        pose.validate()?;
        Ok(Ok(Pose {
            position: pose.position,
            rotation: pose.rotation,
            scale: pose.scale,
        }))
    }
}

#[cfg(test)]
mod tests;
