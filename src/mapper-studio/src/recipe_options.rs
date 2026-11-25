//! Portable controls which apply on top of installation defaults.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy)]
pub enum Landmark {
    Oasis,
    Lakes,
    Canyons,
}

impl Landmark {
    pub const ALL: [Self; 3] = [Self::Oasis, Self::Lakes, Self::Canyons];

    pub fn label(self) -> &'static str {
        match self {
            Self::Oasis => "Oasis",
            Self::Lakes => "Lakes",
            Self::Canyons => "Canyons",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Oasis => "Enable Oasis in Recipe → Landmarks to change this setting.",
            Self::Lakes => "Enable Lakes in Recipe → Landmarks to change this setting.",
            Self::Canyons => "Enable Canyons in Recipe → Landmarks to change this setting.",
        }
    }

    pub fn for_path(path: &str) -> Option<Self> {
        let mut folders = path.split('/');
        if folders.any(|folder| folder == "unique_environment") {
            return match folders.next() {
                Some("oasis") => Some(Self::Oasis),
                Some("lake") => Some(Self::Lakes),
                Some("canyon") => Some(Self::Canyons),
                _ => None,
            };
        }
        path.split('/')
            .any(|folder| folder == "canyon")
            .then_some(Self::Canyons)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oasis: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lakes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canyons: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underground_tunnels: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_car_wrecks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_road_powerlines: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_large_powerlines: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relax_monument_rules: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center_outpost: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo_path: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_on_roads: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jungle_beach_spawns: Option<bool>,
    pub island_intensity: Option<u8>,
    pub river_limit: Option<u8>,
    pub prefab_blocklist: Vec<String>,
    pub filename_template: String,
    pub filename_text: String,
    pub webhook_enabled: bool,
    pub webhook_url: String,
}

impl Options {
    #[doc(hidden)]
    pub fn early_placement_key(&self) -> (bool, bool, bool, bool, bool, Option<u8>, &[String]) {
        (
            self.oasis.unwrap_or(true),
            self.lakes.unwrap_or(true),
            self.canyons.unwrap_or(true),
            self.relax_monument_rules.unwrap_or_default(),
            self.center_outpost.unwrap_or_default(),
            self.island_intensity,
            &self.prefab_blocklist,
        )
    }
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn landmark(&mut self, family: Landmark) -> &mut Option<bool> {
        match family {
            Landmark::Oasis => &mut self.oasis,
            Landmark::Lakes => &mut self.lakes,
            Landmark::Canyons => &mut self.canyons,
        }
    }

    pub(crate) fn includes_underground_tunnels(&self) -> bool {
        self.underground_tunnels.unwrap_or(true)
    }

    pub fn add_prefab(&mut self, input: &str) -> Result<(), String> {
        let entry = input.trim().replace('\\', "/").to_ascii_lowercase();
        if !valid_prefab(&entry) || self.prefab_blocklist.len() == 256 {
            return Err(
                "Enter a prefab name or asset path. Up to 256 entries are supported.".into(),
            );
        }
        match self.prefab_blocklist.binary_search(&entry) {
            Ok(_) => {}
            Err(index) => self.prefab_blocklist.insert(index, entry),
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .island_intensity
            .is_some_and(|intensity| intensity > 10)
            || self.river_limit.is_some_and(|limit| limit > 64)
            || self.prefab_blocklist.len() > 256
            || !self
                .prefab_blocklist
                .iter()
                .all(|entry| valid_prefab(entry))
            || self
                .prefab_blocklist
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self.filename_template.len() > 160
            || self.filename_template.chars().any(char::is_control)
            || self.filename_text.len() > 120
            || self.filename_text.chars().any(char::is_control)
            || self.webhook_url.len() > 2048
            || (!self.webhook_url.is_empty() && !valid_webhook(&self.webhook_url))
        {
            return Err("Invalid generation options in Mapper Recipe".into());
        }
        Ok(())
    }

    pub fn landmark_blocker(&self, path: &str) -> Option<&'static str> {
        let family = Landmark::for_path(path)?;
        let enabled = match family {
            Landmark::Oasis => self.oasis,
            Landmark::Lakes => self.lakes,
            Landmark::Canyons => self.canyons,
        };
        (enabled == Some(false)).then_some(family.hint())
    }

