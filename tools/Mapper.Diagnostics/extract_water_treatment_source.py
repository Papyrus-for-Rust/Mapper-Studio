"""Inspect installation-owned water-treatment roadside programs and prefabs.

This developer capture records serialized inputs. It does not enable generation
or certify the selected installation's native placement or hole-shape behavior.
"""
import argparse
import hashlib
import json
import math
import tempfile
from pathlib import Path

from mapper_paths import publish_directory
from monument_stages import filter_data, flag, integer
from road_object_stages import workflow_data
from road_stages import activation

IDENTITY = ('Assembly-CSharp', '', 'PlaceWaterTreatmentPipes')
FIELDS = {'Filter', 'ResourceFolder', 'AnchorMode', 'RoadType', 'Distance',
          'SideOffset', 'MaxSlope', 'Dithering', 'PlacementAttempts',
          'PreventBuildingPadding'}
LIMIT = 16 * 1024**2
SCRIPTS = (
    'extract_water_treatment_source.py', 'monument_stages.py', 'mapper_progress.py',
    'extract_uniform_cliffs.py', 'extract_decor_source.py', 'extract_spawn_source.py',
    'extract_verified_prefabs.py', 'lazy_unity_bundle.py', 'prefab_geometry.py',
    'decor_collider_meshes.py', 'gameplay_sources.py', 'road_stages.py',
    'monument_placement_attributes.py', 'mapper_paths.py', 'road_object_stages.py',
    'terrain_hole_shapes.py',
)


def producer():
    return hashlib.sha256(b''.join(
        name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
        for name in SCRIPTS
    )).hexdigest()


def stage_data(identity, fields, object_id, order, active, layout):
    if identity != IDENTITY:
        raise ValueError('Unsupported water-treatment stage identity')
    flags = activation(fields, FIELDS, layout)
    if not FIELDS.issubset(fields):
        raise ValueError('Missing water-treatment placement field')
    folder = fields['ResourceFolder']
    if (not isinstance(folder, str) or not 1 <= len(folder) <= 256
            or '\\' in folder or '\0' in folder or ':' in folder
            or any(part in ('', '.', '..') for part in folder.split('/'))):
        raise ValueError('Invalid water-treatment resource folder')
    name = fields.get('Description')
    if not isinstance(name, str) or len(name.encode('utf-8')) > 1024 or '\0' in name:
        raise ValueError('Invalid water-treatment description')
    values = {}
    for name, output, low, high in (
            ('Distance', 'distance', 0.001, 100000.),
            ('SideOffset', 'side_offset', -10000., 10000.),
            ('MaxSlope', 'maximum_slope', 0., 90.),
            ('Dithering', 'dithering', 0., 10000.),
            ('PreventBuildingPadding', 'prevent_building_padding', 0., 10000.)):
        value = fields[name]
        if type(value) not in (int, float) or not math.isfinite(value) or not low <= value <= high:
            raise ValueError('Invalid water-treatment ' + name)
        values[output] = value
    return dict(
        object_id=integer(object_id, 1, 2**63 - 1),
        order=integer(order, 0, 65535), name=fields['Description'],
        hierarchy_active=flag(active), **flags,
        folder=folder.lower(), filter=filter_data(fields['Filter']),
        anchor_mode=integer(fields['AnchorMode'], 0, 2),
        road_type=integer(fields['RoadType'], 0, 5),
        placement_attempts=integer(fields['PlacementAttempts'], 0, 1024),
        prefabs=[], **values,
    )


