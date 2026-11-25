"""Capture ordered roadside sockets. Does not accept or place monuments."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
from extract_monument_stages import bounded, capture_sources
from lazy_unity_bundle import load
from monument_stages import collect
from prefab_paths import inspect_ordered

LIMIT = 4 * 1024**2


def script_hash():
    names = ('extract_roadside_connectors.py', 'extract_monument_stages.py',
             'monument_stages.py', 'prefab_paths.py', 'prefab_geometry.py',
             'extract_verified_prefabs.py', 'lazy_unity_bundle.py', 'mapper_paths.py')
    return hashlib.sha256(b''.join(n.encode()+b'\0'+Path(__file__).with_name(n).read_bytes()
                                   for n in names)).hexdigest()


def capture_connectors(root, catalog, families, *, include_owners=False, include_dungeon=False, include_root_water=False):
    targets = {p: v for group in ('modifiers', 'special') for p, v in catalog['metadata'][group].items()
               if any(p.startswith(f) for f in families[0]['folders'])}
    if not 1 <= len(targets) <= 128:
        raise ValueError('Invalid roadside prefab roster')
    prefabs = {}
    selected = {f"BuildPlayer-{value['assetScene']}" for value in targets.values()
                if isinstance(value.get('assetScene'), str) and value['assetScene']}
    env, stream = load(root/'Bundles/shared/assetscenes.bundle',
                       nodes=selected or None, follow_dependencies=bool(selected))
    try:
        for asset in env.assets:
            names = {p for p, v in targets.items() if asset.name == 'BuildPlayer-'+v.get('assetScene', '')}
            if not names:
                continue

            def read(oid):
                obj = asset.objects[oid]
                return int(obj.type), obj.read_typetree()

            def identity(oid):
                fields = asset.objects[oid].read().m_Script.read_typetree()
                return fields['m_AssemblyName'], fields['m_Namespace'], fields['m_ClassName']

            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                name = obj.read().m_Name.lower()
                if name not in names:
                    continue
                if name in prefabs:
                    raise ValueError('Ambiguous roadside prefab root')
                dungeon = dict(links=[], entrances=[])
                water = []

                def capture_identity(oid):
                    ident = identity(oid)
                    if include_dungeon:
                        from roadside_stages import capture_dungeon
                        capture_dungeon(ident, oid, dungeon)
                    if include_root_water:
                        from roadside_stages import capture_root_water
                        capture_root_water(ident, read(oid)[1], oid, obj.path_id, water)
                    return ident

                result = inspect_ordered(obj.path_id, read, capture_identity)
                if include_dungeon:
                    result['dungeonComponents'] = dungeon
                if include_root_water:
                    if len(water) != 1:
                        raise ValueError('Missing exact root monument water policy')
                    result['waterMapEnabled'] = water[0]
                if include_owners:
                    from prefab_geometry import reference
                    for connector in result['connectors']:
                        connector['gameObjectId'] = reference(read(connector['objectId'])[1]['m_GameObject'])
                    result['version'] = 3
                if not result['complete']:
                    raise ValueError('Incomplete ordered roadside sockets: '+str(result['issues']))
                prefabs[name] = result
    finally:
        stream.close()
    if set(prefabs) != set(targets):
        raise ValueError('Missing roadside prefab roots')
    return prefabs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--code-profile', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, required=True)
    args = parser.parse_args()
    raw = bounded(args.catalog, 16*1024**2)
    profile_raw = bounded(args.code_profile, 1024**2)
    catalog, profile = json.loads(raw), json.loads(profile_raw)
    sources = capture_sources(args.rust_root, profile)
    if catalog.get('format') != 'papyrus-prefab-catalog' or catalog.get('sources') != sources[:3]:
        raise ValueError('Roadside catalog does not match selected installation')
    digest_before = script_hash()
    _, others = collect(args.rust_root)
    families = [s for s in others if s['kind'] == 'PlaceMonumentsRoadside']
    if len(families) != 1:
        raise ValueError('Unsupported roadside placement-family roster')
    prefabs = capture_connectors(args.rust_root, catalog, families)
    if (sources != capture_sources(args.rust_root, profile) or digest_before != script_hash()
            or raw != bounded(args.catalog, 16*1024**2)
            or profile_raw != bounded(args.code_profile, 1024**2)):
        raise ValueError('Source changed during roadside socket capture')
    document = dict(format='papyrus-roadside-connector-source', schema=1,
                    adapter='papyrus-ordered-roadside-sockets-1',
                    catalog_sha256=hashlib.sha256(raw).hexdigest(), sources=sources,
                    code_profile_sha256=hashlib.sha256(profile_raw).hexdigest(),
                    extractor_sha256=digest_before, family_selectors=families,
                    connector_order='root-first-depth-first-component-order', prefabs=prefabs,
                    placement_execution_validated=False)
    encoded = json.dumps(document, indent=2, allow_nan=False).encode()
    if len(encoded) > LIMIT:
        raise ValueError('Roadside socket snapshot exceeds bound')
    digest = hashlib.sha256(encoded).hexdigest()
    args.cache_root.mkdir(parents=True, exist_ok=True)
    destination = args.cache_root/digest
    if destination.exists():
        raise ValueError('Roadside snapshot exists; refusing overwrite')
    with tempfile.TemporaryDirectory(prefix='.roadside-', dir=args.cache_root) as tmp:
        path = Path(tmp)/'connectors.json'
        with path.open('xb') as file:
            file.write(encoded)
            file.flush()
            os.fsync(file.fileno())
        publish_directory(Path(tmp), destination)
    print('SourceOutput: '+str(destination/'connectors.json'))


if __name__ == '__main__':
    main()
