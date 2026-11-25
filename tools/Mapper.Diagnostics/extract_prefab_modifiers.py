"""Read actual terrain-modifier settings from Rust AssetScene records.

The output contains only numeric metadata and local texture-cache references. It never exports
prefab models, meshes, or texture pixels into the repository.
"""
from __future__ import annotations
from mapper_progress import report

import argparse
import json
import os
import pathlib
import sys

from prefab_geometry import inspect as inspect_water_geometry
from no_build_geometry import inspect as inspect_no_build, layer_index, Meshes

try:
    import UnityPy
except ImportError as error:
    raise SystemExit("UnityPy is required. Run extract_prefab_terrain.py first.") from error


def vector(value: dict) -> list[float]:
    return [float(value["x"]), float(value["y"]), float(value["z"])]


def rotation_range(fields: dict) -> dict | None:
    """Return the authored rotation constraint when this prefab exposes one.

    The generated IL2CPP type trees retain field names but not the managed component
    type names, so this intentionally keys off the two serialised fields.
    """
    if "MinRotation" not in fields or "MaxRotation" not in fields:
        return None
    return {"min": vector(fields["MinRotation"]), "max": vector(fields["MaxRotation"])}


def texture_reference(value: dict | None) -> int | None:
    """Preserve the local Unity object id used by a terrain-map component.

    This is only diagnostic metadata. It lets the cache extractor resolve each prefab's
    modifier textures without exporting the texture content to the project.
    """
    path_id = (value or {}).get("m_PathID")
    return int(path_id) if path_id else None


def texture_guid(value: dict | None) -> str | None:
    """Modern Rust terrain references are GUID wrappers, not Unity PPtrs."""
    guid = (value or {}).get("guid", "")
    if not guid:
        return None
    if not isinstance(guid, str) or len(guid) != 32 or any(c not in "0123456789abcdef" for c in guid):
        raise ValueError("Unsupported terrain texture GUID")
    return guid


from mapper_paths import data_root


def prefab_candidates(manifest: dict) -> set[str]:
    """The installation owns the roster, including families outside monument/.

    This is a discovery pool, not permission to spawn every asset. Components
    below distinguish terrain/monument roots from clutter and tunnel modules;
    the procedural scene still determines placement stages and quotas.
    """
    return {
        asset.lower() for scene in manifest["Scenes"] for asset in scene["IncludedAssets"]
        if asset.lower().startswith("assets/bundled/prefabs/autospawn/")
        and asset.lower().endswith(".prefab")
    }


def display_name(monument_info):
    phrase = (monument_info or {}).get("displayPhrase") or {}
    # Older Mono and current IL2CPP builds use different serialized field names.
    name = phrase.get("english") or phrase.get("legacyEnglish")
    if not isinstance(name, str):
        return None
    name = " ".join(name.split())
    return name if name and len(name.encode("utf-8")) <= 256 else None


