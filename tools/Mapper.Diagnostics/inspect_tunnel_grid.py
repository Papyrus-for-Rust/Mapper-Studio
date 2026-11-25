"""Inspect installed dungeon-grid selectors and their ordering beside surface rails."""
import argparse
import hashlib
import json
from pathlib import Path

from extract_monument_stages import bounded, capture_sources
from monument_stages import collect, folders
from tunnel_prefabs import capture, validate_roles
from extract_verified_prefabs import fingerprint
from tunnel_identities import capture as capture_identities
from mapper_progress import report


def script_hash():
    return hashlib.sha256(b''.join(
        name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
        for name in ('inspect_tunnel_grid.py', 'tunnel_prefabs.py', 'dungeon_prefabs.py',
                     'prefab_paths.py', 'prefab_geometry.py', 'extract_monument_stages.py',
                     'extract_verified_prefabs.py', 'monument_stages.py',
                     'lazy_unity_bundle.py', 'mapper_paths.py', 'tunnel_identities.py',
                     'extract_prefab_identities.py', 'resolve_prefab_textures.py',
                     'texture_payload_cache.py', 'mapper_progress.py',
                     'monument_placement_attributes.py'))).hexdigest()


def targets(assets, programs, monuments):
    grids = [program for program in programs if program['identity'][2] == 'GenerateDungeonGrid']
    paths = {path for program in grids for role in program['selectors'].values()
             for path in role['prefabs']}
    connection_types = {program['fields']['ConnectionType'] for program in grids}
    for path, prefab in monuments['prefabs'].items():
        if any(connector['infrastructure_type'] in connection_types
               for connector in prefab.get('placement_attributes', {}).get('path_connectors', [])):
            paths.add(path)
    for program in programs:
        if program['identity'][2].startswith('PlaceMonuments'):
            prefixes = folders(program['fields']['ResourceFolder'])
            paths.update(path for path in assets if path.endswith('.prefab')
                         and any(path.startswith(prefix) for prefix in prefixes))
    if not 1 <= len(paths) <= 1024 or not paths.issubset(assets):
        raise ValueError('Invalid or unresolved tunnel capture roster')
    return {path: assets[path] for path in sorted(paths)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--code-profile', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--capture-prefabs', action='store_true')
    parser.add_argument('--monuments', type=Path)
    args = parser.parse_args()
    if args.capture_prefabs and args.monuments is None:
        parser.error('--capture-prefabs requires the selected installation --monuments capture')
    if args.output.exists():
        raise ValueError('Tunnel inspection output must be new')
    profile_raw = bounded(args.code_profile, 1024**2)
    profile = json.loads(profile_raw)
    extractor = script_hash()
    sources = capture_sources(args.rust_root, profile)
    report('Preparing underground rail modules', 'Reading tunnel programs', 0, 3, force=True)
    programs = []
    collect(args.rust_root, programs, inventory_only=True, capture_fields=True)
    manifest = json.loads(bounded(args.rust_root/'Bundles/AssetSceneManifest.json', 32 * 1024**2))
    assets = {}
    for scene in manifest['Scenes']:
        for path in scene['IncludedAssets']:
            path = path.lower()
            if path in assets:
                raise ValueError('Duplicate tunnel manifest path')
            assets[path] = scene['Name']
    selected = []
    for program in programs:
        kind = program['identity'][2]
        fields = program.get('fields') or {}
        if kind == 'GenerateDungeonGrid':
            program['selectors'] = {}
            for role in ('TunnelFolder', 'StationFolder', 'UpwardsFolder', 'TransitionFolder', 'LinkFolder'):
                prefixes = folders(fields[role])
                paths = sorted(path for path in assets if path.endswith('.prefab') and any(path.startswith(p) for p in prefixes))
                if len(paths) > 1024:
                    raise ValueError('Excessive tunnel selector roster')
                program['selectors'][role] = dict(prefixes=prefixes, prefabs=paths)
            selected.append(program)
        elif kind.startswith('GenerateRail') or (
                kind.startswith('PlaceMonuments') and 'tunnel-entrance' in str(fields.get('ResourceFolder', ''))):
            selected.append(program)
    if not any(program['identity'][2] == 'GenerateDungeonGrid' for program in selected):
        raise ValueError('No dungeon-grid stage found')
    prefabs = None
    identities = None
    content_receipt = None
    monument_raw = None
    if args.capture_prefabs:
        monument_raw = bounded(args.monuments, 32 * 1024**2)
        monuments = json.loads(monument_raw)
        if (monuments.get('format') != 'papyrus-monument-source'
                or monuments['sources'] != sources[:3]):
            raise ValueError('Tunnel entrances differ from the selected installation')
        report('Preparing underground rail modules', 'Reading authored cells and sockets', 1, 3, force=True)
        prefabs = capture(args.rust_root, targets(assets, selected, monuments))
        for program in selected:
            if program['identity'][2] == 'GenerateDungeonGrid':
                validate_roles(program, prefabs)
        content_receipt = fingerprint(args.rust_root, 'Bundles/shared/content.bundle', 8 * 1024**3)
        report('Preparing underground rail modules', 'Resolving installed prefab identities', 2, 3, force=True)
        identities = capture_identities(args.rust_root, prefabs)
    if (capture_sources(args.rust_root, profile) != sources
            or bounded(args.code_profile, 1024**2) != profile_raw
            or monument_raw is not None and bounded(args.monuments, 32 * 1024**2) != monument_raw
            or content_receipt is not None and fingerprint(args.rust_root, content_receipt['relative_path'], 8 * 1024**3) != content_receipt
            or script_hash() != extractor):
        raise ValueError('Installation changed during tunnel inspection')
    result = dict(format='mapper-tunnel-grid-source-inspection', schema=1,
                  sources=sources, code_profile_sha256=hashlib.sha256(profile_raw).hexdigest(),
                  programs=selected, game_code_executed=False, full_network_generated=False)
    if prefabs is not None:
        result['prefabs'] = prefabs
        result['extractor_sha256'] = extractor
        result['monument_sha256'] = hashlib.sha256(monument_raw).hexdigest()
        result['identities'] = identities
        result['extra_sources'] = [content_receipt]
    encoded = json.dumps(result, separators=(',', ':'), allow_nan=False).encode()
    if len(encoded) > 64 * 1024**2:
        raise ValueError('Tunnel capture exceeds 64 MiB')
    with args.output.open('xb') as output:
        output.write(encoded)
    report('Preparing underground rail modules', 'Tunnel metadata ready', 3, 3, force=True)
    print(json.dumps([dict(kind=p['identity'][2], order=p['order'], object_id=p['object_id'],
                           selectors={role:len(data['prefabs']) for role,data in p.get('selectors', {}).items()}
                           if p['identity'][2]=='GenerateDungeonGrid' else {})
                      for p in selected]))


if __name__ == '__main__':
    main()
