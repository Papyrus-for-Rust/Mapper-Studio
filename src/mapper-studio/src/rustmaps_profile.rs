//! Converts RustMaps custom generator exports into a local Mapper recipe.
use crate::{Presence, Project, native_profile, prefab_assets::Catalog};
use serde_json::Value;
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

const MAX_BYTES: u64 = 1024 * 1024;

pub struct Import {
    pub recipe: Project,
    pub skipped: Vec<String>,
}

pub fn load(path: &Path, base: &Project, catalog: Option<&Catalog>) -> Result<Import, String> {
    load_with_bindings(path, base, catalog, None, None)
}

pub fn load_with_bindings(
    path: &Path,
    base: &Project,
    catalog: Option<&Catalog>,
    rails: Option<crate::rail_generation::Recipe>,
    rivers: Option<crate::river_generation::Recipe>,
) -> Result<Import, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("RustMaps profile exceeds 1 MiB".into());
    }
    let config = serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
        .map_err(|error| format!("Invalid RustMaps JSON. {error}"))?;
    convert(&config, base, catalog, rails, rivers)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Default,
    Include,
    Exclude,
}

fn choice(value: &Value, label: &str, skipped: &mut Vec<String>) -> Choice {
    match value.as_str() {
        Some("TryGenerate") => Choice::Include,
        Some("DoNotGenerate") => Choice::Exclude,
        Some("NoPreference") => Choice::Default,
        _ if value.is_null() => Choice::Default,
        _ => {
            skipped.push(format!("{label} has an unrecognized selection"));
            Choice::Default
        }
    }
}

fn enabled(value: &Value) -> bool {
    value.as_bool() == Some(true)
}

fn weights<const N: usize>(config: &Value, fields: [&str; N]) -> Option<[u32; N]> {
    let mut values = [0.; N];
    for (value, field) in values.iter_mut().zip(fields) {
        *value = config[field]
            .as_f64()
            .filter(|number| (0. ..=1.).contains(number))?;
    }
    let total: f64 = values.iter().sum();
    if (total - 1.).abs() > 0.02 {
        return None;
    }
    let mut result = values.map(|value| (value * 10_000. / total).round() as u32);
    let largest = (0..N).max_by_key(|&index| result[index])?;
    result[largest] = 10_000
        - result
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != largest)
            .map(|(_, value)| *value)
            .sum::<u32>();
    Some(result)
}

fn angle(value: &Value, fallback: u32, label: &str, skipped: &mut Vec<String>) -> Option<u32> {
    if value.is_null() || value.as_str() == Some("Default") {
        return None;
    }
    let degrees = value.as_u64().or_else(|| value.as_str()?.parse().ok());
    if let Some(degrees) = degrees.filter(|&number| number <= 360) {
        Some(degrees as u32 * 1000)
    } else {
        skipped.push(format!("{label} rotation"));
        Some(fallback)
    }
}

