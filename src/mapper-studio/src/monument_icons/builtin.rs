//! Authored monument artwork supplied for Mapper. Source SVGs embed these exact PNG bytes.
const ARTWORK: &[(&str, &str, &[u8])] = &[
    ("anvil-rock", "anvilrock", include_bytes!("../../assets/monument-icons/anvil-rock.png")),
    ("canyon", "canyon", include_bytes!("../../assets/monument-icons/canyon.png")),
    ("god-rock", "godrock", include_bytes!("../../assets/monument-icons/god-rock.png")),
    ("jungle-ruins", "jungleruins", include_bytes!("../../assets/monument-icons/jungle-ruins.png")),
    ("jungle-swamp", "jungleswamp", include_bytes!("../../assets/monument-icons/jungle-swamp.png")),
    ("lake", "lake", include_bytes!("../../assets/monument-icons/lake.png")),
    ("oasis", "oasis", include_bytes!("../../assets/monument-icons/oasis.png")),
    ("underground-cave", "undergroundcave", include_bytes!("../../assets/monument-icons/underground-cave.png")),
    ("wild-swamp", "wildswamp", include_bytes!("../../assets/monument-icons/wild-swamp.png")),
    (
        "apartments",
        "apartments",
        include_bytes!("../../assets/monument-icons/apartments.png"),
    ),
    (
        "dome",
        "dome",
        include_bytes!("../../assets/monument-icons/dome.png"),
    ),
    (
        "ferry-terminal",
        "ferryterminal",
        include_bytes!("../../assets/monument-icons/ferry-terminal.png"),
    ),
    (
        "junkyard",
        "junkyard",
        include_bytes!("../../assets/monument-icons/junkyard.png"),
    ),
    (
        "military-base",
        "militarybase",
        include_bytes!("../../assets/monument-icons/military-base.png"),
    ),
    (
        "radtown",
        "radtown",
        include_bytes!("../../assets/monument-icons/radtown.png"),
    ),
    (
        "supermarket",
        "supermarket",
        include_bytes!("../../assets/monument-icons/supermarket.png"),
    ),
    (
        "ice-lake",
        "icelake",
        include_bytes!("../../assets/monument-icons/ice-lake.png"),
    ),
    (
        "train-tunnel",
        "traintunnel",
        include_bytes!("../../assets/monument-icons/train-tunnel.png"),
    ),
    (
        "train-tunnel-rails",
        "traintunnelrails",
        include_bytes!("../../assets/monument-icons/train-tunnel-rails.png"),
    ),
    (
        "jungle-temple",
        "jungletemple",
        include_bytes!("../../assets/monument-icons/jungle-temple.png"),
    ),
    (
        "rock-excavator",
        "rockexcavator",
        include_bytes!("../../assets/monument-icons/rock-excavator.png"),
    ),
    (
        "command-bunker",
        "commandbunker",
        include_bytes!("../../assets/monument-icons/command-bunker.png"),
    ),
    (
        "abandoned-cabins",
        "abandonedcabins",
        include_bytes!("../../assets/monument-icons/abandoned-cabins.png"),
    ),
    (
        "airfield",
        "airfield",
        include_bytes!("../../assets/monument-icons/airfield.png"),
    ),
    (
        "arctic-research-base",
        "arcticresearchbase",
        include_bytes!("../../assets/monument-icons/arctic-research-base.png"),
    ),
    (
        "bandit-camp",
        "banditcamp",
        include_bytes!("../../assets/monument-icons/bandit-camp.png"),
    ),
    (
        "cargo",
        "cargo",
        include_bytes!("../../assets/monument-icons/cargo.png"),
    ),
    (
        "excavator",
        "excavator",
        include_bytes!("../../assets/monument-icons/excavator.png"),
    ),
    (
        "fishing-village",
        "fishingvillage",
        include_bytes!("../../assets/monument-icons/fishing-village.png"),
    ),
    (
        "floating-city",
        "floatingcity",
        include_bytes!("../../assets/monument-icons/floating-city.png"),
    ),
    (
        "gas-station",
        "gasstation",
        include_bytes!("../../assets/monument-icons/gas-station.png"),
    ),
    (
        "ghost-ship",
        "ghostship",
        include_bytes!("../../assets/monument-icons/ghost-ship.png"),
    ),
    (
        "harbor",
        "harbor",
        include_bytes!("../../assets/monument-icons/harbor.png"),
    ),
    (
        "hqm-quarry",
        "hqmquarry",
        include_bytes!("../../assets/monument-icons/hqm-quarry.png"),
    ),
    (
        "island",
        "island",
        include_bytes!("../../assets/monument-icons/island.png"),
    ),
    (
        "large-barn",
        "largebarn",
        include_bytes!("../../assets/monument-icons/large-barn.png"),
    ),
    (
        "large-fishing-village",
        "largefishingvillage",
        include_bytes!("../../assets/monument-icons/large-fishing-village.png"),
    ),
    (
        "large-oil-rig",
        "largeoilrig",
        include_bytes!("../../assets/monument-icons/large-oil-rig.png"),
    ),
    (
        "launch-site",
        "launchsite",
        include_bytes!("../../assets/monument-icons/launch-site.png"),
    ),
    (
        "lighthouse",
        "lighthouse",
        include_bytes!("../../assets/monument-icons/lighthouse.png"),
    ),
    (
        "military-tunnels",
        "militarytunnels",
        include_bytes!("../../assets/monument-icons/military-tunnels.png"),
    ),
    (
        "mining-outpost",
        "miningoutpost",
        include_bytes!("../../assets/monument-icons/mining-outpost.png"),
    ),
    (
        "missile-silo",
        "missilesilo",
        include_bytes!("../../assets/monument-icons/missile-silo.png"),
    ),
    (
        "nuclear-powerplant",
        "nuclearpowerplant",
        include_bytes!("../../assets/monument-icons/nuclear-powerplant.png"),
    ),
    (
        "outpost",
        "outpost",
        include_bytes!("../../assets/monument-icons/outpost.png"),
    ),
    (
        "ranch",
        "ranch",
        include_bytes!("../../assets/monument-icons/ranch.png"),
    ),
    (
        "satelite",
        "satelite",
        include_bytes!("../../assets/monument-icons/satelite.png"),
    ),
    (
        "sewer-branch",
        "sewerbranch",
        include_bytes!("../../assets/monument-icons/sewer-branch.png"),
    ),
    (
        "small-oil-rig",
        "smalloilrig",
        include_bytes!("../../assets/monument-icons/small-oil-rig.png"),
    ),
    (
        "stone-quarry",
        "stonequarry",
        include_bytes!("../../assets/monument-icons/stone-quarry.png"),
    ),
    (
        "sulfur-quarry",
        "sulfurquarry",
        include_bytes!("../../assets/monument-icons/sulfur-quarry.png"),
    ),
    (
        "trainyard",
        "trainyard",
        include_bytes!("../../assets/monument-icons/trainyard.png"),
    ),
    (
        "underwater-labs",
        "underwaterlabs",
        include_bytes!("../../assets/monument-icons/underwater-labs.png"),
    ),
    (
        "water-treatment",
        "watertreatment",
        include_bytes!("../../assets/monument-icons/water-treatment.png"),
    ),
    (
        "well",
        "well",
        include_bytes!("../../assets/monument-icons/well.png"),
    ),
    (
        "ziggaurat",
        "ziggaurat",
        include_bytes!("../../assets/monument-icons/ziggaurat.png"),
    ),
];
pub(super) fn key(name: &str) -> Option<&'static str> {
    let normalized: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let name = match normalized.as_str() {
        "powerplant" => "nuclearpowerplant",
        "satellitedish" | "satellite" => "satelite",
        "giantexcavatorpit" => "excavator",
        "watertreatmentplant" => "watertreatment",
        "oxumsgasstation" => "gasstation",
        "waterwell" => "well",
        "militarytunnel" => "militarytunnels",
        "jungleruin" => "jungleruins",
        "canyona" | "canyonb" | "canyonc" => "canyon",
        "lakea" | "lakeb" | "lakec" => "lake",
        "oasisa" | "oasisb" | "oasisc" => "oasis",
        "oilrig" => "smalloilrig",
        "underwaterlab" => "underwaterlabs",
        "cargoship" => "cargo",
        "jungleziggurat" | "ziggurat" => "ziggaurat",
        "compound" => "outpost",
        "bandittown" => "banditcamp",
        "thedome" => "dome",
        "abandonedsupermarket" => "supermarket",
        name if name.starts_with("abandonedmilitarybase")
            || name.starts_with("desertmilitarybase") =>
        {
            "militarybase"
        }
        name if name.starts_with("icelake") => "icelake",
        "apartment" => "apartments",
        "railtunnel" | "surfacetoundergroundrailconnector" => "traintunnelrails",
        name => name,
    };
    ARTWORK
        .iter()
        .find(|(_, normalized, _)| *normalized == name)
        .map(|(key, _, _)| *key)
}
pub(super) fn png(key: &str) -> Option<&'static [u8]> {
    ARTWORK
        .iter()
        .find(|(name, _, _)| *name == key)
        .map(|(_, _, bytes)| *bytes)
}
#[cfg(test)]
pub(super) fn all_keys() -> impl Iterator<Item = &'static str> {
    ARTWORK.iter().map(|(key, _, _)| *key)
}
