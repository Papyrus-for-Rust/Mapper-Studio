//! Isolated, cancellable execution of embedded metadata parsers. Never project scripts.
use std::{
    ffi::{OsStr, OsString},
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Copy)]
pub(crate) enum Script {
    Palette,
    PaperMapGrid,
    Catalog,
    RestrictionCatalog,
    Textures,
    Monuments,
    MonumentStages,
    OffshoreStages,
    RoadsideStages,
    DecorModels,
    Infrastructure,
    RoadConnectors,
    RoadLayout,
    RoadMeshes,
    Rails,
    RailMeshes,
    Decoration,
    UniformCliffs,
    JungleVines,
    CoastalCliffs,
    WaterTreatment,
    Powerlines,
    SceneWires,
    MonumentTags,
    Spawn,
    RoadObjects,
    RiverObjects,
    RiverMeshes,
    PrefabIdentities,
    Dungeons,
    Tunnels,
}
impl Script {
    fn name(self) -> &'static str {
        match self {
            Self::Palette => "extract_terrain_palette.py",
            Self::PaperMapGrid => "extract_paper_map_grid.py",
            Self::Catalog => "extract_verified_prefabs.py",
            Self::RestrictionCatalog => "prepare_restriction_catalog.py",
            Self::Textures => "prepare_prefab_textures.py",
            Self::Monuments => "prepare_monument_metadata.py",
            Self::MonumentStages => "extract_monument_stages.py",
            Self::OffshoreStages => "extract_offshore_stages.py",
            Self::RoadsideStages => "extract_roadside_stage.py",
            Self::DecorModels => "extract_decor_models.py",
            Self::Infrastructure => "extract_infrastructure.py",
            Self::RoadConnectors => "prepare_road_connectors.py",
            Self::RoadLayout => "extract_road_layout_assets.py",
            Self::RoadMeshes => "extract_road_meshes.py",
            Self::Rails => "extract_rail_source.py",
            Self::RailMeshes => "extract_rail_meshes.py",
            Self::Decoration => "prepare_decoration_metadata.py",
            Self::UniformCliffs => "extract_uniform_cliffs.py",
            Self::JungleVines => "extract_jungle_vines.py",
            Self::CoastalCliffs => "extract_coastal_cliffs.py",
            Self::WaterTreatment => "extract_water_treatment_source.py",
            Self::Powerlines => "extract_powerline_source.py",
            Self::SceneWires => "extract_scene_wires.py",
            Self::MonumentTags => "extract_monument_tags.py",
            Self::Spawn => "prepare_spawn_metadata.py",
            Self::RoadObjects => "extract_road_objects.py",
            Self::RiverObjects => "extract_river_objects.py",
            Self::RiverMeshes => "extract_river_meshes.py",
            Self::PrefabIdentities => "extract_prefab_identities.py",
            Self::Dungeons => "extract_dungeon_source.py",
            Self::Tunnels => "prepare_tunnel_grid.py",
        }
    }
}
macro_rules! parser {
    ($file:literal) => {
        (
            $file,
            include_str!(concat!("../../../tools/Mapper.Diagnostics/", $file)),
        )
    };
}
const SCRIPTS: &[(&str, &str)] = &[
    parser!("mapper_progress.py"),
    parser!("extract_terrain_palette.py"),
    parser!("extract_paper_map_grid.py"),
    parser!("terrain_palette.py"),
    parser!("extract_verified_prefabs.py"),
    parser!("prepare_restriction_catalog.py"),
    parser!("extract_prefab_modifiers.py"),
    parser!("prefab_geometry.py"),
    parser!("no_build_geometry.py"),
    parser!("resolve_prefab_textures.py"),
    parser!("prepare_prefab_textures.py"),
    parser!("texture_payload_cache.py"),
    parser!("lazy_unity_bundle.py"),
    // Byte-equivalent transport acceleration leaves logical cache producers intact.
    parser!("metadata_object_reader.py"),
    parser!("prepare_road_connectors.py"),
    parser!("mapper_paths.py"),
    parser!("extract_monument_source.py"),
    parser!("prepare_monument_metadata.py"),
    parser!("extract_monument_stages.py"),
    parser!("extract_dungeon_source.py"),
    parser!("dungeon_prefabs.py"),
    parser!("inspect_tunnel_grid.py"),
    parser!("prepare_tunnel_grid.py"),
    parser!("metadata_local_transform.py"),
    parser!("metadata_transforms.py"),
    parser!("tunnel_prefabs.py"),
    parser!("tunnel_identities.py"),
    parser!("monument_stages.py"),
    parser!("extract_offshore_stages.py"),
    parser!("offshore_prefabs.py"),
    parser!("offshore_stages.py"),
    parser!("extract_roadside_stage.py"),
    parser!("roadside_stages.py"),
    parser!("extract_roadside_connectors.py"),
    parser!("model_preview_geometry.py"),
    parser!("monument_placement_attributes.py"),
    parser!("gameplay_sources.py"),
    parser!("extract_decor_models.py"),
    parser!("decor_collider_meshes.py"),
    parser!("static_preview_models.py"),
    parser!("extract_infrastructure.py"),
    parser!("extract_road_layout_assets.py"),
    parser!("road_layout_assets.py"),
    parser!("road_stages.py"),
    parser!("prefab_paths.py"),
    parser!("extract_road_meshes.py"),
    parser!("mesh_source_nodes.py"),
    parser!("extract_rail_source.py"),
    parser!("extract_rail_meshes.py"),
    parser!("rail_mesh_buffers.py"),
    parser!("road_mesh_stages.py"),
    parser!("extract_decor_source.py"),
    parser!("prepare_decoration_metadata.py"),
    parser!("extract_uniform_cliffs.py"),
    parser!("extract_jungle_vines.py"),
    parser!("extract_coastal_cliffs.py"),
    parser!("extract_water_treatment_source.py"),
    parser!("extract_powerline_source.py"),
    parser!("extract_scene_wires.py"),
    parser!("inspect_prefab_runtime.py"),
    parser!("extract_monument_tags.py"),
    parser!("monument_tag_geometry.py"),
    parser!("terrain_hole_shapes.py"),
    parser!("extract_spawn_source.py"),
    parser!("prepare_spawn_metadata.py"),
    parser!("extract_road_objects.py"),
    parser!("extract_river_objects.py"),
    parser!("road_object_stages.py"),
    parser!("extract_river_meshes.py"),
    parser!("river_mesh_stages.py"),
    parser!("extract_prefab_identities.py"),
];
pub(crate) const MONUMENT_PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/monuments/client-0f703d43.json"),
    include_str!("../../../compatibility/monuments/client-8dd4e235.json"),
    include_str!("../../../compatibility/monuments/client-58f3263f.json"),
];
pub(crate) fn uniform_cliff_extractor_sha256() -> String {
    cliff_extractor_sha256(None)
}
pub(crate) fn vine_extractor_sha256() -> String {
    cliff_extractor_sha256(Some("extract_jungle_vines.py"))
}
pub(crate) fn water_treatment_extractor_sha256() -> String {
    placement_extractor_sha256(std::iter::empty())
}
pub(crate) fn powerline_extractor_sha256() -> String {
    utility_extractor_sha256(std::iter::empty())
}
pub(crate) fn scene_wire_extractor_sha256() -> String {
    utility_extractor_sha256(["extract_scene_wires.py", "metadata_transforms.py"].into_iter())
}
// The reviewed texture-driver revisions leave utility records unchanged.
// Match only the exact compatible producer digests for each source layout.
pub(crate) fn powerline_extractor_matches(capture: &str) -> bool {
    let current = powerline_extractor_sha256();
    let previous = match current.as_str() {
        "f2957ec3e3d9529a9ada652047723227aeef053d9f7e52cd29e00bce800de29a" =>
            "65e229a162f351b19800135ece506e3de3a7ba38466a57ae4efb76db3c8ceb5d",
        current => current,
    };
    // The exact procedural scene reader produced identical utility records.
    let compatible = match previous {
        "65e229a162f351b19800135ece506e3de3a7ba38466a57ae4efb76db3c8ceb5d" =>
            "46db073143508e92f7376d0ca906358779976264eb347362502a81ab209de2b7",
        current => current,
    };
    capture == current
        || capture == previous
        || capture == compatible
        || matches!((compatible, capture),
            ("7c026605f98315df80f6b695a590f21eff3361c84cd2ac5867dee91460ae8ef2"
             | "46db073143508e92f7376d0ca906358779976264eb347362502a81ab209de2b7"
             | "cdd0b5755aa2dc3fb1943c122462fdf0f25abec8f2039b71053ddd7ce053cc61",
             "7c026605f98315df80f6b695a590f21eff3361c84cd2ac5867dee91460ae8ef2"
             | "cdd0b5755aa2dc3fb1943c122462fdf0f25abec8f2039b71053ddd7ce053cc61"
             | "f832ddcf0f6c53698b81eb816d49c1cd55583abd521ce71bf53f15d9f4156a02"
             | "3c777de963971dff14e9604a8ba94cead60c7450a86a1c4caa1097966498e351")
            | ("5aede71643ff7bf890a262a3c90201b358e0138349945bb7ed8c3f093c50de4f"
               | "fcf08be23e8bab90d54b3b5fdf2e557fe1b11dc9ab1da5344865c83ec82042d8"
               | "75c46103598ef9b99529ff0756d81193077d551912f55317fd490df57e306412",
               "5aede71643ff7bf890a262a3c90201b358e0138349945bb7ed8c3f093c50de4f"
               | "75c46103598ef9b99529ff0756d81193077d551912f55317fd490df57e306412"
               | "4a82c7ec0b3611fa247b064e4c54f455900d36d7d78a766c7a0577743b6cde68"
               | "98c5db66f294393b6f9f4b6a5d545ce9322e0d846ac9cefd431666fb2390e97c"))
}
pub(crate) fn scene_wire_extractor_matches(capture: &str) -> bool {
    let current = scene_wire_extractor_sha256();
    let before_affine = match current.as_str() {
        "0a006cdb173979d44c94740ec1712c9a0ff24f76b3c7a61844d75e53b73e1d56" =>
            "a258716e12d90cfd98b1a55990a6f806654eac619c1d74399ed96ad8183c14c6",
        current => current,
    };
    let preceding = match before_affine {
        "a258716e12d90cfd98b1a55990a6f806654eac619c1d74399ed96ad8183c14c6" =>
            "f04a4c04c7b6e4ba2ac81fd917470d5ddf568b9aed1c4ab2560d3b9bd8433539",
        current => current,
    };
    // Name probes and reused transform reads produced the same cable fields
    // and transforms for both reviewed installation layouts.
    let previous = match preceding {
        "f04a4c04c7b6e4ba2ac81fd917470d5ddf568b9aed1c4ab2560d3b9bd8433539" =>
            "bb11a262e1409d52f9cca3eaca1037b2dfecfc8cbafa466532eac0c40f219191",
        current => current,
    };
    let compatible = match previous {
        "bb11a262e1409d52f9cca3eaca1037b2dfecfc8cbafa466532eac0c40f219191" =>
            "8769fd8fa7d02e32f1db4ada4bc7995955af600bcdeba11e22eeecb505b77aac",
        current => current,
    };
    capture == current
        || capture == before_affine
        || capture == preceding
        || capture == previous
        || capture == compatible
        || matches!((compatible, capture),
            ("efe3e34b66e30637c97f40b34a795888e28bfbad3c8dc26e012e1b51ee95ffcd"
             | "8769fd8fa7d02e32f1db4ada4bc7995955af600bcdeba11e22eeecb505b77aac"
             | "66b1aa59f4ec399dacd2e8aef65ffc988582a86183346716dd2df241109c5de1",
             "efe3e34b66e30637c97f40b34a795888e28bfbad3c8dc26e012e1b51ee95ffcd"
             | "66b1aa59f4ec399dacd2e8aef65ffc988582a86183346716dd2df241109c5de1"
             | "bda15aafff95128391e97af52f83dd415cb31e0d58c08015fa352ac7d513ac77"
             | "6b96bd7882dd125ee5b6f9cdd726f57ad662b5e265e25f2ab784750a26ad96c7")
            | ("8f2b5d51e28c362fc50039ae6418e48dedb0b1ecf352ace699f09823dbf7b915"
               | "a5f298a5dff50c1d40e89aad890f1b05327e51b858a41a8e65164999d257377b"
               | "8c52c564de640f16de5b5fd893d4e2bd76a8b6b65a14c3bced4c98bb972225ed",
               "8f2b5d51e28c362fc50039ae6418e48dedb0b1ecf352ace699f09823dbf7b915"
               | "8c52c564de640f16de5b5fd893d4e2bd76a8b6b65a14c3bced4c98bb972225ed"
               | "1e7e0a159adf64e40fdde7ce1c3cbfa63be44a24f6879008a56bc4008caa0bd9"
               | "73f6389c175c8d717d3fe3d8873bae501c0b41ba44ea450bcc852471f78eb1f8"))
}
fn utility_extractor_sha256(prefix: impl Iterator<Item = &'static str>) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in prefix.chain([
        "extract_powerline_source.py", "extract_water_treatment_source.py",
        "inspect_prefab_runtime.py", "extract_uniform_cliffs.py", "extract_decor_source.py",
        "extract_spawn_source.py", "extract_verified_prefabs.py", "lazy_unity_bundle.py",
        "prefab_geometry.py", "decor_collider_meshes.py", "gameplay_sources.py",
        "road_stages.py", "monument_placement_attributes.py", "mapper_paths.py",
        "road_object_stages.py", "monument_stages.py", "mapper_progress.py",
        "terrain_hole_shapes.py", "resolve_prefab_textures.py", "texture_payload_cache.py",
    ]) {
        let (_, source) = SCRIPTS.iter().find(|(candidate, _)| *candidate == name)
            .expect("bundled utility helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn monument_tag_extractor_sha256() -> String {
    placement_extractor_sha256([
        "extract_monument_tags.py", "monument_tag_geometry.py", "metadata_transforms.py",
    ].into_iter())
}
fn placement_extractor_sha256(prefix: impl Iterator<Item = &'static str>) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in prefix.chain([
        "extract_water_treatment_source.py",
        "monument_stages.py",
        "mapper_progress.py",
        "extract_uniform_cliffs.py",
        "extract_decor_source.py",
        "extract_spawn_source.py",
        "extract_verified_prefabs.py",
        "lazy_unity_bundle.py",
        "prefab_geometry.py",
        "decor_collider_meshes.py",
        "gameplay_sources.py",
        "road_stages.py",
        "monument_placement_attributes.py",
        "mapper_paths.py",
        "road_object_stages.py",
        "terrain_hole_shapes.py",
    ]) {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("bundled pipe helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn coastal_cliff_extractor_sha256() -> String {
    cliff_extractor_sha256(Some("extract_coastal_cliffs.py"))
}
fn cliff_extractor_sha256(prefix: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    if let Some(prefix) = prefix {
        let (name, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == prefix)
            .expect("bundled coastal cliff helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    for name in [
        "extract_uniform_cliffs.py",
        "extract_decor_source.py",
        "extract_spawn_source.py",
        "extract_verified_prefabs.py",
        "lazy_unity_bundle.py",
        "prefab_geometry.py",
        "decor_collider_meshes.py",
        "gameplay_sources.py",
        "road_stages.py",
        "monument_placement_attributes.py",
        "mapper_paths.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("bundled cliff helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn monument_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_monument_source.py",
        "prefab_geometry.py",
        "lazy_unity_bundle.py",
        "model_preview_geometry.py",
        "monument_placement_attributes.py",
        "gameplay_sources.py",
        "extract_verified_prefabs.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("bundled monument helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn dungeon_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in ["extract_dungeon_source.py", "dungeon_prefabs.py", "prefab_paths.py",
        "prefab_geometry.py", "monument_stages.py", "extract_monument_stages.py",
        "extract_verified_prefabs.py", "lazy_unity_bundle.py", "mapper_paths.py"] {
        let (_, source) = SCRIPTS.iter().find(|(n, _)| *n == name).expect("bundled dungeon helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn tunnel_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "inspect_tunnel_grid.py",
        "tunnel_prefabs.py",
        "dungeon_prefabs.py",
        "prefab_paths.py",
        "prefab_geometry.py",
        "extract_monument_stages.py",
        "extract_verified_prefabs.py",
        "monument_stages.py",
        "lazy_unity_bundle.py",
        "mapper_paths.py",
        "tunnel_identities.py",
        "extract_prefab_identities.py",
        "resolve_prefab_textures.py",
        "texture_payload_cache.py",
        "mapper_progress.py",
        "monument_placement_attributes.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("bundled tunnel helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
// Component and child transform reuse, LOD validation and scene selection
// preserve the capture data. Check both LF and mixed Windows line endings.
// An unrecognized producer cannot reuse preceding captures.
pub(crate) fn monument_cache_extractors(current: String) -> Vec<String> {
    const TRAVERSAL_LF: &str = "7753b4587de730200b93a165b468120092fdeaaada9da735de09b1edd6ff5996";
    const TRAVERSAL_WINDOWS: &str = "43b6a2c82bd9ce0d18305550d4f08b87ec95b9385efed3393ce469dae8bfd7a5";
    const PREVIEW_LF: &str = "bb23af753e9e4a5354af9e957b80d1f2bf5aef883353da3318ca173bc83a4c05";
    const PREVIEW_WINDOWS: &str = "23d4e49ad73d3bbba4a94f68bd1ceb15f3f60cf074b217a9d530fe71f0ed4a56";
    const OPTIMIZED_LF: &str = "72be0904d9c3a98bd65a8d4425f95fa1a336a4ae0024b067e0460abce5ea580b";
    const OPTIMIZED: &str = "918ca875c51849f2b1569d5f15419ad4d23eb678787d5b1547ab8e2868adba6b";
    const COMPONENT_REUSE: &str = "eb5d77fb7fb8436f2c4f79297e071ca2d66ae42d102c7a2788f105d193f98c3f";
    const ORIGINAL: &str = "7dfb25b180fadd23e4117b46b0153e8e1bb65594949af0ab3c04e8ce06339927";
    let mut extractors = vec![current];
    match extractors[0].as_str() {
        TRAVERSAL_LF => extractors.extend([
            PREVIEW_LF.into(),
            PREVIEW_WINDOWS.into(),
            OPTIMIZED_LF.into(),
            OPTIMIZED.into(),
            COMPONENT_REUSE.into(),
            ORIGINAL.into(),
        ]),
        TRAVERSAL_WINDOWS => extractors.extend([
            PREVIEW_WINDOWS.into(),
            PREVIEW_LF.into(),
            OPTIMIZED.into(),
            OPTIMIZED_LF.into(),
            COMPONENT_REUSE.into(),
            ORIGINAL.into(),
        ]),
        PREVIEW_LF => extractors.extend([
            OPTIMIZED_LF.into(),
            OPTIMIZED.into(),
            COMPONENT_REUSE.into(),
            ORIGINAL.into(),
        ]),
        PREVIEW_WINDOWS => extractors.extend([
            OPTIMIZED.into(),
            OPTIMIZED_LF.into(),
            COMPONENT_REUSE.into(),
            ORIGINAL.into(),
        ]),
        OPTIMIZED => extractors.extend([COMPONENT_REUSE.into(), ORIGINAL.into()]),
        COMPONENT_REUSE => extractors.push(ORIGINAL.into()),
        _ => {}
    }
    extractors
}
// These texture-driver changes preserve the captured data. Gate reuse on
// the exact producer pairs for the Windows and LF source layouts.
fn tunnel_cache_extractors(current: String) -> Vec<String> {
    const CURRENT: &str = "4fe2c8981e150d6409e565e81cfc486c860537d341f14e63fcd6f24c6f9472fa";
    const PREVIOUS: &str = "1585f5102f90916b8b203e1e6aac6983c666999840353ec46ad9c652bbab44ac";
    const TEXTURE_REUSE: &str = "5958e753cc2427edf488e75d7a2e0a0bd180c645fa9c6ebafad4b65260a8e1b9";
    const TEXTURE_REUSE_LF: &str = "071b8a3a761509080c065f68e99f9fddfcdb72cc8e2c773e7e096ab97f507e31";
    const PREVIOUS_LF: &str = "a8ea8abeef9cb009afe03b305622355abb1e14f3419964fae4e6168d6b02ff04";
    const ENCODING_OVERLAP: &str = "c26d7f67b1e436a17274b930069336274d0f6d3c6a1a8d8102de065f62578aa8";
    const ENCODING_OVERLAP_LF: &str = "5fbe62816815b5664bc2de2f47129ebb93b389669ef50048111a0c40a70e2c56";
    const BEFORE_SCENE_SELECTION: &str = "68930424b82e361c59a71eb800968dfcbe9ae98ea506025d0a3691f6bf4179cc";
    const SCENE_SELECTION: &str = "ac342a29d93c64e3870f6b3b0a58501c4b64dca909c10804c7bbf4da5b9c475d";
    let mut extractors = vec![current];
    if extractors[0] == SCENE_SELECTION {
        extractors.push(BEFORE_SCENE_SELECTION.into());
    }
    match extractors.last().unwrap().as_str() {
        BEFORE_SCENE_SELECTION => {
            extractors.extend([ENCODING_OVERLAP.into(), TEXTURE_REUSE.into(), CURRENT.into(), PREVIOUS.into()]);
        }
        "685cb8a9dd7a8e62dc0acc1dc5e3ea89fcc1c0e7a614f6cd15f3f57f5471a11a" => {
            extractors.extend([ENCODING_OVERLAP_LF.into(), TEXTURE_REUSE_LF.into(), PREVIOUS_LF.into()]);
        }
        ENCODING_OVERLAP => {
            extractors.extend([TEXTURE_REUSE.into(), CURRENT.into(), PREVIOUS.into()]);
        }
        ENCODING_OVERLAP_LF => {
            extractors.extend([TEXTURE_REUSE_LF.into(), PREVIOUS_LF.into()]);
        }
        TEXTURE_REUSE => {
            extractors.extend([CURRENT.into(), PREVIOUS.into()]);
        }
        TEXTURE_REUSE_LF => extractors.push(PREVIOUS_LF.into()),
        CURRENT => extractors.push(PREVIOUS.into()),
        _ => {}
    }
    extractors
}
pub(crate) fn tunnel_extractor_candidates() -> Vec<String> {
    tunnel_cache_extractors(tunnel_extractor_sha256())
}
pub(crate) fn catalog_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_prefab_modifiers.py",
        "prefab_geometry.py",
        "no_build_geometry.py",
        "decor_collider_meshes.py",
        "lazy_unity_bundle.py",
    ] {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(
            SCRIPTS
                .iter()
                .find(|(n, _)| *n == name)
                .expect("bundled catalog helper")
                .1
                .as_bytes(),
        );
    }
    format!("{:x}", digest.finalize())
}
pub(crate) const MONUMENT_STAGE_CODE_PROFILE: &str =
    include_str!("../../../compatibility/monuments/client-0f703d43-coastal.json");
pub(crate) const CURRENT_MONUMENT_STAGE_CODE_PROFILE: &str =
    include_str!("../../../compatibility/monuments/client-8dd4e235-stages.json");
pub(crate) const ROADSIDE_PROFILE: &str =
    include_str!("../../../compatibility/monuments/client-0f703d43-roadside.json");
pub(crate) const CURRENT_ROADSIDE_PROFILE: &str =
    include_str!("../../../compatibility/monuments/client-8dd4e235-roadside.json");
pub(crate) fn road_layout_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_road_layout_assets.py",
        "road_layout_assets.py",
        "prefab_paths.py",
        "prefab_geometry.py",
        "lazy_unity_bundle.py",
        "extract_verified_prefabs.py",
        "mapper_progress.py",
        "mapper_paths.py",
        "road_stages.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("Bundled ordered road helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn roadside_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_roadside_stage.py",
        "roadside_stages.py",
        "road_stages.py",
        "extract_roadside_connectors.py",
        "extract_monument_stages.py",
        "monument_stages.py",
        "prefab_paths.py",
        "prefab_geometry.py",
        "extract_verified_prefabs.py",
        "lazy_unity_bundle.py",
        "mapper_paths.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("Bundled roadside helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn monument_stage_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_monument_stages.py",
        "monument_stages.py",
        "extract_verified_prefabs.py",
        "prefab_geometry.py",
        "lazy_unity_bundle.py",
        "mapper_paths.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("Bundled monument stage helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn offshore_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_offshore_stages.py",
        "offshore_stages.py",
        "monument_stages.py",
        "extract_verified_prefabs.py",
        "lazy_unity_bundle.py",
        "extract_monument_stages.py",
        "prefab_geometry.py",
        "mapper_paths.py",
        "offshore_prefabs.py",
        "monument_placement_attributes.py",
        "prefab_paths.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("Bundled offshore stage helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn river_objects_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_river_objects.py",
        "road_object_stages.py",
        "road_stages.py",
        "monument_stages.py",
        "tunnel_prefabs.py",
        "dungeon_prefabs.py",
        "prefab_paths.py",
        "prefab_geometry.py",
        "monument_placement_attributes.py",
        "extract_decor_source.py",
        "extract_verified_prefabs.py",
        "lazy_unity_bundle.py",
        "mapper_paths.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("Bundled river object helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
pub(crate) fn river_extractor_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for name in [
        "extract_river_meshes.py",
        "extract_rail_source.py",
        "extract_verified_prefabs.py",
        "rail_mesh_buffers.py",
        "lazy_unity_bundle.py",
        "river_mesh_stages.py",
        "monument_stages.py",
        "mapper_paths.py",
    ] {
        let (_, source) = SCRIPTS
            .iter()
            .find(|(n, _)| *n == name)
            .expect("Bundled river helper");
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(source.as_bytes());
    }
    format!("{:x}", digest.finalize())
}
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn capture(
    stream: impl Read + Send + 'static,
    overflow: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<Vec<u8>, String>> {
    std::thread::spawn(move || {
        let mut raw = Vec::new();
        stream
            .take(65537)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > 65536 {
            overflow.store(true, Ordering::Relaxed);
        }
        Ok(raw)
    })
}
fn capture_progress(
    stream: impl Read + Send + 'static,
    overflow: Arc<AtomicBool>,
    reporter: crate::loading_progress::Reporter,
) -> std::thread::JoinHandle<Result<Vec<u8>, String>> {
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stream);
        let mut raw = Vec::new();
        let mut total = 0usize;
        let mut completed_prefabs = 0_u64;
        loop {
            let mut line = Vec::new();
            let count = reader
                .by_ref()
                .take(4097)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            total += count;
            if count > 4096 || total > 4 * 1024 * 1024 {
                overflow.store(true, Ordering::Relaxed);
                break;
            }
            if !reporter.structured_line(String::from_utf8_lossy(&line).trim_end()) {
                if line.starts_with(b"{")
                    && let Ok(item) = serde_json::from_slice::<MonumentTagProgress>(&line)
                    && item.complete && item.tags <= 4096 && completed_prefabs < 512
                    && item.prefab.len() <= 512
                    && item.prefab.starts_with("assets/") && item.prefab.ends_with(".prefab") {
                    completed_prefabs += 1;
                    reporter.update(&item.prefab, completed_prefabs, None, crate::loading_progress::Unit::Items);
                    continue;
                }
                if raw.len() + count > 65536 {
                    overflow.store(true, Ordering::Relaxed);
                    break;
                }
                raw.extend(line);
            }
        }
        Ok(raw)
    })
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MonumentTagProgress {
    prefab: String,
    tags: usize,
    complete: bool,
}
fn resolve(
    directory: &Path,
    local: &Path,
    override_python: Option<&OsStr>,
) -> Result<(PathBuf, PathBuf), String> {
    let bundled = directory.join("tools");
    let executable = bundled.join(if cfg!(windows) {
        "python/python.exe"
    } else {
        "python/bin/python3"
    });
    let python = override_python
        .map(PathBuf::from)
        .or_else(|| executable.is_file().then_some(executable))
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
    let modules = if bundled.join("unitypy/UnityPy").is_dir() {
        bundled.join("unitypy")
    } else {
        local.join("tools/unitypy")
    };
    if !modules.join("UnityPy").is_dir() {
        return Err("Installation metadata requires Mapper's metadata runtime".into());
    }
    Ok((python, modules))
}

#[derive(Default)]
pub(crate) struct ScriptWorkspace {
    staged: Mutex<Option<tempfile::TempDir>>,
    #[cfg(windows)]
    manifest_fields: Mutex<()>,
}

impl ScriptWorkspace {
    fn directory(&self) -> Result<PathBuf, String> {
        let mut staged = self.staged.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(folder) = staged.as_ref() {
            return Ok(folder.path().to_owned());
        }
        let folder = tempfile::Builder::new()
            .prefix("mapper-metadata-code-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        for (name, source) in SCRIPTS {
            std::fs::write(folder.path().join(name), source).map_err(|e| e.to_string())?;
        }
        let path = folder.path().to_owned();
        *staged = Some(folder);
        Ok(path)
    }

    #[cfg(windows)]
    fn identity_fields(
        &self,
        directory: &Path,
        root: &Path,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<PathBuf>, String> {
        use std::sync::TryLockError;

        // All three callers retain their verified installation scope until the writer exits.
        let receipt = crate::installation::hash_file(
            root, "Bundles/shared/content.bundle", 8 * 1024_u64.pow(3), cancelled,
        )?;
        let _guard = loop {
            if cancelled() {
                return Ok(None);
            }
            match self.manifest_fields.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(25)),
            }
        };
        let fields = directory.join(format!("manifest-fields-{}.json", receipt.sha256));
        if fields.is_file() {
            return Ok(Some(fields));
        }
        let stage = tempfile::tempdir_in(directory).map_err(|e| e.to_string())?;
        let output = stage.path().join("fields.json");
        if !crate::native_texture_sources::read_identities(
            crate::native_texture_sources::IdentityRead::ManifestFields,
            root, &output, cancelled,
        )? {
            return Ok(None);
        }
        std::fs::rename(output, &fields).map_err(|e| e.to_string())?;
        Ok(Some(fields))
    }
}

/// Keep the embedded read-only scripts available across preparation workers.
/// The directory is staged lazily and removed after the last operation exits.
pub(crate) fn script_workspace() -> Arc<ScriptWorkspace> {
    static ACTIVE: Mutex<Weak<ScriptWorkspace>> = Mutex::new(Weak::new());
    let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(workspace) = active.upgrade() {
        return workspace;
    }
    let workspace = Arc::new(ScriptWorkspace::default());
    *active = Arc::downgrade(&workspace);
    workspace
}

pub(crate) fn run(
    script: Script,
    args: &[OsString],
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    let stage = match script {
        Script::Catalog => "Reading monument metadata".into(),
        Script::Textures if args.iter().any(|arg| arg == "--discover-sources") => {
            "Finding terrain textures".into()
        }
        Script::Textures => "Preparing terrain textures".into(),
        _ => format!(
            "Processing {}",
            script
                .name()
                .trim_start_matches("extract_")
                .trim_end_matches(".py")
                .replace('_', " ")
        ),
    };
    let progress = crate::loading_progress::begin(&stage, None);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (python, modules) = resolve(
        exe.parent().ok_or("Missing application directory")?,
        &crate::storage::local_data()?,
        std::env::var_os("MAPPER_PYTHON").as_deref(),
    )?;
    let scripts = script_workspace();
    let code = scripts.directory()?;
    let mut command = Command::new(python);
    if matches!(script, Script::Textures) {
        let cap = 2;
        #[cfg(windows)]
        let cap = match papyrus_file_id::available_memory()
            .map(|memory| memory.physical.min(memory.commit) / (1024 * 1024 * 1024))
        {
            Ok(8..) => 6,
            Ok(3..) => 4,
            _ => cap,
        };
        let workers = crate::generation_resources::current()
            .effective_workers()
            .min(cap);
        command.env("MAPPER_TEXTURE_WORKERS", workers.to_string());
    }
    #[cfg(windows)]
    let identity_fields = if matches!(script, Script::Tunnels | Script::Powerlines | Script::SceneWires)
        && let [flag, root, ..] = args
        && flag == "--rust-root"
    {
        scripts.identity_fields(&code, Path::new(root), &cancelled)?
    } else {
        None
    };
    // Never inherit a receipt file from another operation. This child runs
    // synchronously while its parent retains every exported deny-write lease.
    command.env_remove("MAPPER_VERIFIED_SOURCES");
    let receipts = crate::source_lease::receipts();
    let _receipt = if !receipts.is_empty() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format":"mapper-live-source-leases-1", "parent":std::process::id(), "sources":receipts,
        }))
        .map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 {
            return Err("Live source receipts exceed their budget".into());
        }
        let mut receipt = tempfile::Builder::new()
            .prefix("mapper-source-receipts-")
            .suffix(".json")
            .tempfile()
            .map_err(|e| e.to_string())?;
        receipt.write_all(&bytes).map_err(|e| e.to_string())?;
        let path = receipt.into_temp_path();
        command.env("MAPPER_VERIFIED_SOURCES", &path);
        Some(path)
    } else {
        None
    };
    command.args(["-I","-B","-u","-c",concat!(
        "import sys,runpy; code,modules,script=sys.argv[1:4]; sys.path[:0]=[code,modules]; ",
        "import metadata_object_reader; metadata_object_reader.install(); ",
        "sys.argv=sys.argv[3:]; runpy.run_path(script,run_name='__main__')")])
        .arg(&code).arg(modules).arg(code.join(script.name())).args(args)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    if let Some(fields) = identity_fields {
        command.arg("--identity-fields").arg(fields);
    }
    execute(command, progress.reporter(), cancelled)
}
/// Isolated parser execution shared by the native reader and Python adapters.
/// The caller retains source leases and owns all output paths for this call.
pub(crate) fn execute(
    mut command: Command,
    reporter: crate::loading_progress::Reporter,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if cancelled() {
        return Ok(false);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = Guard(
        command
            .spawn()
            .map_err(|e| format!("Cannot start metadata preparation: {e}"))?,
    );
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = capture_progress(child.0.stdout.take().unwrap(), overflow.clone(), reporter);
    let stderr = capture(child.0.stderr.take().unwrap(), overflow.clone());
    let start = Instant::now();
    let status = loop {
        if cancelled() {
            return Ok(false);
        }
        if overflow.load(Ordering::Relaxed) {
            return Err("Metadata preparation exceeded output budget".into());
        }
        if start.elapsed() > Duration::from_secs(1800) {
            return Err("Metadata preparation exceeded time budget".into());
        }
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = stdout
        .join()
        .map_err(|_| "Metadata output reader failed")??;
    let errors = stderr
        .join()
        .map_err(|_| "Metadata error reader failed")??;
    if overflow.load(Ordering::Relaxed) {
        return Err("Metadata preparation exceeded output budget".into());
    }
    if !status.success() {
        return Err(format!(
            "Metadata preparation failed: {}",
            String::from_utf8_lossy(&errors)
                .chars()
                .take(2048)
                .collect::<String>()
        ));
    }
    Ok(!cancelled())
}
pub(crate) fn document(cache: &Path, name: &str) -> Result<PathBuf, String> {
    let mut found = None;
    for (i, entry) in std::fs::read_dir(cache)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if i >= 256 {
            return Err("Metadata output exceeds directory budget".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.len() != 64
            || !id
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            || !entry.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            continue;
        }
        let path = entry.path().join(name);
        if path.is_file() {
            if found.is_some() {
                return Err("Ambiguous metadata output".into());
            }
            found = Some(path);
        }
    }
    found.ok_or_else(|| "Metadata preparation produced no complete document".into())
}
pub(crate) struct PublishedDocument {
    pub path: PathBuf,
    pub moved: bool,
}

pub(crate) fn publish(
    document: &Path,
    cache: &Path,
    expected_sha: &str,
    limit: u64,
) -> Result<PathBuf, String> {
    publish_with_status(document, cache, expected_sha, limit).map(|publication| publication.path)
}

pub(crate) fn publish_with_status(
    document: &Path,
    cache: &Path,
    expected_sha: &str,
    limit: u64,
) -> Result<PublishedDocument, String> {
    use sha2::{Digest, Sha256};
    let folder = document
        .parent()
        .ok_or("Missing metadata output directory")?;
    let id = folder
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("Invalid metadata output identity")?;
    if id.len() != 64
        || !id
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err("Invalid metadata cache identity".into());
    }
    let checked = |path: &Path| -> Result<(), String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(limit + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() as u64 > limit || format!("{:x}", Sha256::digest(&raw)) != expected_sha {
            return Err("Metadata cache content differs; existing data was retained".into());
        }
        Ok(())
    };
    checked(document)?;
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let destination = cache.join(id);
    let path = destination.join(document.file_name().ok_or("Missing metadata filename")?);
    if destination.exists() {
        checked(&path)?;
        return Ok(PublishedDocument { path, moved: false });
    }
    // Both paths are direct child directories of their respective owned cache
    // roots, with a validated hexadecimal leaf. Never replace an existing cache.
    match std::fs::rename(folder, &destination) {
        Ok(()) => Ok(PublishedDocument { path, moved: true }),
        Err(_) if destination.exists() => {
            checked(&path)?;
            Ok(PublishedDocument { path, moved: false })
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Stage an unkeyed, privately captured document for immutable publication.
pub(crate) fn publish_unkeyed(
    path: &Path,
    cache: &Path,
    expected_sha: &str,
    limit: u64,
) -> Result<PathBuf, String> {
    if expected_sha.len() != 64
        || !expected_sha
            .bytes()
            .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
    {
        return Err("Invalid metadata cache identity".into());
    }
    let folder = path
        .parent()
        .ok_or("Missing metadata capture directory")?
        .join(expected_sha);
    std::fs::create_dir(&folder).map_err(|error| error.to_string())?;
    let document = folder.join(
        path.file_name()
            .ok_or("Missing metadata capture filename")?,
    );
    std::fs::rename(path, &document).map_err(|error| error.to_string())?;
    publish(&document, cache, expected_sha, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workers_share_complete_scripts_until_preparation_finishes() {
        let workspace = Arc::new(ScriptWorkspace::default());
        let [first, second] = std::thread::scope(|scope| {
            let workers = std::array::from_fn(|_| {
                let scripts = workspace.clone();
                scope.spawn(move || scripts.directory().unwrap())
            });
            workers.map(|worker| worker.join().unwrap())
        });
        assert_eq!(first, second);
        assert_eq!(std::fs::read_dir(&first).unwrap().count(), SCRIPTS.len());
        for (name, source) in SCRIPTS {
            assert_eq!(std::fs::read(first.join(name)).unwrap(), source.as_bytes());
        }
        drop(workspace);
        assert!(!first.exists());
    }

    #[test]
    fn monument_cache_migration_requires_a_verified_producer() {
        let current = monument_extractor_sha256();
        let preview = "23d4e49ad73d3bbba4a94f68bd1ceb15f3f60cf074b217a9d530fe71f0ed4a56";
        let preview_lf = "bb23af753e9e4a5354af9e957b80d1f2bf5aef883353da3318ca173bc83a4c05";
        let optimized = "918ca875c51849f2b1569d5f15419ad4d23eb678787d5b1547ab8e2868adba6b";
        let optimized_lf = "72be0904d9c3a98bd65a8d4425f95fa1a336a4ae0024b067e0460abce5ea580b";
        let component_reuse = "eb5d77fb7fb8436f2c4f79297e071ca2d66ae42d102c7a2788f105d193f98c3f";
        let previous = "7dfb25b180fadd23e4117b46b0153e8e1bb65594949af0ab3c04e8ce06339927";
        let selected = monument_cache_extractors(current.clone());
        assert_eq!(selected.len(), 7);
        assert_eq!(selected.first(), Some(&current));
        assert!(selected[1..3].contains(&preview.to_owned()));
        assert!(selected[1..3].contains(&preview_lf.to_owned()));
        assert!(selected[3..5].contains(&optimized.to_owned()));
        assert!(selected[3..5].contains(&optimized_lf.to_owned()));
        assert_eq!(selected[5..], [component_reuse, previous]);
        assert_eq!(
            monument_cache_extractors(preview.into()),
            vec![preview, optimized, optimized_lf, component_reuse, previous]
        );
        assert_eq!(
            monument_cache_extractors(optimized.into()),
            vec![optimized, component_reuse, previous]
        );
        assert_eq!(
            monument_cache_extractors(component_reuse.into()),
            vec![component_reuse, previous]
        );
        assert_eq!(monument_cache_extractors(previous.into()), vec![previous]);
        let changed = "0".repeat(64);
        assert_eq!(monument_cache_extractors(changed.clone()), vec![changed]);
    }
    #[test]
    fn tunnel_cache_migration_requires_the_exact_verified_producer_pair() {
        let current = "4fe2c8981e150d6409e565e81cfc486c860537d341f14e63fcd6f24c6f9472fa";
        let previous = "1585f5102f90916b8b203e1e6aac6983c666999840353ec46ad9c652bbab44ac";
        let texture_reuse = "5958e753cc2427edf488e75d7a2e0a0bd180c645fa9c6ebafad4b65260a8e1b9";
        assert_eq!(
            tunnel_cache_extractors(texture_reuse.into()),
            vec![texture_reuse, current, previous]
        );
        assert_eq!(tunnel_cache_extractors(current.into()), vec![current, previous]);
        assert_eq!(tunnel_cache_extractors(previous.into()), vec![previous]);
        let changed = "0".repeat(64);
        assert_eq!(tunnel_cache_extractors(changed.clone()), vec![changed]);
    }

    #[test]
    fn texture_driver_reuse_retains_the_verified_tunnel_and_utility_captures() {
        assert!(tunnel_extractor_candidates().iter().any(|hash|
            hash == "68930424b82e361c59a71eb800968dfcbe9ae98ea506025d0a3691f6bf4179cc"));
        assert!(powerline_extractor_matches(
            "65e229a162f351b19800135ece506e3de3a7ba38466a57ae4efb76db3c8ceb5d"));
        assert!(scene_wire_extractor_matches(
            "a258716e12d90cfd98b1a55990a6f806654eac619c1d74399ed96ad8183c14c6"));
        assert!(scene_wire_extractor_matches(
            "f04a4c04c7b6e4ba2ac81fd917470d5ddf568b9aed1c4ab2560d3b9bd8433539"));
        assert!(powerline_extractor_matches(
            "46db073143508e92f7376d0ca906358779976264eb347362502a81ab209de2b7"));
        assert!(scene_wire_extractor_matches(
            "8769fd8fa7d02e32f1db4ada4bc7995955af600bcdeba11e22eeecb505b77aac"));
        assert!(scene_wire_extractor_matches(
            "bb11a262e1409d52f9cca3eaca1037b2dfecfc8cbafa466532eac0c40f219191"));
        assert!(tunnel_extractor_candidates()[1..].iter().any(|hash| [
            "5958e753cc2427edf488e75d7a2e0a0bd180c645fa9c6ebafad4b65260a8e1b9",
            "071b8a3a761509080c065f68e99f9fddfcdb72cc8e2c773e7e096ab97f507e31",
        ].contains(&hash.as_str())));
        assert!([
            "7c026605f98315df80f6b695a590f21eff3361c84cd2ac5867dee91460ae8ef2",
            "5aede71643ff7bf890a262a3c90201b358e0138349945bb7ed8c3f093c50de4f",
        ].into_iter().any(powerline_extractor_matches));
        assert!([
            "efe3e34b66e30637c97f40b34a795888e28bfbad3c8dc26e012e1b51ee95ffcd",
            "8f2b5d51e28c362fc50039ae6418e48dedb0b1ecf352ace699f09823dbf7b915",
        ].into_iter().any(scene_wire_extractor_matches));
        let old_tunnel = [
            "4fe2c8981e150d6409e565e81cfc486c860537d341f14e63fcd6f24c6f9472fa",
            "a8ea8abeef9cb009afe03b305622355abb1e14f3419964fae4e6168d6b02ff04",
        ];
        assert!(tunnel_extractor_candidates().iter().any(|hash| old_tunnel.contains(&hash.as_str())));
        assert!([
            "f832ddcf0f6c53698b81eb816d49c1cd55583abd521ce71bf53f15d9f4156a02",
            "4a82c7ec0b3611fa247b064e4c54f455900d36d7d78a766c7a0577743b6cde68",
        ].into_iter().any(powerline_extractor_matches));
        assert!([
            "bda15aafff95128391e97af52f83dd415cb31e0d58c08015fa352ac7d513ac77",
            "1e7e0a159adf64e40fdde7ce1c3cbfa63be44a24f6879008a56bc4008caa0bd9",
        ].into_iter().any(scene_wire_extractor_matches));
        assert!([
            "3c777de963971dff14e9604a8ba94cead60c7450a86a1c4caa1097966498e351",
            "98c5db66f294393b6f9f4b6a5d545ce9322e0d846ac9cefd431666fb2390e97c",
        ].into_iter().any(powerline_extractor_matches));
        assert!([
            "6b96bd7882dd125ee5b6f9cdd726f57ad662b5e265e25f2ab784750a26ad96c7",
            "73f6389c175c8d717d3fe3d8873bae501c0b41ba44ea450bcc852471f78eb1f8",
        ].into_iter().any(scene_wire_extractor_matches));
        assert!(!powerline_extractor_matches(&"0".repeat(64)));
        assert!(!scene_wire_extractor_matches(&"0".repeat(64)));
    }

    #[test]
    fn progress_pipe_streams_updates_without_spending_diagnostic_budget() {
        let scope = crate::loading_progress::begin("Pipe test", None);
        let reporter = scope.reporter();
        let line = b"MAPPER_PROGRESS {\"schema\":1,\"stage\":\"Reading scenes\",\"item\":\"scene.bundle\",\"completed\":1,\"total\":4}\n";
        let mut stream = line.repeat(1000);
        stream.extend(b"finished\n");
        let overflow = Arc::new(AtomicBool::new(false));
        let raw = capture_progress(std::io::Cursor::new(stream), overflow.clone(), reporter)
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(raw, b"finished\n");
        assert!(!overflow.load(Ordering::Relaxed));
        assert!(
            crate::loading_progress::active()
                .iter()
                .any(|s| s.label == "Pipe test"
                    && s.item == "scene.bundle"
                    && s.fraction() == Some(0.25))
        );
        let overflow = Arc::new(AtomicBool::new(false));
        capture_progress(
            std::io::Cursor::new(vec![b'x'; 4097]),
            overflow.clone(),
            scope.reporter(),
        )
        .join()
        .unwrap()
        .unwrap();
        assert!(overflow.load(Ordering::Relaxed));
    }
    #[test]
    fn monument_tag_progress_streams_the_full_roster_and_retains_incomplete_diagnostics() {
        let scope = crate::loading_progress::begin("Tag progress test", None);
        let mut stream = Vec::new();
        for index in 0..512 {
            let line = serde_json::json!({"prefab":format!("assets/{index}/{}.prefab","a".repeat(400)),"tags":10,"complete":true});
            stream.extend(serde_json::to_vec(&line).unwrap());
            stream.push(b'\n');
        }
        assert!(stream.len() > 65536);
        let incomplete = b"{\"prefab\":\"assets/bad.prefab\",\"tags\":0,\"complete\":false}\n";
        stream.extend(incomplete);
        let overflow = Arc::new(AtomicBool::new(false));
        let raw = capture_progress(
            std::io::Cursor::new(stream),
            overflow.clone(),
            scope.reporter(),
        )
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(raw, incomplete);
        assert!(!overflow.load(Ordering::Relaxed));
        assert!(
            crate::loading_progress::active()
                .iter()
                .any(|snapshot| snapshot.label == "Tag progress test"
                    && snapshot.completed == 512
                    && snapshot.unit == crate::loading_progress::Unit::Items)
        );
    }

    #[test]
    fn publication_is_bounded_atomic_and_retains_conflicting_existing_data() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let cache = dir.path().join("cache");
        let folder = stage.join("a".repeat(64));
        std::fs::create_dir_all(&folder).unwrap();
        let doc = folder.join("catalog.json");
        let raw = b"verified metadata";
        std::fs::write(&doc, raw).unwrap();
        let sha = format!("{:x}", Sha256::digest(raw));
        assert_eq!(document(&stage, "catalog.json").unwrap(), doc);
        assert!(publish(&doc, &cache, &sha, 2).is_err());
        assert!(doc.is_file());
        assert!(!cache.exists());
        let publication = publish_with_status(&doc, &cache, &sha, 1024).unwrap();
        assert!(publication.moved);
        let published = publication.path;
        assert_eq!(std::fs::read(&published).unwrap(), raw);
        assert!(!folder.exists());
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&doc, raw).unwrap();
        let publication = publish_with_status(&doc, &cache, &sha, 1024).unwrap();
        assert_eq!(publication.path, published);
        assert!(!publication.moved);
        std::fs::write(&published, b"foreign data").unwrap();
        assert!(publish(&doc, &cache, &sha, 1024).is_err());
        assert_eq!(std::fs::read(&published).unwrap(), b"foreign data");
        let other = stage.join("b".repeat(64));
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("catalog.json"), raw).unwrap();
        assert!(document(&stage, "catalog.json").is_err());
    }
    #[test]
    fn runtime_prefers_adjacent_modules_and_explicit_interpreter_without_cwd_search() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        let local = dir.path().join("local");
        assert!(resolve(&app, &local, None).is_err());
        std::fs::create_dir_all(local.join("tools/unitypy/UnityPy")).unwrap();
        let (_, modules) = resolve(&app, &local, None).unwrap();
        assert_eq!(modules, local.join("tools/unitypy"));
        let exe = app.join(if cfg!(windows) {
            "tools/python/python.exe"
        } else {
            "tools/python/bin/python3"
        });
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, []).unwrap();
        std::fs::create_dir_all(app.join("tools/unitypy/UnityPy")).unwrap();
        assert_eq!(
            resolve(&app, &local, None).unwrap(),
            (exe, app.join("tools/unitypy"))
        );
        assert_eq!(
            resolve(&app, &local, Some(OsStr::new("explicit-python")))
                .unwrap()
                .0,
            PathBuf::from("explicit-python")
        );
    }
    #[test]
    fn embedded_entrypoints_are_closed_and_cancellation_never_starts_a_runtime() {
        let mut names = std::collections::BTreeSet::new();
        for (name, _) in SCRIPTS {
            assert!(names.insert(*name));
            assert!(!name.contains(['/', '\\']));
        }
        for script in [
            Script::Palette,
            Script::Catalog,
            Script::RestrictionCatalog,
            Script::Textures,
            Script::Monuments,
            Script::DecorModels,
            Script::Infrastructure,
            Script::RoadMeshes,
            Script::Rails,
            Script::RailMeshes,
            Script::Decoration,
            Script::UniformCliffs,
            Script::JungleVines,
            Script::CoastalCliffs,
            Script::WaterTreatment,
            Script::Powerlines,
            Script::SceneWires,
            Script::MonumentTags,
            Script::Spawn,
            Script::RoadObjects,
            Script::RiverObjects,
            Script::Tunnels,
        ] {
            assert!(names.contains(script.name()));
            assert!(!run(script, &[], || true).unwrap());
        }
    }
}
