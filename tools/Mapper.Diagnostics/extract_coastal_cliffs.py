"""Capture installation-owned coastal cliff chains and their socket metadata."""
import argparse
import hashlib
import json
import math
import os
import tempfile
from pathlib import Path

from decor_collider_meshes import MeshSources
from extract_spawn_source import filter_data
from extract_verified_prefabs import fingerprint, snapshot
from extract_uniform_cliffs import capture_prefab, identity, local_pointer, SCRIPTS as UNIFORM_SCRIPTS
from gameplay_sources import verify_code
from lazy_unity_bundle import load
from mapper_paths import data_root, publish_directory
from road_stages import activation

LIMIT = 16 * 1024**2
SCRIPTS = ('extract_coastal_cliffs.py',) + UNIFORM_SCRIPTS
PARAMETERS = {
    'CutoffSlopeInitial': 'cutoff_slope_initial',
    'CutoffSlopeRepeat': 'cutoff_slope_repeat',
    'MinTerrainHeight': 'minimum_terrain_height',
    'MaxTerrainHeight': 'maximum_terrain_height',
    'MinCliffHeight': 'minimum_cliff_height',
    'MaxCliffHeight': 'maximum_cliff_height',
    'MinCliffScale': 'minimum_scale',
    'MaxCliffScale': 'maximum_scale',
}


def producer():
    return hashlib.sha256(b''.join(
        name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
        for name in SCRIPTS
    )).hexdigest()


def parameters(fields):
    result = {}
    for name, output in PARAMETERS.items():
        value = fields.get(name)
        scale = name.endswith('Scale')
        low, high = (0., 10000.) if scale else ((0, 90) if 'Slope' in name else (-500, 500))
        if ((type(value) not in (int, float) if scale else type(value) is not int)
                or not math.isfinite(value) or not low <= value <= high or scale and value == 0):
            raise ValueError('Invalid coastal cliff parameter ' + name)
        result[output] = value
    for low, high in [('minimum_terrain_height', 'maximum_terrain_height'),
                      ('minimum_cliff_height', 'maximum_cliff_height'),
                      ('minimum_scale', 'maximum_scale')]:
        if result[low] > result[high]:
            raise ValueError('Reversed coastal cliff parameter range')
    return result


def stage_data(identity, fields, object_id, order, active, layout):
    if identity != ('Assembly-CSharp', '', 'PlaceCliffs'):
        raise ValueError('Unknown coastal cliff stage')
    flags = activation(fields, set(PARAMETERS) | {
        'ResourceFolder', 'Filter', 'RetryMultiplier', 'TargetCount', 'TargetLength',
        'AnchorModeInitial', 'AnchorModeRepeat', 'FilterModeInitial', 'FilterModeRepeat',
    }, layout)
    folder = fields.get('ResourceFolder')
    if (not isinstance(folder, str) or not folder.lower().startswith('decor/')
            or len(folder) > 256 or '\\' in folder or '\0' in folder
            or any(p in ('', '.', '..') for p in folder.split('/'))):
        raise ValueError('Invalid coastal cliff folder')
    for name, low, high in [('RetryMultiplier', 0, 1000), ('TargetCount', 0, 1024),
                            ('TargetLength', 0, 100000)]:
        if type(fields.get(name)) is not int or not low <= fields[name] <= high:
            raise ValueError('Invalid coastal cliff ' + name)
    for name in ['AnchorModeInitial', 'AnchorModeRepeat']:
        if type(fields.get(name)) is not int or fields[name] not in range(3):
            raise ValueError('Invalid coastal cliff anchor mode')
    for name in ['FilterModeInitial', 'FilterModeRepeat']:
        if type(fields.get(name)) is not int or fields[name] not in (-1, *range(8)):
            raise ValueError('Invalid coastal cliff filter mode')
    return dict(
        stage=dict(id=str(object_id), order=order, name=fields['Description'],
                   kind='white_noise', folder=folder.lower(),
                   enabled=bool(active) and not flags['disabled'], mode=flags['mode'],
                   filter=filter_data(fields['Filter']), filter_mode=fields['FilterModeInitial'],
                   anchor_mode=fields['AnchorModeInitial'], distance=0., dithering=0.,
                   density=0., cluster=None, prefabs=[]),
        parameters=parameters(fields), retry_multiplier=fields['RetryMultiplier'],
        target_count=fields['TargetCount'], target_length=fields['TargetLength'],
        repeat_anchor_mode=fields['AnchorModeRepeat'], repeat_filter_mode=fields['FilterModeRepeat'],
    )


