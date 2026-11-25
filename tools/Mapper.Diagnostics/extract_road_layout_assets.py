"""Capture ordered road inputs from the chosen installation without changing old caches."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
from extract_verified_prefabs import snapshot, fingerprint
from lazy_unity_bundle import load
from road_layout_assets import inspect_layout
from prefab_paths import component_readers
from road_stages import source_layout
from mapper_progress import report as progress

LIMIT = 16 * 1024**2
CODE_PATHS = {'GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
              'RustClient_Data/Managed/Assembly-CSharp.dll', 'RustDedicated_Data/Managed/Assembly-CSharp.dll'}


def bounded(path):
    with path.open('rb') as stream:
        raw = stream.read(LIMIT+1)
    if len(raw) > LIMIT:
        raise ValueError('Road-layout input exceeds 16 MiB')
    return raw


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--infrastructure', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, required=True)
    args = parser.parse_args()
    progress('Verifying road-layout sources', 'Selected installation', force=True)
    catalog_raw = bounded(args.catalog)
    infrastructure_raw = bounded(args.infrastructure)
    catalog = json.loads(catalog_raw)
    infrastructure = json.loads(infrastructure_raw)
    source_layout(infrastructure)
    sources = snapshot(args.rust_root)
    if (catalog.get('format') != 'papyrus-prefab-catalog'
        or catalog.get('sources') != sources or infrastructure.get('sources') != sources
        or infrastructure.get('catalog_sha256') != hashlib.sha256(catalog_raw).hexdigest()):
        raise ValueError('Catalog/infrastructure/source receipts differ')
    profile = infrastructure['profile']
    code = profile.get('code_sources')
    if not isinstance(code, list) or not 1 <= len(code) <= 3 or len({s['relative_path'] for s in code}) != len(code):
        raise ValueError('Road-layout capture requires bounded gameplay receipts')
    for expected in code:
        if expected['relative_path'] not in CODE_PATHS or fingerprint(args.rust_root, expected['relative_path'], 1024**3) != expected:
            raise ValueError('Gameplay receipt differs from infrastructure')
    scene = fingerprint(args.rust_root, 'Bundles/maps/maps.bundle', 512*1024**2)
    if scene != profile.get('map_scene'):
        raise ValueError('Road map scene differs from infrastructure')
    script_names = ('extract_road_layout_assets.py', 'road_layout_assets.py', 'prefab_paths.py',
                    'prefab_geometry.py', 'lazy_unity_bundle.py', 'extract_verified_prefabs.py', 'mapper_progress.py', 'mapper_paths.py', 'road_stages.py')
    def script_hash():
        return hashlib.sha256(b''.join(name.encode()+b'\0'+Path(__file__).with_name(name).read_bytes() for name in script_names)).hexdigest()
    extractor = script_hash()
    targets = {p:v for group in ('modifiers', 'special') for p,v in catalog['metadata'][group].items()}
    if not 1 <= len(targets) <= 4096:
        raise ValueError('Invalid road-layout prefab roster')
    prefabs = {}
    selected = {f"BuildPlayer-{value['assetScene']}" for value in targets.values()
                if isinstance(value.get('assetScene'), str) and value['assetScene']}
    env, stream = load(args.rust_root/'Bundles/shared/assetscenes.bundle',
                       nodes=selected or None, follow_dependencies=bool(selected))
    progress('Reading ordered road connectors', 'Asset scenes', 0, len(targets), force=True)
    try:
        for asset in env.assets:
            names = {p for p,v in targets.items() if asset.name == 'BuildPlayer-'+v.get('assetScene','')}
            if not names:
                continue
            read, script_type = component_readers(asset.objects)
            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                name = obj.read().m_Name.lower()
                if name not in names:
                    continue
                if name in prefabs:
                    raise ValueError('Duplicate road-layout prefab root')
                report = inspect_layout(obj.path_id, read, script_type)
                prefabs[name] = report
                progress('Reading ordered road connectors', name, len(prefabs), len(targets))
    finally:
        stream.close()
    for name in targets.keys()-prefabs.keys():
        prefabs[name] = dict(version=1, space='prefab-root-local-unity', complete=False,
            connectorOrder='root-first-depth-first-component-order', connectors=[], monument=None,
            issues=[dict(objectId=0, reason='Missing prefab root')], nodesVisited=0)
    progress('Checking road-layout sources', 'Before cache publication', len(targets), len(targets), force=True)
    if (sources != snapshot(args.rust_root) or extractor != script_hash()
        or bounded(args.catalog) != catalog_raw or bounded(args.infrastructure) != infrastructure_raw
        or any(fingerprint(args.rust_root, s['relative_path'], 1024**3) != s for s in code)
        or fingerprint(args.rust_root, 'Bundles/maps/maps.bundle', 512*1024**2) != scene):
        raise ValueError('Road-layout sources changed during capture')
    document = dict(format='papyrus-road-layout-assets', schema=1,
        adapter='papyrus-ordered-monument-road-inputs-1',
        catalog_sha256=hashlib.sha256(catalog_raw).hexdigest(),
        infrastructure_sha256=hashlib.sha256(infrastructure_raw).hexdigest(),
        sources=sources+code+[scene], extractor_sha256=extractor, prefabs=prefabs)
    encoded = json.dumps(document, indent=2, allow_nan=False).encode('utf-8')
    if len(encoded) > LIMIT:
        raise ValueError('Road-layout document exceeds 16 MiB')
    digest = hashlib.sha256(encoded).hexdigest()
    args.cache_root.mkdir(parents=True, exist_ok=True)
    destination = args.cache_root/digest
    if destination.exists():
        raise ValueError('Road-layout snapshot already exists; refusing overwrite')
    with tempfile.TemporaryDirectory(prefix='.road-layout-', dir=args.cache_root) as temporary:
        stage = Path(temporary)
        with (stage/'layout-assets.json').open('xb') as output:
            output.write(encoded); output.flush(); os.fsync(output.fileno())
        publish_directory(stage, destination)
    print(destination/'layout-assets.json', flush=True)
    progress('Ordered road connectors ready', 'Snapshot prepared', len(targets), len(targets), force=True)


if __name__ == '__main__':
    main()
