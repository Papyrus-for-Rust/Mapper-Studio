"""Inspect runtime terrain components without executing game code or physics.

This developer report records serialized prefab data. Component absence does not
establish runtime absence because game code can add components after spawning.
"""
import argparse
import json
import os
import tempfile
from pathlib import Path

from extract_uniform_cliffs import identity, local_pointer
from extract_verified_prefabs import snapshot
from monument_stages import flag
from prefab_geometry import COLLIDERS, IDENTITY, matrix, multiply

RUNTIME_TYPES = {
    'TerrainHoleShape', 'TerrainCollisionTrigger', 'TerrainAlphaScanner',
    'AddToAlphaMap', 'EnvironmentVolume', 'ApplyTerrainModifiers',
}
MAX_REPORT = 16 * 1024**2


def prefab_node(manifest, target):
    if (not isinstance(target, str) or len(target) > 512
            or not target.startswith('assets/') or not target.endswith('.prefab')
            or any(part in ('', '.', '..') for part in target.split('/'))
            or any(character in target for character in ('\\', '\0', ':'))):
        raise ValueError('Invalid prefab path')
    matches = [scene['Name'] for scene in manifest['Scenes']
               if target in {path.lower() for path in scene['IncludedAssets']}]
    if len(matches) != 1:
        raise ValueError('Prefab requires exactly one manifest scene')
    return 'BuildPlayer-' + matches[0]


def capture(asset, root, *, runtime_types=RUNTIME_TYPES):
    rows, seen = [], set()
    bytes_read = 0

    def walk(go, parent=IDENTITY, active=True, depth=0):
        nonlocal bytes_read
        if depth > 128 or go.path_id in seen or len(seen) >= 65536:
            raise ValueError('Invalid runtime prefab hierarchy')
        seen.add(go.path_id)
        data = go.read_typetree()
        local_active = flag(data['m_IsActive'])
        # Serialized prefab roots are inactive templates, activated on placement.
        active = active and (depth == 0 or local_active)
        if len(data['m_Component']) > 4096:
            raise ValueError('Runtime prefab object component budget exhausted')
        objects = [local_pointer(asset, entry['component']) for entry in data['m_Component']]
        transforms = [obj.read_typetree() for obj in objects if int(obj.type) == 4]
        if len(transforms) != 1:
            raise ValueError('Runtime prefab requires one transform per object')
        transform = transforms[0]
        local = parent if depth == 0 else multiply(parent, matrix(transform))
        for obj in objects:
            kind = int(obj.type)
            if kind != 114 and kind not in COLLIDERS:
                continue
            if len(rows) >= 8192 or obj.byte_size > 1024**2:
                raise ValueError('Runtime prefab component budget exhausted')
            bytes_read += obj.byte_size
            if bytes_read > 8 * 1024**2:
                raise ValueError('Runtime prefab total component budget exhausted')
            fields = obj.read_typetree()
            owner = identity(obj) if kind == 114 else None
            detail = kind in COLLIDERS or (
                owner is not None and owner[:2] == ('Assembly-CSharp', '')
                and owner[2] in runtime_types)
            rows.append(dict(
                game_object=go.path_id, name=data['m_Name'], object_id=obj.path_id,
                hierarchy_active_without_root=active, layer=data['m_Layer'],
                local_to_root=local, component=owner or COLLIDERS[kind],
                fields=fields if detail else None,
            ))
        for child in transform['m_Children']:
            child_transform = local_pointer(asset, child).read_typetree()
            walk(local_pointer(asset, child_transform['m_GameObject']), local, active, depth + 1)

    walk(root)
    return rows


def inspect(root, target):
    from lazy_unity_bundle import load

    # The requested scene and its serialized dependencies are read from
    # assetscenes.bundle. The building-restrictions sources belong to a
    # separate extractor.
    before = snapshot(root)
    manifest = json.loads((root / 'Bundles/AssetSceneManifest.json').read_bytes())
    node = prefab_node(manifest, target)
    env, stream = load(root / 'Bundles/shared/assetscenes.bundle', nodes={node}, follow_dependencies=True)
    try:
        assets = [asset for asset in env.assets if asset.name == node]
        if len(assets) != 1:
            raise ValueError('Missing or ambiguous serialized prefab scene')
        asset = assets[0]
        roots = [obj for obj in asset.objects.values() if int(obj.type) == 1
                 and obj.read_typetree()['m_Name'].lower() == target]
        if len(roots) != 1:
            raise ValueError('Missing or ambiguous prefab root')
        components = capture(asset, roots[0])
    finally:
        stream.close()
    if before != snapshot(root):
        raise ValueError('Rust sources changed during runtime prefab inspection')
    return dict(format='mapper-prefab-runtime-inspection', schema=1, prefab=target,
                node=node, sources=before, components=components,
                game_code_executed=False, physics_executed=False,
                runtime_compatibility_validated=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--prefab', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Existing inspection was retained')
    report = inspect(args.rust_root, args.prefab.lower())
    raw = json.dumps(report, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(raw) > MAX_REPORT:
        raise ValueError('Runtime prefab report budget exhausted')
    with tempfile.NamedTemporaryFile(dir=args.output.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        except BaseException:
            stream.close()
            temporary.unlink()
            raise
    try:
        os.link(temporary, args.output)
    finally:
        temporary.unlink()


if __name__ == '__main__':
    main()