def inspect_scene(serialized, scene_name, scene_prefabs, args, results, non_monuments, special):
    for obj in serialized.objects.values():
        if str(obj.type) != "1":  # GameObject
            continue
        # Most GameObjects are children or scenery. Read their names without
        # materializing components, and fall back for older layouts.
        try:
            name = obj.peek_name()
        except (AttributeError, ValueError, TypeError, IndexError):
            name = None
        if isinstance(name, str) and name.lower() not in scene_prefabs:
            continue
        game_object = obj.read()
        prefab_path = game_object.m_Name.lower()
        if prefab_path not in scene_prefabs:
            continue
        components = [serialized.objects[item.component.path_id] for item in game_object.m_Component if str(item.component.type) == "114"]
        modifier = None
        monument_info = None
        landmark_info = None
        placement = None
        rotation = None
        for component in components:
            fields = component.read_typetree()
            if {"size", "offset", "splatmap0", "splatmap1", "blendmap"}.issubset(fields):
                modifier = fields
            if "Bounds" in fields and "MinWorldSize" in fields:
                monument_info = fields
            if args.building_restrictions and "shouldDisplayOnMap" in fields and "displayPhrase" in fields:
                landmark_info = fields
            # PrefabParameters holds Rust's authored priority and desired count.
            # It is identified structurally because IL2CPP strips its type name.
            if "Priority" in fields and "Count" in fields:
                placement = fields
            if rotation_range(fields) is not None:
                rotation = fields
        # Autospawn also contains resources, clutter and dungeon modules.
        # Keep only authored monument or terrain-modifier roots in this
        # catalog; dungeon assembly remains a separate generation stage.
        surface_tunnel = (args.building_restrictions and prefab_path.startswith(
            "assets/bundled/prefabs/autospawn/tunnel-upwards/"))
        if not args.prefab and monument_info is None and modifier is None and not surface_tunnel:
            non_monuments.add(prefab_path)
            continue
        if args.debug_fields and modifier is not None:
            print(json.dumps({key: modifier[key] for key in modifier if "map" in key.lower() or "texture" in key.lower()}, default=str, indent=2))
        geometry_records = {}
        def read_geometry_object(path_id):
            if path_id in geometry_records:
                return geometry_records[path_id]
            if len(geometry_records) >= 262144:
                raise ValueError('Prefab object allowance exceeded')
            record = serialized.objects[path_id]
            geometry_records[path_id] = (int(record.type), record.read_typetree())
            return geometry_records[path_id]
        def geometry_class_id(path_id):
            return int(serialized.objects[path_id].type)
        restriction_metadata = {}
        if args.building_restrictions:
            if args.prevent_building_layer is None:
                restriction_metadata["noBuildIssue"] = "Installation layer table unavailable"
            else:
                restriction_metadata["noBuildGeometry"] = inspect_no_build(
                    obj.path_id, read_geometry_object, args.prevent_building_layer,
                    lambda fields, transform: args.restriction_meshes.decode(serialized, fields, transform),
                    class_id=geometry_class_id)
                landmark = monument_info or landmark_info
                visible = landmark.get("shouldDisplayOnMap") if landmark is not None else False
                if type(visible) in (bool, int) and visible in (0, 1):
                    restriction_metadata["noBuildGeometry"]["shouldDisplayOnMap"] = bool(visible)
        if surface_tunnel and monument_info is None and modifier is None:
            special[prefab_path] = {"reason": "Surface rail tunnel restriction volumes", **restriction_metadata}
            continue
        try:
            water_geometry = inspect_water_geometry(
                obj.path_id, read_geometry_object, class_id=geometry_class_id)
        except Exception as error:
            # Failed inspection must never look like a verified absence of local water.
            water_geometry = {"version": 1, "space": "prefab-root-local-unity", "nodesVisited": 0,
                              "waterBodies": [], "contributors": [], "issues": [{"objectId": obj.path_id, "reason": str(error)[:256]}], "complete": False}
        placement_metadata = {
            "displayName": display_name(monument_info),
            **restriction_metadata,
            "waterGeometry": water_geometry,
            "assetScene": scene_name,
            "monumentBounds": None if monument_info is None else {"center": vector(monument_info["Bounds"]["m_Center"]), "extent": vector(monument_info["Bounds"]["m_Extent"]), "minWorldSize": int(monument_info["MinWorldSize"])},
            "placement": None if placement is None else {"priority": int(placement["Priority"]), "count": int(placement["Count"])},
            "rotation": None if rotation is None else rotation_range(rotation),
        }
        if modifier is None:
            special[prefab_path] = {"reason": "No terrain modifier component", **placement_metadata}
            continue
        required = {"size", "offset", "HeightMap", "AlphaMap", "SplatMask", "BiomeMask",
                    "TopologyMask", "Radius", "Fade", "AutoCliffSplat", "AutoCliffTopology",
                    "RemoveExistingTopology"}
        missing_fields = sorted(required - modifier.keys())
        if missing_fields:
            # A newer/older component layout must remain visible without
            # inventing defaults or aborting discovery of every other family.
            special[prefab_path] = {"reason": "Unsupported terrain modifier layout",
                                    "missingModifierFields": missing_fields, **placement_metadata}
            continue
        results[prefab_path] = {
            **placement_metadata,
            "textureObjectIds": {
                "height": texture_reference(modifier.get("heightmap", {})),
                "alpha": texture_reference(modifier.get("alphamap", {})),
                "splat0": texture_reference(modifier.get("splatmap0", {})),
                "splat1": texture_reference(modifier.get("splatmap1", {})),
                "blend": texture_reference(modifier.get("blendmap", {})),
                "topology": texture_reference(modifier.get("topologymap", {})),
                "biome": texture_reference(modifier.get("biomemap", {})),
            },
            "textureGuids": {
                name: texture_guid(modifier.get(field, {}))
                for name, field in {"height": "heightmap", "alpha": "alphamap", "splat0": "splatmap0",
                                    "splat1": "splatmap1", "blend": "blendmap", "topology": "topologymap",
                                    "biome": "biomemap", "water": "watermap"}.items()
            },
            "size": vector(modifier["size"]),
            "offset": vector(modifier["offset"]),
            "heightMap": bool(modifier["HeightMap"]),
            "waterMap": None if "WaterMap" not in modifier else bool(modifier["WaterMap"]),
            "alphaMap": bool(modifier["AlphaMap"]),
            "splatMask": int(modifier["SplatMask"]),
            "biomeMask": int(modifier["BiomeMask"]),
            "topologyMask": int(modifier["TopologyMask"]),
            "radius": float(modifier["Radius"]),
            "fade": float(modifier["Fade"]),
            "autoCliffSplat": bool(modifier["AutoCliffSplat"]),
            "autoCliffTopology": bool(modifier["AutoCliffTopology"]),
            "removeExistingTopology": bool(modifier["RemoveExistingTopology"]),
        }
        print(prefab_path)


