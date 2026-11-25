"""Inspect source-bound monument phrases and PreventBuildingMonumentTag colliders."""
import argparse
import hashlib
import json
import tempfile
from pathlib import Path

from extract_water_treatment_source import SCRIPTS as PIPE_SCRIPTS, sources
from mapper_paths import publish_directory
from monument_tag_geometry import capture

SCRIPTS = ('extract_monument_tags.py', 'monument_tag_geometry.py', 'metadata_transforms.py') + PIPE_SCRIPTS
LIMIT = 32 * 1024**2


def producer():
    return hashlib.sha256(b''.join(name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
                                   for name in SCRIPTS)).hexdigest()


def read_json(path, limit):
    with path.open('rb') as stream:
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise ValueError('Monument tag input exceeds its bound')
    return raw, json.loads(raw)


def targets(catalog, manifest, requested):
    from extract_verified_prefabs import supported_catalog

    if not supported_catalog(catalog):
        raise ValueError('Unsupported monument tag catalog')
    entries = {path for group in ('modifiers', 'special') for path in catalog['metadata'][group]}
    names = set(requested) if requested else entries
    if not 1 <= len(names) <= 512:
        raise ValueError('Monument tag prefab roster exceeds its bound')
    if any(not isinstance(path, str) or path != path.lower() or '\\' in path or '\0' in path
           or not path.startswith('assets/') or not path.endswith('.prefab')
           or any(part in ('', '.', '..') for part in path.split('/')) for path in names):
        raise ValueError('Invalid monument tag prefab path')
    result = {}
    for scene in manifest['Scenes']:
        for path in scene['IncludedAssets']:
            path = path.lower()
            if path in names:
                node = 'BuildPlayer-' + scene['Name']
                if path in result and result[path] != node:
                    raise ValueError('Ambiguous monument tag prefab scene')
                result[path] = node
    if set(result) != names:
        raise ValueError('Incomplete monument tag prefab roster')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--prefab', action='append', default=[])
    parser.add_argument('--output', type=Path, required=True, help='New capture directory')
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Existing monument tag capture was retained')

    from extract_uniform_cliffs import local_pointer
    from extract_verified_prefabs import snapshot
    from lazy_unity_bundle import load
    from terrain_hole_shapes import HoleShapeSources
    from UnityPy.classes.PPtr import PPtr

    raw, catalog = read_json(args.catalog, 16 * 1024**2)
    before, extractor = sources(args.rust_root), producer()
    catalog_sources = snapshot(args.rust_root, building=len(catalog.get('sources', [])) > 3)
    if catalog.get('sources') != catalog_sources:
        raise ValueError('Monument tag catalog differs from selected installation')
    _, manifest = read_json(args.rust_root / 'Bundles/AssetSceneManifest.json', 32 * 1024**2)
    roster = targets(catalog, manifest, args.prefab)
    env, stream = load(args.rust_root / 'Bundles/shared/assetscenes.bundle',
                       nodes=set(roster.values()), follow_dependencies=True)
    prefabs = {}
    mesh_sources = HoleShapeSources(args.rust_root, env, False)
    mesh_cache = {}
    try:
        for asset in env.assets:
            names = {path for path, node in roster.items() if node == asset.name}
            if not names:
                continue
            scripts = {}
            script_types = {}

            def identify(selected, fields):
                ref = fields['m_Script']
                key = ref['m_FileID'], ref['m_PathID']
                if key not in scripts:
                    data = PPtr(**ref, assetsfile=selected).read_typetree()
                    scripts[key] = tuple(data[name] for name in ('m_AssemblyName', 'm_Namespace', 'm_ClassName'))
                return scripts[key]

            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                # Almost all GameObjects are unrelated children or scenery.
                # Keep the full read for candidate roots and older layouts.
                try:
                    name = obj.peek_name()
                except (AttributeError, ValueError, TypeError, IndexError):
                    name = None
                if isinstance(name, str) and name.lower() not in names:
                    continue
                path = obj.read().m_Name.lower()
                if path not in names:
                    continue
                if path in prefabs:
                    raise ValueError('Duplicate monument tag prefab root')
                prefabs[path] = capture(asset, obj, identify, local_pointer, mesh_sources, mesh_cache,
                                        script_types=script_types)
                print(json.dumps(dict(prefab=path, tags=len(prefabs[path]['tags']),
                                      complete=prefabs[path]['complete'])), flush=True)
        if set(prefabs) != set(roster):
            raise ValueError('Missing monument tag prefab roots')
        mesh_sources.verify()
    finally:
        mesh_sources.close()
        stream.close()
    if (before != sources(args.rust_root) or extractor != producer()
            or catalog_sources != snapshot(args.rust_root, building=len(catalog_sources) > 3)
            or raw != read_json(args.catalog, 16 * 1024**2)[0]):
        raise ValueError('Monument tag inputs changed during inspection')
    output = dict(format='mapper-monument-tag-reference', schema=1, inspection_only=True,
                  game_code_executed=False, sources=before, extractor_sha256=extractor,
                  extra_sources=list(mesh_sources.sources.values()),
                  catalog_sha256=hashlib.sha256(raw).hexdigest(), prefabs=prefabs)
    encoded = json.dumps(output, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(encoded) > LIMIT:
        raise ValueError('Monument tag capture exceeds 32 MiB')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.monument-tags-', dir=args.output.parent) as temporary:
        stage = Path(temporary) / 'capture'
        stage.mkdir()
        (stage / 'source.json').write_bytes(encoded)
        publish_directory(stage, args.output)
    print(json.dumps(dict(output=str(args.output), prefabs=len(prefabs),
                          tags=sum(len(data['tags']) for data in prefabs.values()),
                          incomplete=sum(not data['complete'] for data in prefabs.values()))))


if __name__ == '__main__':
    main()
