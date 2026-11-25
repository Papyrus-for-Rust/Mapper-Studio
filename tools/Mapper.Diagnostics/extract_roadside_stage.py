"""Publish complete roadside program policy and ordered numeric sockets."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
from extract_monument_stages import bounded
from extract_verified_prefabs import snapshot, fingerprint
from roadside_stages import collect, ORDER, validate_road_owners
from monument_stages import integer

LIMIT = 4 * 1024**2
SCRIPTS = ('extract_roadside_stage.py', 'roadside_stages.py', 'road_stages.py',
           'extract_roadside_connectors.py', 'extract_monument_stages.py',
           'monument_stages.py', 'prefab_paths.py', 'prefab_geometry.py',
           'extract_verified_prefabs.py', 'lazy_unity_bundle.py', 'mapper_paths.py')


def script_hash():
    return hashlib.sha256(b''.join(n.encode()+b'\0'+Path(__file__).with_name(n).read_bytes()
                                   for n in SCRIPTS)).hexdigest()


def sources(root, profile):
    if (profile.get('format') != 'papyrus-roadside-monument-profile' or profile.get('schema') != 1
            or profile.get('proposal_engine') != 'papyrus-roadside-monument-proposals-1'):
        raise ValueError('Unknown roadside code profile')
    stage_layout(profile)
    captured = snapshot(root)
    expected = profile['sources']
    if not 5 <= len(expected) <= 6 or captured != expected[:3]:
        raise ValueError('Roadside profile belongs to another installation')
    allowed = {'Bundles/maps/maps.bundle', 'GameAssembly.dll',
               'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
               'RustClient_Data/Managed/Assembly-CSharp.dll',
               'RustDedicated_Data/Managed/Assembly-CSharp.dll'}
    names = {s['relative_path'] for s in captured}
    for receipt in expected[3:]:
        name = receipt['relative_path']
        if name not in allowed or name in names:
            raise ValueError('Unknown or duplicate roadside receipt path')
        actual = fingerprint(root, name, 1024**3)
        if actual != receipt:
            raise ValueError('Roadside gameplay or map source changed')
        captured.append(actual)
        names.add(name)
    if 'Bundles/maps/maps.bundle' not in names:
        raise ValueError('Missing procedural map receipt')
    for field in ('sample_spacing', 'end_padding'):
        value = profile[field]
        if type(value) not in (int, float) or not 0.1 <= value <= 100:
            raise ValueError('Invalid roadside code constant')
    for field in ('group_attempts', 'individual_candidates'):
        integer(profile[field], 1, 64)
    integer(profile['road_infrastructure_type'], 0, 64)
    integer(profile['trail_infrastructure_type'], 0, 64)
    return captured


def stage_layout(profile):
    layout = profile.get('stage_layout')
    if layout not in (None, 'mode-description'):
        raise ValueError('Unsupported roadside activation layout')
    return 'legacy' if layout is None else layout


def parameters(data):
    p = data.get('placement')
    if not isinstance(p, dict) or set(p) != {'priority', 'count'}:
        raise ValueError('Roadside prefab parameters need a default/shape adapter')
    return dict(priority=integer(p['priority'], 0, 4), count=integer(p['count'], 0, 4096))


def main():
    from extract_roadside_connectors import capture_connectors
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, required=True)
    args = parser.parse_args()
    raw = bounded(args.catalog, 16*1024**2)
    profile_raw = bounded(args.profile, 1024**2)
    catalog, profile = json.loads(raw), json.loads(profile_raw)
    receipts = sources(args.rust_root, profile)
    if catalog.get('format') != 'papyrus-prefab-catalog' or catalog.get('sources') != receipts[:3]:
        raise ValueError('Roadside catalog source mismatch')
    before = script_hash()
    layout = stage_layout(profile)
    stages, sequence = collect(args.rust_root, stage_layout=layout)
    prefabs = capture_connectors(args.rust_root, catalog, stages, include_owners=True, include_dungeon=True, include_root_water=True)
    for prefab in prefabs.values():
        validate_road_owners(prefab, profile['road_infrastructure_type'])
    metadata = {p: v for group in ('modifiers', 'special') for p, v in catalog['metadata'][group].items()}
    entries = {path: dict(parameters=parameters(metadata[path]), sockets=p) for path, p in prefabs.items()}
    if (receipts != sources(args.rust_root, profile) or before != script_hash()
            or raw != bounded(args.catalog, 16*1024**2)
            or profile_raw != bounded(args.profile, 1024**2)):
        raise ValueError('Source changed during roadside stage preparation')
    version = 1 if layout == 'legacy' else 2
    document = dict(format='papyrus-roadside-monument-source', schema=version,
                    adapter=f'papyrus-roadside-programs-{version}', catalog_sha256=hashlib.sha256(raw).hexdigest(),
                    sources=receipts, profile=profile, profile_sha256=hashlib.sha256(profile_raw).hexdigest(),
                    extractor_sha256=before, order_scope=ORDER, stages=stages,
                    road_sequence=sequence, prefabs=entries, full_stage_execution_validated=False)
    encoded = json.dumps(document, indent=2, allow_nan=False).encode()
    if len(encoded) > LIMIT:
        raise ValueError('Roadside stage document exceeds bound')
    digest = hashlib.sha256(encoded).hexdigest()
    args.cache_root.mkdir(parents=True, exist_ok=True)
    destination = args.cache_root/digest
    if destination.exists():
        existing = destination/'roadside.json'
        if existing.read_bytes() != encoded:
            raise ValueError('Roadside source cache bytes changed')
    else:
        with tempfile.TemporaryDirectory(prefix='.roadside-', dir=args.cache_root) as tmp:
            with (Path(tmp)/'roadside.json').open('xb') as file:
                file.write(encoded)
                file.flush()
                os.fsync(file.fileno())
            publish_directory(Path(tmp), destination)
    print('SourceOutput: '+str(destination/'roadside.json'))


if __name__ == '__main__':
    main()