fn terrain(config: &Value, recipe: &mut Project, skipped: &mut Vec<String>) {
    let tiers = &config["tierConfig"];
    let biomes = &config["biomeConfig"];
    let profile = recipe
        .installation
        .as_ref()
        .and_then(|source| native_profile::for_source(source, recipe.native_profile.as_ref()).ok());
    let tier_angle = profile
        .as_ref()
        .map_or(0, |p| p.settings.loot_angle_millidegrees);
    let biome_angle = profile
        .as_ref()
        .map_or(0, |p| p.settings.biome_angle_millidegrees);
    let tier_rotation = angle(&config["lootAxisAngle"], tier_angle, "Tier", skipped);
    let biome_rotation = angle(&config["biomeAxisAngle"], biome_angle, "Biome", skipped);
    if enabled(&tiers["enabled"]) || tier_rotation.is_some() {
        let mix = if enabled(&tiers["enabled"]) {
            weights(
                tiers,
                ["tier0Percentage", "tier1Percentage", "tier2Percentage"],
            )
        } else {
            profile.as_ref().map(|p| p.settings.terrain_weights)
        };
        if let Some(tier_weights) = mix {
            recipe.terrain_overrides = Some(native_profile::TerrainOverrides {
                tier_angle: tier_rotation.unwrap_or(tier_angle),
                tier_weights,
            });
        } else {
            skipped.push(
                "Tier percentages or rotation require valid percentages or source defaults".into(),
            );
        }
    } else if !tiers.is_null() {
        recipe.terrain_overrides = None;
    }
    if enabled(&biomes["enabled"]) || biome_rotation.is_some() {
        let mix = if enabled(&biomes["enabled"]) {
            weights(
                biomes,
                [
                    "aridPercentage",
                    "temperatePercentage",
                    "tundraPercentage",
                    "arcticPercentage",
                ],
            )
            .zip(
                biomes["junglePercentage"]
                    .as_f64()
                    .filter(|number| (0. ..=1.).contains(number))
                    .map(|number| (number * 10_000.).round() as u32)
                    .or_else(|| {
                        biomes["junglePercentage"]
                            .is_null()
                            .then(|| profile.as_ref().map(|p| p.settings.jungle_weight))
                            .flatten()
                    }),
            )
        } else {
            profile
                .as_ref()
                .map(|p| (p.settings.biome_weights, p.settings.jungle_weight))
        };
        if let Some((weights, jungle_weight)) = mix {
            recipe.biome_overrides = Some(native_profile::BiomeOverrides {
                angle: biome_rotation.unwrap_or(biome_angle),
                weights,
                jungle_weight,
            });
        } else {
            skipped.push(
                "Biome percentages or rotation require valid percentages or source defaults".into(),
            );
        }
    } else if !biomes.is_null() {
        recipe.biome_overrides = None;
    }
    let islands = &config["islandConfig"];
    if !islands.is_null() {
        if !enabled(&islands["enabled"]) {
            recipe.options.island_intensity = None;
        } else if let Some(value) = islands["intensity"].as_u64().filter(|&value| value <= 10) {
            recipe.options.island_intensity = Some(value as u8);
        } else {
            skipped.push("Island intensity must be between 0 and 10".into());
        }
    }
    for (key, label) in [
        ("flattenShoreAndBay", "Flatten shore and bay"),
        ("landSize", "Land size"),
        ("mountainConfig", "Mountain configuration"),
        ("islandCount", "Island count"),
        ("era", "Terrain era"),
        ("mountains", "Mountain scale"),
        ("hills", "Hill scale"),
        ("cliffs", "Cliff density"),
        ("rocks", "Rock density"),
    ] {
        let value = &config[key];
        if active(value) {
            skipped.push(label.into());
        }
    }
    if active(&biomes["matchBiomePercentages"]) {
        skipped.push("Exact biome coverage matching".into());
    }
}

fn active(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => {
            !fields.is_empty() && fields.get("enabled").and_then(Value::as_bool) != Some(false)
        }
        Value::String(text) => !matches!(text.as_str(), "" | "Default" | "Full" | "NoPreference"),
        _ => true,
    }
}

fn normalized(name: &str) -> String {
    name.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect()
}

fn monument_path(name: &str, recipe: &Project, catalog: Option<&Catalog>) -> Option<String> {
    let key = normalized(name);
    let aliases: &[&str] = match key.as_str() {
        "outpost" => &["compound"],
        "bandittown" => &["banditcamp"],
        "gasstation" => &["gasstation1", "oxumsgasstation"],
        "warehouse" => &["miningoutpost", "warehouse1"],
        "supermarket" => &["supermarket1", "abandonedsupermarket"],
        "militarytunnels" => &["militarytunnel1"],
        "watertreatment" => &["watertreatmentplant1"],
        "spheretank" => &["thedome"],
        "largeoilrig" => &["oilrigai2", "oilriglarge"],
        "smalloilrig" => &["oilrigai", "oilrigsmall"],
        _ => &[],
    };
    let matches = |name: &str| {
        let candidate = normalized(name);
        candidate == key
            || aliases.contains(&candidate.as_str())
            || candidate.strip_suffix('1') == Some(&key)
    };
    let mut paths = recipe
        .targets
        .iter()
        .filter_map(|target| {
            let path = target.prefab_path.as_deref()?;
            matches(&target.name).then_some(path)
        })
        .chain(
            catalog
                .into_iter()
                .flat_map(|catalog| catalog.entries.iter())
                .filter_map(|(path, asset)| {
                    let stem = path
                        .rsplit('/')
                        .next()
                        .unwrap_or(path)
                        .trim_end_matches(".prefab");
                    (matches(stem) || asset.display_name.as_deref().is_some_and(matches))
                        .then_some(path.as_str())
                }),
        )
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    if paths.len() == 1 {
        return Some(paths[0].to_owned());
    }
    None
}

