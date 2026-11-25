"""Capture installation-owned uniform vine-tree placement and prefab metadata."""
import argparse
import hashlib
import json
import math
import os
import tempfile
from pathlib import Path

from UnityPy.classes.PPtr import PPtr
from decor_collider_meshes import MeshSources
from extract_uniform_cliffs import capture_prefab, SCRIPTS as CLIFF_SCRIPTS
from extract_spawn_source import filter_data
from extract_verified_prefabs import fingerprint, snapshot
from gameplay_sources import verify_code
from lazy_unity_bundle import load
from mapper_paths import data_root, publish_directory
from road_stages import activation

LIMIT = 16 * 1024**2
SCRIPTS = ('extract_jungle_vines.py', *CLIFF_SCRIPTS)


def producer():
    return hashlib.sha256(b''.join(
        name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
        for name in SCRIPTS
    )).hexdigest()


def stage_data(identity, fields, object_id, order, active, layout):
    if identity != ('Assembly-CSharp', '', 'PlaceJungleVines'):
        raise ValueError('Unknown jungle vine stage')
    flags = activation(fields, {
        'ResourceFolder', 'Filter', 'FilterMode', 'AnchorMode',
        'ObjectDistance', 'ObjectDithering',
    }, layout)
    if not isinstance(fields.get('ResourceFolder'), str):
        raise ValueError('Missing jungle vine resource folder')
    folder = fields['ResourceFolder'].lower()
    if (not folder.startswith('resource/') or len(folder) > 256
            or '\\' in folder or any(p in ('', '.', '..') for p in folder.split('/'))):
        raise ValueError('Invalid jungle vine folder')
    for name, low, high in (('ObjectDistance', 1., 1000.), ('ObjectDithering', 0., 1000.)):
        value = fields.get(name)
        if type(value) not in (float, int) or not math.isfinite(value) or not low <= value <= high:
            raise ValueError('Invalid jungle vine ' + name)
    if type(fields.get('AnchorMode')) is not int or fields['AnchorMode'] not in range(3):
        raise ValueError('Unsupported jungle vine anchor mode')
    if type(fields.get('FilterMode')) is not int or fields['FilterMode'] not in (-1, *range(8)):
        raise ValueError('Unsupported jungle vine filter mode')
    # WorldSetup's procedural gate uses the hierarchy, realm and Disabled field.
    return dict(
        id=str(object_id), order=order, name=fields['Description'], kind='uniform',
        folder=folder, enabled=bool(active) and not flags['disabled'], mode=flags['mode'],
        filter=filter_data(fields['Filter']), filter_mode=fields['FilterMode'],
        anchor_mode=fields['AnchorMode'], distance=fields['ObjectDistance'],
        dithering=fields['ObjectDithering'], density=0., cluster=None, prefabs=[],
    )


def identity(obj):
    fields = obj.read().m_Script.read_typetree()
    return fields['m_AssemblyName'], fields['m_Namespace'], fields['m_ClassName']


def local_pointer(asset, reference):
    if reference.get('m_FileID') != 0:
        raise ValueError('External cliff hierarchy reference')
    return PPtr(**reference, assetsfile=asset).deref()


def stages(root, layout):
    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={'BuildPlayer-Procedural Map'}, follow_dependencies=True)
    try:
        scenes = [a for a in env.assets if a.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact vine procedural scene')
        scene = scenes[0]
        setups = [o for o in scene.objects.values() if int(o.type) == 114
                  and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous cliff WorldSetup')
        rows, visited = [], set()

        def walk(go, active=True, depth=0):
            if depth > 128 or go.path_id in visited or len(visited) >= 65536:
                raise ValueError('Invalid vine procedural hierarchy')
            visited.add(go.path_id)
            fields = go.read_typetree()
            active = active and bool(fields['m_IsActive'])
            objects = [local_pointer(scene, c['component']) for c in fields['m_Component']]
            for obj in objects:
                if int(obj.type) == 114 and identity(obj)[2] == 'PlaceJungleVines':
                    rows.append(stage_data(identity(obj), obj.read_typetree(), obj.path_id,
                                           len(rows), active, layout))
            transforms = [o.read_typetree() for o in objects if int(o.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous vine procedural transform')
            for child in transforms[0]['m_Children']:
                transform = local_pointer(scene, child).read_typetree()
                walk(local_pointer(scene, transform['m_GameObject']), active, depth + 1)

        walk(local_pointer(scene, setups[0].read_typetree()['m_GameObject']))
        if not 1 <= len(rows) <= 16:
            raise ValueError('Unexpected jungle vine stage count')
        return rows
    finally:
        stream.close()



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--topology-profile', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, default=data_root() / 'papyrus-vine-cache')
    args = parser.parse_args()
    raw = args.topology_profile.read_bytes()
    if len(raw) > 1024**2:
        raise ValueError('Vine topology profile exceeds its bound')
    profile = json.loads(raw)
    profile_sha = profile.get('vine_profile_sha256')
    if (not isinstance(profile_sha, str) or len(profile_sha) != 64
            or any(c not in '0123456789abcdef' for c in profile_sha)):
        raise ValueError('Missing vine compatibility profile receipt')
    receipts = snapshot(args.rust_root) + [fingerprint(
        args.rust_root, 'Bundles/maps/maps.bundle', 512 * 1024**2)]
    if (profile.get('format') != 'papyrus-topology-profile'
            or profile.get('native_sha256') != receipts[0]['sha256']
            or profile.get('scene_sha256') != receipts[2]['sha256']):
        raise ValueError('Vine profile differs from selected Rust installation')
    code = verify_code(args.rust_root, profile)
    if not code:
        raise ValueError('Jungle vines require bound gameplay sources')
    layout = profile.get('stage_layout', 'legacy')
    rows = stages(args.rust_root, layout)
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
                        raise ValueError('Ambiguous vine prefab scene')
                    targets[path] = node
                    stage['prefabs'].append(path)
        stage['prefabs'].sort()
        if not stage['prefabs']:
            raise ValueError('Missing jungle vine prefab family')
    if not 1 <= len(targets) <= 256:
        raise ValueError('Unexpected jungle vine prefab count')
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
                            raise ValueError('Duplicate vine prefab root')
                        prefabs[name] = capture_prefab(asset, obj, meshes)
                        print(name, flush=True)
        if set(prefabs) != set(targets):
            raise ValueError('Incomplete jungle vine prefab coverage')
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
            raise ValueError('Vine sources changed during extraction')
        encoded = json.dumps(dict(format='papyrus-jungle-vine-source', schema=1,
                                  profile_sha256=profile_sha,
                                  metadata=metadata), separators=(',', ':'), allow_nan=False).encode()
        if len(encoded) > LIMIT:
            raise ValueError('Jungle vine metadata exceeds 16 MiB')
        args.cache_root.mkdir(parents=True, exist_ok=True)
        destination = args.cache_root / hashlib.sha256(encoded).hexdigest()
        if destination.exists():
            raise ValueError('Jungle vine snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.jungle-vines-', dir=args.cache_root) as temporary:
            stage = Path(temporary)
            with (stage / 'vines.json').open('xb') as output:
                output.write(encoded)
                output.flush()
                os.fsync(output.fileno())
            publish_directory(stage, destination)
        print(destination / 'vines.json', flush=True)
    finally:
        meshes.close()
        stream.close()


if __name__ == '__main__':
    main()