def stages(root, layout):
    if layout not in ('legacy', 'mode-description'):
        raise ValueError('Unsupported water-treatment stage layout')
    from extract_uniform_cliffs import identity, local_pointer
    from lazy_unity_bundle import load

    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={'BuildPlayer-Procedural Map'}, follow_dependencies=True)
    try:
        scenes = [asset for asset in env.assets if asset.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]
        setups = [obj for obj in scene.objects.values() if int(obj.type) == 114
                  and identity(obj) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous WorldSetup')
        result, workflow, seen = [], [], set()

        def walk(go, active=True, depth=0):
            if depth > 128 or go.path_id in seen or len(seen) >= 65536:
                raise ValueError('Invalid water-treatment procedural hierarchy')
            seen.add(go.path_id)
            fields = go.read_typetree()
            local_active = flag(fields['m_IsActive'])
            active = active and local_active
            objects = [local_pointer(scene, entry['component']) for entry in fields['m_Component']]
            for obj in objects:
                if int(obj.type) != 114:
                    continue
                ident = identity(obj)
                if ident[:2] != ('Assembly-CSharp', ''):
                    continue
                data = obj.read_typetree()
                header = {'Mode', 'Description'} | ({'Disabled'} if layout == 'legacy' else set())
                if header.issubset(data):
                    order = len(workflow)
                    workflow.append(workflow_data(ident, data, obj.path_id, order, active,
                                                  stage_layout=layout))
                    if len(workflow) > 512:
                        raise ValueError('Excessive procedural workflow')
                    if ident == IDENTITY:
                        result.append(stage_data(ident, data, obj.path_id, order, active, layout))
                elif ident == IDENTITY:
                    raise ValueError('Water-treatment stage lacks its procedural header')
            transforms = [obj for obj in objects if int(obj.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous procedural transform')
            for child in transforms[0].read_typetree()['m_Children']:
                transform = local_pointer(scene, child).read_typetree()
                walk(local_pointer(scene, transform['m_GameObject']), active, depth + 1)

        walk(local_pointer(scene, setups[0].read_typetree()['m_GameObject']))
        if len(result) > 16:
            raise ValueError('Excessive water-treatment stages')
        return result, dict(format='papyrus-procedural-order',
                            schema=1 if layout == 'legacy' else 2, stages=workflow)
    finally:
        stream.close()


def prefab_targets(manifest, programs):
    targets = {}
    for program in programs:
        prefix = 'assets/bundled/prefabs/autospawn/' + program['folder'] + '/'
        selected = set()
        for scene in manifest['Scenes']:
            for name in scene['IncludedAssets']:
                path = name.lower()
                if path.startswith(prefix) and path.endswith('.prefab'):
                    node = 'BuildPlayer-' + scene['Name']
                    if path in targets and targets[path] != node:
                        raise ValueError('Ambiguous water-treatment prefab scene')
                    targets[path] = node
                    selected.add(path)
        if not selected or len(selected) > 256:
            raise ValueError('Missing or excessive water-treatment prefab family')
        program['prefabs'] = sorted(selected)
    if len(targets) > 256:
        raise ValueError('Excessive water-treatment prefab count')
    return targets


def sources(root):
    from extract_verified_prefabs import fingerprint, snapshot

    result = snapshot(root) + [fingerprint(root, 'Bundles/maps/maps.bundle', 512 * 1024**2)]
    for name in ('GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
                 'RustClient_Data/Managed/Assembly-CSharp.dll',
                 'RustDedicated_Data/Managed/Assembly-CSharp.dll'):
        if (root / name).is_file():
            result.append(fingerprint(root, name, 1024**3))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--stage-layout', choices=('legacy', 'mode-description'), required=True)
    parser.add_argument('--output', type=Path, required=True, help='New capture directory')
    parser.add_argument('--collider-backfaces', action='store_true',
                        help='Record both triangle faces for inspection of collider contributors')
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Existing capture was retained')
    args.output.parent.mkdir(parents=True, exist_ok=True)

    from extract_uniform_cliffs import capture_prefab
    from lazy_unity_bundle import load
    from terrain_hole_shapes import HoleShapeSources, capture_holes

    before, extractor = sources(args.rust_root), producer()
    programs, workflow = stages(args.rust_root, args.stage_layout)
    manifest = json.loads((args.rust_root / 'Bundles/AssetSceneManifest.json').read_bytes())
    targets = prefab_targets(manifest, programs)
    prefabs, extra_sources = {}, []
    if targets:
        env, stream = load(args.rust_root / 'Bundles/shared/assetscenes.bundle',
                           nodes=set(targets.values()), follow_dependencies=True)
        meshes = HoleShapeSources(args.rust_root, env, args.collider_backfaces)
        try:
            for asset in env.assets:
                names = {name for name, node in targets.items() if node == asset.name}
                if not names:
                    continue
                for obj in asset.objects.values():
                    if int(obj.type) != 1:
                        continue
                    path = obj.read().m_Name.lower()
                    if path in names:
                        if path in prefabs:
                            raise ValueError('Duplicate water-treatment prefab root')
                        prefabs[path] = capture_prefab(asset, obj, meshes)
                        holes = capture_holes(asset, obj, meshes)
                        expected = prefabs[path]['components'].get('TerrainHoleShape', 0)
                        if len(holes) != expected:
                            raise ValueError('Incomplete water-treatment terrain-hole coverage')
                        prefabs[path]['hole_shapes'] = holes
                        if holes:
                            prefabs[path]['issues'] = [issue for issue in prefabs[path]['issues']
                                                      if issue != 'TerrainHoleShape.asset: unresolved field']
            if set(prefabs) != set(targets):
                raise ValueError('Incomplete water-treatment prefab coverage')
            meshes.verify()
            extra_sources = list(meshes.sources.values())
        finally:
            meshes.close()
            stream.close()
    if before != sources(args.rust_root) or extractor != producer():
        raise ValueError('Water-treatment sources changed during inspection')
    output = dict(format='mapper-water-treatment-reference', schema=2,
                  inspection_only=True, game_code_executed=False,
                  scene='BuildPlayer-Procedural Map', stage_layout=args.stage_layout,
                  order_basis='relative hierarchy procedural component order',
                  collider_backfaces=args.collider_backfaces, sources=before,
                  extra_sources=extra_sources, extractor_sha256=extractor,
                  stages=programs, procedural_order=workflow, prefabs=prefabs)
    raw = json.dumps(output, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(raw) > LIMIT:
        raise ValueError('Water-treatment capture exceeds 16 MiB')
    with tempfile.TemporaryDirectory(prefix='.water-treatment-', dir=args.output.parent) as temporary:
        stage = Path(temporary) / 'capture'
        stage.mkdir()
        (stage / 'source.json').write_bytes(raw)
        # Both resolved targets are inside this explicitly selected output parent.
        publish_directory(stage, args.output)
    print(json.dumps(dict(output=str(args.output), stages=len(programs), prefabs=len(prefabs))))


if __name__ == '__main__':
    main()