fn monuments(
    config: &Value,
    recipe: &mut Project,
    catalog: Option<&Catalog>,
    skipped: &mut Vec<String>,
) {
    for group in [
        "oilRigConfigurations",
        "safezones",
        "largeMonuments",
        "smallMonuments",
        "harbors",
        "waterWells",
        "caves",
        "mountains",
        "quarries",
        "icebergs",
        "iceLakes",
        "ruins",
    ] {
        for monument in config[group].as_array().into_iter().flatten() {
            let name = monument["type"].as_str().unwrap_or("Unnamed monument");
            let blocked = enabled(&monument["blocked"]);
            if blocked || enabled(&monument["desired"]) {
                if let Some(path) = monument_path(name, recipe, catalog) {
                    if blocked {
                        if recipe.options.add_prefab(&path).is_err() {
                            skipped.push(format!("{name} block"));
                        }
                    } else {
                        let target = recipe
                            .targets
                            .iter_mut()
                            .find(|target| target.prefab_path.as_deref() == Some(&path));
                        if let Some(target) = target {
                            target.presence = Presence::Required;
                        } else if recipe.add_target(name.to_owned(), path).is_err() {
                            skipped.push(format!("{name} preference"));
                        }
                    }
                } else {
                    skipped.push(format!(
                        "{name} preference needs a matching installation prefab"
                    ));
                }
            }
            if enabled(&monument["customPrefab"]["enabled"]) {
                skipped.push(format!("{name} overlay needs its original local map"));
            }
            if [
                "distanceRules",
                "nearbyPrefabRules",
                "flattenTerrain",
                "amount",
                "spacing",
                "biomePreference",
                "position",
            ]
            .into_iter()
            .any(|key| active(&monument[key]))
                || ["biomePreferences", "tierPreferences"]
                    .into_iter()
                    .any(|key| {
                        monument[key]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|rule| active(&rule["selection"]))
                    })
                || monument["placements"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|rule| rule["mode"].as_str().is_some_and(|mode| mode != "Anywhere"))
            {
                skipped.push(format!("{name} placement constraints"));
            }
        }
    }
}

