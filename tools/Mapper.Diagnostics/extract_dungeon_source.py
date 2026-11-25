"""Capture selected-installation dungeon stages and their authored prefab graph."""
import argparse
import hashlib
import json
from pathlib import Path

from dungeon_prefabs import inspect
from extract_monument_stages import bounded, capture_sources, stage_layout
from lazy_unity_bundle import load
from monument_stages import collect, folders, integer
from prefab_paths import component_readers

LIMIT = 16 * 1024**2
SCRIPTS = ('extract_dungeon_source.py', 'dungeon_prefabs.py', 'prefab_paths.py',
           'prefab_geometry.py', 'monument_stages.py', 'extract_monument_stages.py',
           'extract_verified_prefabs.py', 'lazy_unity_bundle.py', 'mapper_paths.py')


def script_hash():
    return hashlib.sha256(b''.join(name.encode() + b'\0' +
                                  Path(__file__).with_name(name).read_bytes()
                                  for name in SCRIPTS)).hexdigest()


def stage_data(program):
    fields = program['fields']
    base = {'Mode', 'Description'}
    if program['disabled'] is not None:
        base.update(('Disabled', 'HideInMapCreator', 'MapCreatorCategory'))
    names = ('EntranceFolder', 'LinkFolder', 'EndFolder', 'TransitionFolder')
    if set(fields) != base | set(names) | {'ConnectionType'}:
        raise ValueError('Unknown dungeon stage layout')
    result = {key: program.get(key) for key in (
        'object_id', 'order', 'mode', 'disabled', 'hierarchy_active', 'component_enabled')}
    result['connection_type'] = integer(fields['ConnectionType'], 0, 64)
    result['folders'] = {}
    for name in names:
        paths = folders(fields[name])
        if len(paths) != 1:
            raise ValueError('Dungeon stage requires one folder per role')
        result['folders'][name] = paths[0]
    return result


def targets(manifest, stages, monuments):
    """Use stage selectors and captured path-connector roles for the complete roster."""
    entries = {}
    for scene in manifest['Scenes']:
        for path in scene['IncludedAssets']:
            path = path.lower()
            if path in entries:
                raise ValueError('Duplicate manifest prefab path')
            entries[path] = scene['Name']
    if len(entries) > 262144:
        raise ValueError('Excessive manifest prefab roster')
    selected = {}
    for stage in stages:
        for role, prefix in stage['folders'].items():
            matches = [path for path in entries if path.startswith(prefix) and path.endswith('.prefab')]
            if not 1 <= len(matches) <= 512:
                raise ValueError('Missing or excessive dungeon folder roster')
            stage.setdefault('prefabs', {})[role] = sorted(matches)
            for path in matches:
                selected[path] = entries[path]
        roots = []
        for path, prefab in monuments['prefabs'].items():
            attributes = prefab.get('placement_attributes') or {}
            if any(c['infrastructure_type'] == stage['connection_type']
                   for c in attributes.get('path_connectors', [])):
                if path not in entries:
                    raise ValueError('Dungeon root is absent from the manifest')
                roots.append(path)
                selected[path] = entries[path]
        stage['roots'] = sorted(roots)
    if not 1 <= len(selected) <= 1024:
        raise ValueError('Invalid dungeon prefab roster')
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--monuments', type=Path, required=True)
    parser.add_argument('--code-profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise ValueError('Dungeon output must be new')
    catalog_raw, monument_raw = bounded(args.catalog, LIMIT), bounded(args.monuments, 32 * 1024**2)
    profile_raw = bounded(args.code_profile, 1024**2)
    catalog, monuments, profile = map(json.loads, (catalog_raw, monument_raw, profile_raw))
    sources = capture_sources(args.rust_root, profile)
    if (catalog.get('format') != 'papyrus-prefab-catalog' or catalog['sources'] != sources[:3]
            or monuments.get('format') != 'papyrus-monument-source'
            or monuments['sources'] != catalog['sources']
            or monuments['catalog_sha256'] != hashlib.sha256(catalog_raw).hexdigest()):
        raise ValueError('Dungeon inputs differ from the selected installation')
    layout = stage_layout(profile)
    extractor = script_hash()
    programs = []
    collect(args.rust_root, programs, inventory_only=True, capture_fields=True)
    stages = [stage_data(p) for p in programs if p['identity'][2] == 'GenerateDungeonBase']
    if not 1 <= len(stages) <= 16:
        raise ValueError('Invalid dungeon stage roster')
    manifest = json.loads(bounded(args.rust_root / 'Bundles/AssetSceneManifest.json', 32 * 1024**2))
    selected = targets(manifest, stages, monuments)
    wanted_scenes = {'BuildPlayer-' + name for name in selected.values()}
    environment, stream = load(args.rust_root / 'Bundles/shared/assetscenes.bundle',
                               nodes=wanted_scenes, follow_dependencies=True)
    prefabs = {}
    try:
        for asset in environment.assets:
            names = {path for path, scene in selected.items() if asset.name == 'BuildPlayer-' + scene}
            if not names:
                continue
            read, script_type = component_readers(asset.objects)
            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                path = obj.read().m_Name.lower()
                if path not in names:
                    continue
                if path in prefabs:
                    raise ValueError('Duplicate dungeon prefab root')
                prefabs[path] = inspect(obj.path_id, read, script_type)
        if set(prefabs) != set(selected):
            raise ValueError('Incomplete dungeon prefab capture')
        for stage in stages:
            if any(len(prefabs[path]['entrances']) != 1 or len(prefabs[path]['links']) != 1
                   for path in stage['roots']):
                raise ValueError('Dungeon root requires DungeonBaseInfo and one link')
            for role, paths in stage['prefabs'].items():
                if any(len(prefabs[path]['transitions' if role == 'TransitionFolder' else 'links']) != 1
                       for path in paths):
                    raise ValueError('Dungeon selector contains the wrong component family')
        if (capture_sources(args.rust_root, profile) != sources or script_hash() != extractor
                or bounded(args.catalog, LIMIT) != catalog_raw
                or bounded(args.monuments, 32 * 1024**2) != monument_raw
                or bounded(args.code_profile, 1024**2) != profile_raw):
            raise ValueError('Dungeon inputs changed during capture')
        document = dict(format='papyrus-dungeon-source', schema=1, activation=layout,
                        catalog_sha256=hashlib.sha256(catalog_raw).hexdigest(),
                        monument_sha256=hashlib.sha256(monument_raw).hexdigest(),
                        code_profile_sha256=hashlib.sha256(profile_raw).hexdigest(),
                        extractor_sha256=extractor, sources=sources,
                        stages=stages, prefabs=dict(sorted(prefabs.items())),
                        full_stage_execution_validated=False)
        encoded = json.dumps(document, separators=(',', ':'), allow_nan=False).encode()
        if len(encoded) > LIMIT:
            raise ValueError('Dungeon capture exceeds 16 MiB')
        with args.output.open('xb') as output:
            output.write(encoded)
        print(f'Captured {len(prefabs)} dungeon prefabs across {len(stages)} stages')
    finally:
        stream.close()


if __name__ == '__main__':
    main()