def placement_data(prefab):
    sockets = {}
    override = None
    for attribute in prefab['attributes']:
        kind = attribute['kind']
        if kind in ('DecorSocketMale', 'DecorSocketFemale'):
            if kind in sockets or attribute['fields']:
                raise ValueError('Ambiguous or unsupported coastal cliff socket')
            position = attribute['position']
            if (not isinstance(position, list) or len(position) != 3
                    or any(type(v) not in (int, float) or not math.isfinite(v)
                           or abs(v) > 10000 for v in position)):
                raise ValueError('Invalid coastal cliff socket position')
            sockets[kind] = position
        elif kind == 'PlaceCliffParameters':
            if override is not None or set(attribute['fields']) != set(PARAMETERS):
                raise ValueError('Ambiguous or unsupported coastal cliff override')
            override = parameters(attribute['fields'])
    return dict(male=sockets.get('DecorSocketMale'),
                female=sockets.get('DecorSocketFemale'), parameters=override)


def stages(root, layout):
    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={'BuildPlayer-Procedural Map'}, follow_dependencies=True)
    try:
        scenes = [a for a in env.assets if a.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact cliff procedural scene')
        scene = scenes[0]
        setups = [o for o in scene.objects.values() if int(o.type) == 114
                  and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous cliff WorldSetup')
        rows, visited = [], set()

        def walk(go, active=True, depth=0):
            if depth > 128 or go.path_id in visited or len(visited) >= 65536:
                raise ValueError('Invalid cliff procedural hierarchy')
            visited.add(go.path_id)
            fields = go.read_typetree()
            active = active and bool(fields['m_IsActive'])
            objects = [local_pointer(scene, c['component']) for c in fields['m_Component']]
            for obj in objects:
                if int(obj.type) == 114 and identity(obj)[2] == 'PlaceCliffs':
                    rows.append(stage_data(identity(obj), obj.read_typetree(), obj.path_id,
                                           len(rows), active, layout))
            transforms = [o.read_typetree() for o in objects if int(o.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous cliff procedural transform')
            for child in transforms[0]['m_Children']:
                transform = local_pointer(scene, child).read_typetree()
                walk(local_pointer(scene, transform['m_GameObject']), active, depth + 1)

        walk(local_pointer(scene, setups[0].read_typetree()['m_GameObject']))
        if not 1 <= len(rows) <= 16:
            raise ValueError('Unexpected coastal cliff stage count')
        return rows
    finally:
        stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--topology-profile', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, default=data_root() / 'papyrus-coastal-cliff-cache')
    args = parser.parse_args()
    raw = args.topology_profile.read_bytes()
    if len(raw) > 1024**2:
        raise ValueError('Cliff topology profile exceeds its bound')
    profile = json.loads(raw)
    profile_sha = profile.get('cliff_profile_sha256')
    if (not isinstance(profile_sha, str) or len(profile_sha) != 64
            or any(c not in '0123456789abcdef' for c in profile_sha)):
        raise ValueError('Missing cliff compatibility profile receipt')
    receipts = snapshot(args.rust_root) + [fingerprint(
        args.rust_root, 'Bundles/maps/maps.bundle', 512 * 1024**2)]
    if (profile.get('format') != 'papyrus-topology-profile'
            or profile.get('native_sha256') != receipts[0]['sha256']
            or profile.get('scene_sha256') != receipts[2]['sha256']):
        raise ValueError('Cliff profile differs from selected Rust installation')
    code = verify_code(args.rust_root, profile)
    if not code:
        raise ValueError('Coastal cliffs require bound gameplay sources')
    layout = profile.get('stage_layout', 'legacy')
    programs = stages(args.rust_root, layout)
    rows = [program["stage"] for program in programs]
    manifest = json.loads((args.rust_root / 'Bundles/AssetSceneManifest.json').read_bytes())
    targets = {}
    for stage in rows:
        prefix = 'assets/bundled/prefabs/autospawn/' + stage['folder'] + '/'
        for scene in manifest['Scenes']:
            for name in scene['IncludedAssets']:
                path = name.lower()
                if path.startswith(prefix) and path.endswith('.prefab'):
                    node = 'BuildPlayer-' + scene['Name']
                    if path in targets and targets[path] != node:
                        raise ValueError('Ambiguous cliff prefab scene')
                    targets[path] = node
                    stage['prefabs'].append(path)
        stage['prefabs'].sort()
        if not stage['prefabs']:
            raise ValueError('Missing coastal cliff prefab family')
    if not 1 <= len(targets) <= 256:
        raise ValueError('Unexpected coastal cliff prefab count')
    before = producer()
    env, stream = load(args.rust_root / 'Bundles/shared/assetscenes.bundle',
                       nodes=set(targets.values()), follow_dependencies=True)
    meshes = MeshSources(args.rust_root, env, profile['collider_sampling']['backfaces'])
    try:
        prefabs = {}
        for asset in env.assets:
            names = {p for p, node in targets.items() if asset.name == node}
            if not names:
                continue
            for obj in asset.objects.values():
                if int(obj.type) == 1:
                    name = obj.read().m_Name.lower()
                    if name in names:
                        if name in prefabs:
                            raise ValueError('Duplicate cliff prefab root')
                        prefabs[name] = capture_prefab(asset, obj, meshes)
                        placement_data(prefabs[name])
                        print(name, len(prefabs[name]['attributes']), flush=True)
        if set(prefabs) != set(targets):
            raise ValueError('Incomplete coastal cliff prefab coverage')
        flags = profile['flags']
        protected = 0
        for name in ('Monument', 'Building', 'Road', 'Roadside', 'Rail', 'Railside',
                     'Ocean', 'River', 'Lake'):
            protected |= flags[name]
        metadata = dict(
            format='papyrus-decor-source', schema=6 if layout == 'mode-description' else 5,
            adapter='papyrus-static-decor-collider-6' if layout == 'mode-description'
            else 'papyrus-static-decor-collider-5', scene='BuildPlayer-Procedural Map',
            sources=receipts, code_sources=code, extra_sources=list(meshes.sources.values()),
            extractor_sha256=before, stages=rows, prefabs=prefabs, issues=[],
            terrain_policy=dict(profile_id=profile['id'],
                                profile_sha256=hashlib.sha256(raw).hexdigest(),
                                native_sha256=receipts[0]['sha256'], scene_sha256=receipts[2]['sha256'],
                                topology_flags=flags, protected_topology=protected,
                                material_removal=profile['material_removal']),
        )
        if layout == 'mode-description':
            metadata['stage_layout'] = layout
        meshes.verify()
        if (receipts != snapshot(args.rust_root) + [fingerprint(
                args.rust_root, 'Bundles/maps/maps.bundle', 512 * 1024**2)]
                or code != verify_code(args.rust_root, profile) or producer() != before
                or args.topology_profile.read_bytes() != raw):
            raise ValueError('Cliff sources changed during extraction')
        encoded = json.dumps(dict(format='papyrus-coastal-cliff-source', schema=1,
                                  profile_sha256=profile_sha,
                                  programs=programs, metadata=metadata), separators=(',', ':'), allow_nan=False).encode()
        if len(encoded) > LIMIT:
            raise ValueError('Coastal cliff metadata exceeds 16 MiB')
        args.cache_root.mkdir(parents=True, exist_ok=True)
        destination = args.cache_root / hashlib.sha256(encoded).hexdigest()
        if destination.exists():
            raise ValueError('Coastal cliff snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.coastal-cliffs-', dir=args.cache_root) as temporary:
            stage = Path(temporary)
            with (stage / 'coastal.json').open('xb') as output:
                output.write(encoded)
                output.flush()
                os.fsync(output.fileno())
            publish_directory(stage, destination)
        print(destination / 'coastal.json', flush=True)
    finally:
        meshes.close()
        stream.close()


if __name__ == '__main__':
    main()