fn convert(
    config: &Value,
    base: &Project,
    catalog: Option<&Catalog>,
    rails: Option<crate::rail_generation::Recipe>,
    rivers: Option<crate::river_generation::Recipe>,
) -> Result<Import, String> {
    let settings = config
        .as_object()
        .ok_or("Expected a RustMaps custom generator profile")?;
    if config["version"].as_u64() != Some(5)
        || ![
            "terrainConfiguration",
            "generateRingRoad",
            "riverCount",
            "largeMonuments",
            "blockedPrefabs",
        ]
        .iter()
        .any(|key| settings.contains_key(*key))
    {
        return Err("Expected a RustMaps custom generator profile (version 5)".into());
    }
    let mut recipe = base.clone();
    let mut skipped = Vec::new();
    let options = &mut recipe.options;
    for (field, target, invert) in [
        (
            "removeSmallPowerLines",
            &mut options.remove_road_powerlines,
            false,
        ),
        (
            "removeLargePowerLines",
            &mut options.remove_large_powerlines,
            false,
        ),
        ("removeCarWrecks", &mut options.block_car_wrecks, false),
        ("allowBuildingOnRoads", &mut options.build_on_roads, false),
        ("modifyTiers", &mut options.relax_monument_rules, false),
        (
            "trySpawningOutpostInCenter",
            &mut options.center_outpost,
            false,
        ),
        (
            "removeUndergroundTunnels",
            &mut options.underground_tunnels,
            true,
        ),
        ("embedCargoShipPath", &mut options.cargo_path, false),
    ] {
        if let Some(value) = config[field].as_bool() {
            *target = Some(value ^ invert);
        }
    }
    if let Some(biomes) = config["playerSpawnBeachBiomes"].as_array() {
        options.jungle_beach_spawns = Some(biomes.iter().any(|value| value.as_str() == Some("Jungle")));
        if biomes.iter().any(|value| value.as_str() != Some("Jungle")) {
            skipped.push("Custom beach spawn biome list".into());
        }
    } else if !config["playerSpawnBeachBiomes"].is_null() {
        skipped.push("Custom beach spawn biome list".into());
    }
    for (field, target, label) in [
        ("oasesConfiguration", &mut options.oasis, "Oases"),
        ("lakesConfiguration", &mut options.lakes, "Lakes"),
        ("canyonsConfiguration", &mut options.canyons, "Canyons"),
    ] {
        let family = &config[field];
        let mode = choice(&family["generate"], label, &mut skipped);
        if enabled(&family["blocked"]) || mode == Choice::Exclude {
            *target = Some(false);
        } else if mode == Choice::Include {
            *target = Some(true);
            skipped.push(format!("{label} placement counts"));
        } else if family["generate"].as_str() == Some("NoPreference") {
            *target = None;
        }
    }
    if let Some(items) = config["blockedPrefabs"].as_array() {
        options.prefab_blocklist.clear();
        for item in items {
            if item["blocked"].as_bool() == Some(false) {
                continue;
            }
            if item
                .as_str()
                .or_else(|| item["prefab"].as_str())
                .or_else(|| item["name"].as_str())
                .is_none_or(|rule| options.add_prefab(rule).is_err())
            {
                skipped.push("An unsupported prefab blocklist entry".into());
            }
        }
    }
    if let Some(parts) = config["fileName"].as_object() {
        let ordered = parts
            .iter()
            .map(|(index, part)| Some((index.parse::<u32>().ok()?, part.as_str()?)))
            .collect::<Option<BTreeMap<_, _>>>();
        if let Some(template) = ordered
            .filter(|parts| parts.len() == config["fileName"].as_object().unwrap().len())
            .map(|parts| parts.values().copied().collect::<String>())
            .filter(|template| template.len() <= 160 && !template.chars().any(char::is_control))
        {
            options.filename_template = template;
        } else {
            skipped.push("Map filename template".into());
        }
    }
    if enabled(&config["webhook"]["enabled"]) {
        skipped.push("Webhook endpoint requires local setup".into());
    }
    if enabled(&config["removeRivers"]) {
        recipe.rivers = None;
        options.river_limit = None;
    } else if let Some(count) = config["riverCount"].as_u64().filter(|&count| count <= 64) {
        options.river_limit = Some(count as u8);
        if recipe.rivers.is_none() {
            recipe.rivers = rivers;
        }
        if recipe.rivers.is_none() {
            skipped.push("Enable Water & rivers to use the imported river limit".into());
        }
    } else if !config["riverCount"].is_null() {
        skipped.push("River count must be between 0 and 64".into());
    }

    let ring = choice(&config["generateRingRoad"], "Ring road", &mut skipped);
    if ring != Choice::Default {
        let applied = recipe.roads.as_mut().is_some_and(|roads| {
            let mut next = roads.clone();
            let ring = ring == Choice::Include;
            next.main_ring = Some(ring);
            next.ring_costmap = if ring && next.ring_search.is_some() {
                next.access_costmap.clone()
            } else {
                None
            };
            if next.validate().is_err() {
                return false;
            }
            *roads = next;
            true
        });
        if !applied {
            skipped.push("Ring road requires compatible roads in the base recipe".into());
        }
    }
    match choice(
        &config["generateAboveGroundTrainTracks"],
        "Surface rails",
        &mut skipped,
    ) {
        Choice::Include if recipe.rails.is_none() => {
            recipe.rails = rails;
            if recipe.rails.is_none() {
                skipped.push("Enable Rails in the recipe to use surface tracks".into());
            }
        }
        Choice::Exclude => {
            if recipe
                .path_edits
                .iter()
                .any(|edit| edit.route.kind == crate::path_edit::Kind::Rail)
            {
                skipped.push("Surface rails kept because the recipe contains rail edits".into());
            } else {
                recipe.rails = None;
            }
        }
        _ => {}
    }
    for (key, label) in [
        ("riverWidth", "River width"),
        ("riverMaxLength", "River length"),
        ("heightmapResolution", "Heightmap resolution"),
        ("splatmapResolution", "Splatmap resolution"),
    ] {
        if active(&config[key]) {
            skipped.push(label.into());
        }
    }
    let labs = &config["underwaterLabsConfiguration"];
    if enabled(&labs["blocked"])
        || choice(&labs["generate"], "Underwater labs", &mut skipped) != Choice::Default
        || active(&labs["distanceRules"])
    {
        skipped.push("Underwater lab generation preferences".into());
    }
    terrain(&config["terrainConfiguration"], &mut recipe, &mut skipped);
    monuments(config, &mut recipe, catalog, &mut skipped);
    recipe.validate()?;
    skipped.sort_unstable();
    skipped.dedup();
    Ok(Import { recipe, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn imports_settings_and_reports_unsupported_rules_without_activating_a_webhook() {
        let base = Project::default();
        let config = json!({
            "version": 5,
            "riverCount": 8,
            "removeUndergroundTunnels": true,
            "terrainConfiguration": {
                "biomeConfig": {
                    "enabled": true, "aridPercentage": 0.38, "temperatePercentage": 0.2,
                    "tundraPercentage": 0.2, "arcticPercentage": 0.22, "junglePercentage": 0.45
                },
                "flattenShoreAndBay": true
            },
            "fileName": {"10": "{ShortId}", "2": "{Size}", "0": "{Type}", "3": "{_}"},
            "blockedPrefabs": ["Some.prefab"],
            "lakesConfiguration": {"generate": "DoNotGenerate"},
            "webhook": {"enabled": true, "url": "https://example.test/webhook"}
        });
        let Import { recipe, skipped } = convert(&config, &base, None, None, None).unwrap();
        assert_eq!(recipe.master_seed, base.master_seed);
        assert_eq!(recipe.options.river_limit, Some(8));
        assert_eq!(recipe.options.filename_template, "{Type}{Size}{_}{ShortId}");
        assert_eq!(recipe.options.prefab_blocklist, ["some.prefab"]);
        assert_eq!(recipe.options.underground_tunnels, Some(false));
        assert_eq!(recipe.options.lakes, Some(false));
        assert!(!recipe.options.webhook_enabled);
        assert!(recipe.options.webhook_url.is_empty());
        assert_eq!(
            recipe.biome_overrides.unwrap().weights,
            [3800, 2000, 2000, 2200]
        );
        assert_eq!(recipe.biome_overrides.unwrap().jungle_weight, 4500);
        assert!(skipped.iter().any(|note| note == "Flatten shore and bay"));
        assert!(
            skipped
                .iter()
                .any(|note| note == "Webhook endpoint requires local setup")
        );
        assert!(
            convert(
                &json!({"version":5,"unrelated":true}),
                &base,
                None,
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn monument_flags_bind_to_the_recipe_prefab_identity() {
        let mut base = Project::default();
        base.add_target(
            "compound".into(),
            "assets/bundled/prefabs/autospawn/monument/medium/compound.prefab".into(),
        )
        .unwrap();
        base.targets.last_mut().unwrap().presence = Presence::Allowed;
        let config = json!({
            "version": 5, "riverCount": null,
            "safezones": [{"type":"Outpost","desired":true,"customPrefab":{"enabled":true,"id":"remote"}}],
            "largeMonuments": [{"type":"Launch Site","blocked":true}]
        });
        let import = convert(&config, &base, None, None, None).unwrap();
        assert_eq!(
            import.recipe.targets.last().unwrap().presence,
            Presence::Required
        );
        assert_eq!(
            import.recipe.monument_presence(&import.recipe.targets[0]),
            Presence::Excluded
        );
        assert!(import.recipe.monument_addons.is_empty());
        assert_eq!(
            import.skipped,
            ["Outpost overlay needs its original local map"]
        );
    }
}