def inspect_scene_bundle(scene_name, scene_prefabs, args, results, non_monuments, special):
    from contextlib import closing
    from lazy_unity_bundle import load
    environment, stream = load(args.rust_root / "Bundles" / "shared" / "assetscenes.bundle",
                               nodes={f"BuildPlayer-{scene_name}"})
    with closing(stream):
        serialized = next(asset for asset in environment.assets if asset.name == f"BuildPlayer-{scene_name}")
        inspect_scene(serialized, scene_name, scene_prefabs, args, results, non_monuments, special)


def main() -> None:
    default_rust = pathlib.Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Steam" / "steamapps" / "common" / "Rust"
    default_output = data_root() / "prefab-terrain-cache" / "prefab-modifiers.json"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-root", type=pathlib.Path, default=default_rust)
    parser.add_argument("--output", type=pathlib.Path, default=default_output)
    parser.add_argument("--prefab", action="append", default=[])
    parser.add_argument("--debug-fields", action="store_true", help="Print terrain-modifier field names for the selected prefabs.")
    parser.add_argument("--building-restrictions", action="store_true", help="Include optional authored restriction collider geometry.")
    args = parser.parse_args()
    args.prevent_building_layer = None
    args.restriction_meshes = Meshes(args.rust_root) if args.building_restrictions else None
    if args.building_restrictions:
        managers = [args.rust_root / name / 'globalgamemanagers' for name in ('RustClient_Data', 'RustDedicated_Data')
                    if (args.rust_root / name / 'Plugins/x86_64/RustNative.dll').is_file()]
        if len(managers) != 1:
            raise ValueError('Select exactly one installation data directory')
        try:
            args.prevent_building_layer = layer_index(managers[0])
        except (ValueError, KeyError, FileNotFoundError):
            args.prevent_building_layer = None

    manifest = json.loads((args.rust_root / "Bundles" / "AssetSceneManifest.json").read_text(encoding="utf-8"))
    targets = {path.lower() for path in args.prefab} if args.prefab else prefab_candidates(manifest)
    scene_targets: dict[str, set[str]] = {}
    for scene in manifest["Scenes"]:
        for asset in scene["IncludedAssets"]:
            normalized = asset.lower()
            if normalized in targets:
                scene_targets.setdefault(scene["Name"], set()).add(normalized)
    missing = targets - set().union(*scene_targets.values())

    # Decode one scene at a time; UnityPy object graphs are cyclic, so release
    # them between scenes rather than retaining the full monument catalog.
    import gc
    results: dict[str, dict] = {}
    non_monuments: set[str] = set()
    special: dict[str, dict] = {path: {"reason": "No AssetScene assignment"} for path in missing}
    for scene_index, (scene_name, scene_prefabs) in enumerate(scene_targets.items()):
        report("Reading monument scenes", scene_name, scene_index, len(scene_targets), force=True)
        inspect_scene_bundle(scene_name, scene_prefabs, args, results, non_monuments, special)
        gc.collect()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for path in targets - set(results) - set(special) - non_monuments:
        special[path] = {"reason": "Prefab root was not found in its assigned AssetScene"}
    args.output.write_text(json.dumps({"format": 5, "modifiers": results, "special": special}, separators=(',', ':')), encoding="utf-8")
    if args.restriction_meshes is not None:args.restriction_meshes.close()
    print(args.output)


if __name__ == "__main__":
    main()
