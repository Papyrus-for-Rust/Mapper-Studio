"""Publish a receipt-bound monument program inventory from the selected Rust folder.

This captures filters/selectors/counts/spacing. It does not execute the full game
placement algorithm, instantiate monuments or promise playable map generation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
from extract_verified_prefabs import snapshot, fingerprint
from mapper_paths import data_root
from monument_stages import collect

LIMIT = 4 * 1024**2
CODE_PATHS = {'GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
              'RustClient_Data/Managed/Assembly-CSharp.dll',
              'RustDedicated_Data/Managed/Assembly-CSharp.dll'}


def bounded(path, limit=LIMIT):
    with path.open('rb') as stream:
        data = stream.read(limit+1)
    if len(data) > limit:
        raise ValueError('Monument program input exceeds its bound')
    return data


def capture_sources(root, profile):
    sources = snapshot(root)
    if (profile.get('format') not in ('papyrus-coastal-placement-profile', 'papyrus-monument-stage-profile')
            or profile.get('schema') != 1
            or profile.get('native_sha256') != sources[0]['sha256']
            or profile.get('scene_sha256') != sources[2]['sha256']):
        raise ValueError('No matching monument gameplay-code adapter')
    scene = profile['map_scene']
    if scene['relative_path'] != 'Bundles/maps/maps.bundle':
        raise ValueError('Unsupported procedural scene source')
    if fingerprint(root, scene['relative_path'], 512*1024**2) != scene:
        raise ValueError('Procedural map scene changed')
    sources.append(scene)
    code = profile['code_sources']
    if not 1 <= len(code) <= 2:
        raise ValueError('Invalid gameplay-code receipts')
    paths = {s['relative_path'] for s in sources}
    for item in code:
        name = item['relative_path']
        if name not in CODE_PATHS or name in paths:
            raise ValueError('Unsupported or duplicate gameplay-code path')
        if fingerprint(root, name, 1024**3) != item:
            raise ValueError('Monument gameplay code changed')
        paths.add(name)
        sources.append(item)
    return sources


def stage_layout(profile):
    if profile.get('format') == 'papyrus-coastal-placement-profile':
        if 'activation' in profile:
            raise ValueError('Legacy monument profile cannot override activation')
        return 'legacy'
    if (profile.get('format') == 'papyrus-monument-stage-profile'
            and profile.get('activation') == 'active-in-hierarchy-client-uncached'):
        return 'hierarchy'
    raise ValueError('Unsupported monument stage activation profile')


def script_hash():
    names = ('extract_monument_stages.py', 'monument_stages.py',
             'extract_verified_prefabs.py', 'prefab_geometry.py',
             'lazy_unity_bundle.py', 'mapper_paths.py')
    return hashlib.sha256(b''.join(name.encode()+b'\0'+Path(__file__).with_name(name).read_bytes()
                                   for name in names)).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--code-profile', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, default=data_root()/'papyrus-monument-stage-cache')
    args = parser.parse_args()
    catalog_raw = bounded(args.catalog, 16*1024**2)
    profile_raw = bounded(args.code_profile, 1024**2)
    catalog, profile = json.loads(catalog_raw), json.loads(profile_raw)
    layout = stage_layout(profile)
    sources = capture_sources(args.rust_root, profile)
    if catalog.get('format') != 'papyrus-prefab-catalog' or catalog.get('sources') != sources[:3]:
        raise ValueError('Monument catalog differs from selected installation')
    extractor = script_hash()
    stages, unsupported = collect(args.rust_root, stage_layout=layout)
    paths = sorted({p for group in ('modifiers', 'special') for p in catalog['metadata'][group]})
    if len(paths) > 512:
        raise ValueError('Monument catalog path allowance reached')
    matches = {p: [s['object_id'] for s in stages if any(p.startswith(f) for f in s['folders'])]
               for p in paths}
    if (sources != capture_sources(args.rust_root, profile) or extractor != script_hash()
            or bounded(args.catalog, 16*1024**2) != catalog_raw
            or bounded(args.code_profile, 1024**2) != profile_raw):
        raise ValueError('Source changed during monument program extraction')
    document = dict(format='papyrus-monument-stage-source', schema=1,
                    adapter='papyrus-monument-scene-programs-1',
                    catalog_sha256=hashlib.sha256(catalog_raw).hexdigest(),
                    sources=sources, extractor_sha256=extractor,
                    code_profile_sha256=hashlib.sha256(profile_raw).hexdigest(),
                    order_scope='relative hierarchy MonoBehaviour order; not an official seed ordinal',
                    stages=stages, unsupported_families=unsupported,
                    prefab_stage_matches=matches,
                    full_stage_execution_validated=False)
    if layout == 'hierarchy':
        document.update(schema=2, adapter='papyrus-monument-scene-programs-2')
    raw = json.dumps(document, indent=2, sort_keys=True, allow_nan=False).encode()
    if len(raw) > LIMIT:
        raise ValueError('Monument program source exceeds 4 MiB')
    args.cache_root.mkdir(parents=True, exist_ok=True)
    destination = args.cache_root/hashlib.sha256(raw).hexdigest()
    if destination.exists():
        if bounded(destination/'stages.json') != raw:
            raise ValueError('Cached monument program bytes changed')
        print(destination/'stages.json')
        return
    with tempfile.TemporaryDirectory(prefix='.monument-stages-', dir=args.cache_root) as temp:
        stage = Path(temp)
        if stage.resolve().parent != args.cache_root.resolve() or destination.resolve().parent != args.cache_root.resolve():
            raise ValueError('Monument stage output escaped cache root')
        with (stage/'stages.json').open('xb') as file:
            file.write(raw)
            file.flush()
            os.fsync(file.fileno())
        publish_directory(stage, destination)
    print(destination/'stages.json')


if __name__ == '__main__':
    main()