    pub fn prefab_blocker(&self, path: &str) -> Option<&'static str> {
        self.landmark_blocker(path).or_else(|| {
            (!self.prefab_blocklist.is_empty()
                && self.prefab_blocklist.iter().any(|rule| {
                    let candidate = if rule.contains('/') {
                        path
                    } else {
                        path.rsplit('/').next().unwrap_or(path)
                    };
                    candidate
                        .strip_suffix(".prefab")
                        .unwrap_or(candidate)
                        .eq_ignore_ascii_case(rule.strip_suffix(".prefab").unwrap_or(rule))
                }))
            .then_some("Remove this prefab from the recipe blocklist to change this setting.")
        })
    }

    pub fn export_blocked(&self, path: &str) -> bool {
        if self.prefab_blocker(path).is_some() {
            return true;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        self.block_car_wrecks.unwrap_or_default()
            && (path.contains("/v3_road_vehicles")
                || name.contains("carwreck")
                || name.contains("car_wreck")
                || name.starts_with("sedan_a_wreck"))
    }

    pub(crate) fn road_object_blocked(&self, path: &str, roadside: bool) -> bool {
        self.export_blocked(path)
            || (roadside
                && self.remove_road_powerlines.unwrap_or_default()
                && (path.contains("/powerline-")
                    || path.contains("/power substations/")
                    || path
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| name.starts_with("powerline_pole"))))
    }

    pub(crate) fn prefers_center(&self, path: Option<&str>) -> bool {
        self.center_outpost.unwrap_or_default()
            && path.is_some_and(|path| {
                path.contains("/monument/")
                    && path
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| matches!(name, "compound.prefab" | "outpost.prefab"))
            })
    }

    pub fn filename(
        &self,
        size: u32,
        seed: u32,
        stages: &[crate::StageSeed; 6],
        label: &str,
    ) -> String {
        use sha2::{Digest, Sha256};
        let name = if self.filename_template.trim().is_empty() {
            label.to_owned()
        } else {
            let mut hash = Sha256::new();
            hash.update(size.to_le_bytes());
            hash.update(seed.to_le_bytes());
            for stage in stages {
                hash.update(stage.value.to_le_bytes());
            }
            let id = format!("{:x}", hash.finalize());
            self.filename_template
                .replace("{Type}", "Procedural")
                .replace("{Size}", &size.to_string())
                .replace("{Seed}", &seed.to_string())
                .replace("{Id}", &id[..24])
                .replace("{ShortId}", &id[..10])
                .replace("{Date}", &utc_date())
                .replace("{_}", "_")
                .replace("{Custom}", &self.filename_text)
        };
        let safe: String = name
            .trim()
            .trim_end_matches('.')
            .chars()
            .take(150)
            .map(|c| {
                if c.is_control() || "<>:\"/\\|?*".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        if safe.is_empty() {
            format!("map-{size}-{seed}")
        } else {
            safe
        }
    }
}

pub(crate) fn valid_webhook(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
        && !authority.contains('@')
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\\' | '"' | '#'))
}

fn utc_date() -> String {
    // Convert whole days since the UNIX epoch to a Gregorian UTC date.
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / 86400)
        + 719468;
    let era = days / 146097;
    let cycle = days % 146097;
    let year = (cycle - cycle / 1460 + cycle / 36524 - cycle / 146096) / 365;
    let ordinal = cycle - (365 * year + year / 4 - year / 100);
    let month = (5 * ordinal + 2) / 153;
    let day = ordinal - (153 * month + 2) / 5 + 1;
    let month = (month + 2) % 12 + 1;
    let year = year + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn valid_prefab(entry: &str) -> bool {
    !entry.is_empty()
        && entry.len() <= 300
        && entry
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
        && entry
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/_.- ".contains(&c))
}
