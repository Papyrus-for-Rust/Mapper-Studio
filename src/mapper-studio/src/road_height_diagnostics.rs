//! Bounded, opt-in observations of source road/rail height brush writes.
use papyrus_terrain::{path_terrain::Grid, road_terrain::HeightActivity};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

const MAX_EVENTS: usize = 65_536;
static SERIAL: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Trace {
    directory: PathBuf,
    cells: BTreeSet<usize>,
    before: Vec<(usize, i16)>,
    grid: [f32; 3],
    resolution: usize,
    input_sha256: String,
    events: Vec<HeightActivity>,
    dropped: usize,
}
impl Trace {
    pub(crate) fn begin(grid: Grid<'_>) -> Result<Option<Self>, String> {
        let Some(root) = std::env::var_os("MAPPER_ROAD_HEIGHT_TRACE") else {
            return Ok(None);
        };
        let text = std::env::var("MAPPER_ROAD_HEIGHT_TRACE_CELLS")
            .map_err(|_| "Height trace requires selected cells")?;
        let cells = parse_cells(&text, grid.height.len())?;
        let root = PathBuf::from(root);
        if root.as_os_str().is_empty() {
            return Err("Empty height trace directory".into());
        }
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let directory = root.join(format!(
            "trace-{}-{:04}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        for value in grid.height {
            hash.update(value.to_le_bytes());
        }
        Ok(Some(Self {
            directory,
            before: cells.iter().map(|&i| (i, grid.height[i])).collect(),
            cells,
            grid: [grid.size, grid.origin_y, grid.vertical_span],
            resolution: grid.resolution,
            input_sha256: format!("{:x}", hash.finalize()),
            events: Vec::new(),
            dropped: 0,
        }))
    }
    pub(crate) fn observe(&mut self, event: HeightActivity) {
        if self.cells.contains(&event.write.cell) {
            if self.events.len() < MAX_EVENTS {
                self.events.push(event);
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
        }
    }
    pub(crate) fn finish(self, height: &[i16]) -> Result<(), String> {
        if height.len() != self.resolution * self.resolution {
            return Err("Height trace output size differs".into());
        }
        let selected: Vec<_> = self
            .before
            .iter()
            .map(|&(cell, before)| json!({"cell":cell,"before":before,"after":height[cell]}))
            .collect();
        let document = json!({"format":"mapper-source-height-activity-1",
            "complete":self.dropped == 0,"dropped_events":self.dropped,
            "grid":{"size":self.grid[0],"origin_y":self.grid[1],"vertical_span":self.grid[2],"resolution":self.resolution},
            "input_height_sha256":self.input_sha256,"selected":selected,"events":self.events,
            "acceptance_certificate":false});
        let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
        std::fs::write(self.directory.join("activity.tmp"), bytes).map_err(|e| e.to_string())?;
        std::fs::rename(
            self.directory.join("activity.tmp"),
            self.directory.join("activity.json"),
        )
        .map_err(|e| e.to_string())
    }
}
fn parse_cells(text: &str, length: usize) -> Result<BTreeSet<usize>, String> {
    let mut result = BTreeSet::new();
    for (n, cell) in text.split(',').enumerate() {
        if n >= 64 {
            return Err("Height trace supports at most 64 cell requests".into());
        }
        let index = cell
            .trim()
            .parse::<usize>()
            .map_err(|_| "Invalid height trace cell")?;
        if index >= length {
            return Err("Height trace cell outside grid".into());
        }
        result.insert(index);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_cells_are_bounded_and_validated() {
        assert_eq!(
            parse_cells("2, 1,2", 3)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        for text in ["", "-1", "3", "1,no", &vec!["0"; 65].join(",")] {
            assert!(parse_cells(text, 3).is_err());
        }
    }
}
