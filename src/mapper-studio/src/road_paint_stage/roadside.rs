//! The private road-terrain revision available to roadside prefab placement.
use papyrus_terrain::{native_layers::View, road_paint::Road};

pub struct RoadsideInput<'a, 'path> {
    pub layers: View<'a>,
    /// Smoothed road controls after the complete road/retained-rail height pass.
    pub roads: &'a [Road<'path>],
}

#[cfg(test)]
mod tests;

/// Only channels changed by the roadside transaction need owned replacements.
#[derive(Default)]
pub struct RoadsideChanges {
    pub height: Option<Vec<i16>>,
    pub topology: Option<Vec<i32>>,
    pub splat: Option<Vec<u8>>,
}
impl RoadsideChanges {
    pub(super) fn validate(
        &self,
        view: View<'_>,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if self
            .height
            .as_ref()
            .is_some_and(|v| v.len() != view.height.len())
            || self
                .topology
                .as_ref()
                .is_some_and(|v| v.len() != view.topology.len())
            || self
                .splat
                .as_ref()
                .is_some_and(|v| v.len() != view.splat.len())
        {
            return Err("Roadside terrain revision has incompatible dimensions".into());
        }
        let work =
            self.height.as_ref().map_or(0, Vec::len) + self.splat.as_ref().map_or(0, Vec::len);
        if work > maximum_work {
            return Err("Roadside terrain validation exceeded its work allowance".into());
        }
        if let Some(height) = &self.height {
            for (index, &sample) in height.iter().enumerate() {
                if index % 4096 == 0 && cancelled() {
                    return Ok(false);
                }
                if !(0..=32766).contains(&sample) {
                    return Err("Roadside terrain revision has invalid height samples".into());
                }
            }
        }
        // Serialized splat channels are independent bytes. Source river,
        // absolute-road and material-set operations can produce non-255 sums,
        // including an empty pixel. Keep their exact values like read_raw does.
        Ok(!cancelled())
    }
}
