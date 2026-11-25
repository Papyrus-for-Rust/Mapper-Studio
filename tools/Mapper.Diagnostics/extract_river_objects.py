"""Capture source-bound river object rules using the shared path-object format."""
import argparse
import hashlib
import json
import tempfile
from pathlib import Path

from extract_decor_source import numeric
from extract_verified_prefabs import fingerprint, snapshot
from lazy_unity_bundle import load
from mapper_paths import publish_directory
from monument_placement_attributes import attribute_fields
from monument_stages import collect
from prefab_geometry import IDENTITY, matrix, multiply, point, reference, vector
from prefab_paths import component_readers
from road_object_stages import GROUPS, boolean, stage_data, stages, placement_policy_data
from road_stages import source_layout
from tunnel_prefabs import inspect

LIMIT = 16 * 1024**2


def bounded(path):
    with path.open('rb') as stream:
        raw = stream.read(LIMIT + 1)
    if len(raw) > LIMIT:
        raise ValueError('River object input exceeds 16 MiB')
    return raw


def river_stage(program, order, global_row, layout):
    if program['identity'] != ['Assembly-CSharp', '', 'PlaceRiverObjects']:
        raise ValueError('Incorrect river object program identity')
    # Both classes serialize the same four PathList rule groups. Reuse their
    # strict parameter decoder after checking the actual river class above.
    fields = dict(program['fields'], m_Enabled=global_row['component_enabled'])
    row = stage_data(('Assembly-CSharp', '', 'PlaceRoadObjects'), fields,
                     program['object_id'], order, program['hierarchy_active'], stage_layout=layout)
    row['component_enabled'] = global_row['component_enabled']
    row['scene_order'] = global_row['order']
    return row


def prefab(root, read, script_type, layout):
    seen, attributes, components = set(), [], {}
    base = {}

    def walk(owner_id, parent=IDENTITY, active=True, depth=0):
        if owner_id in seen or depth > 128 or len(seen) >= 65536:
            raise ValueError('Invalid river prefab hierarchy')
        seen.add(owner_id)
        kind, owner = read(owner_id)
        if kind != 1:
            raise ValueError('River prefab owner is not a GameObject')
        ids = [reference(row['component']) for row in owner['m_Component']]
        if len(ids) > 256 or len(set(ids)) != len(ids):
            raise ValueError('Invalid river prefab component roster')
        records = [(oid, *read(oid)) for oid in ids]
        transforms = [(oid, fields) for oid, kind, fields in records if kind == 4]
        if len(transforms) != 1:
            raise ValueError('Ambiguous river prefab transform')
        transform_id, transform = transforms[0]
        if reference(transform['m_GameObject']) != owner_id:
            raise ValueError('River prefab transform owner mismatch')
        if depth == 0 and transform['m_Father'] != {'m_FileID': 0, 'm_PathID': 0}:
            raise ValueError('Parented river prefab root')
        # Prefab.Spawn activates the root. Preserve authored activation below it.
        active = active and (depth == 0 or boolean(owner['m_IsActive']))
        world = multiply(parent, matrix(transform))
        is_root = depth == 0
        if is_root:
            base.update(rotation=vector(transform['m_LocalRotation'], 'xyzw'),
                        scale=vector(transform['m_LocalScale']))
        for object_id, kind, fields in records:
            if kind != 114:
                continue
            if reference(fields['m_GameObject']) != owner_id:
                raise ValueError('River prefab component owner mismatch')
            if not active:
                continue
            identity = script_type(object_id)
            if identity[:2] != ('Assembly-CSharp', ''):
                continue
            name = identity[2]
            components[name] = components.get(name, 0) + 1
            if (name.startswith(('Terrain', 'Decor', 'WaterCheck')) or name.lower().startswith('powerline')
                    or 'Sequence' in name or name in ('EnvironmentVolumeCheck', 'PrefabParameters',
                    'AddToHeightMap', 'SpawnableBoundsBlocker', 'RandomDestroy', 'DestroyIfInMonumentNoBuildZone')):
                parameters = attribute_fields(fields) if layout == 'mode-description' else fields
                values = {key: numeric(value) for key, value in parameters.items()
                          if not key.startswith('m_')}
                attributes.append(dict(kind=name, root=is_root, position=point(world, [0., 0., 0.]),
                                       local_to_root=[list(row) for row in world], fields=values,
                                       references=[]))
                if len(attributes) > 4096:
                    raise ValueError('Excessive river prefab attributes')
        for child in transform['m_Children']:
            child_id = reference(child)
            kind, fields = read(child_id)
            if kind != 4 or reference(fields['m_Father']) != transform_id:
                raise ValueError('Invalid river prefab child ownership')
            walk(reference(fields['m_GameObject']), world, active, depth + 1)

    walk(root)
    environment = inspect(root, read, script_type)['environment']
    return dict(**base, attributes=attributes, components=components, issues=[]), environment


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--infrastructure', type=Path, required=True)
    parser.add_argument('--placement-profile', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, required=True)
    args = parser.parse_args()
    parent_raw = bounded(args.infrastructure)
    parent = json.loads(parent_raw)
    profile_raw = bounded(args.placement_profile)
    profile = json.loads(profile_raw)
    layout = source_layout(parent)
    receipts = parent['sources'] + parent['profile']['code_sources'] + [parent['profile']['map_scene']]
    allowed = {'RustNative.dll', 'RustClient_Data/Plugins/x86_64/RustNative.dll',
               'RustDedicated_Data/Plugins/x86_64/RustNative.dll', 'Bundles/AssetSceneManifest.json',
               'Bundles/shared/assetscenes.bundle', 'Bundles/maps/maps.bundle', 'GameAssembly.dll',
               'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
               'RustClient_Data/Managed/Assembly-CSharp.dll', 'RustDedicated_Data/Managed/Assembly-CSharp.dll'}
    if (not 1 <= len(receipts) <= 8 or len({row['relative_path'] for row in receipts}) != len(receipts)
            or any(row['relative_path'] not in allowed for row in receipts)):
        raise ValueError('Invalid river object source receipts')
    if (profile.get('format') != 'papyrus-road-object-profile' or profile.get('schema') != 1
            or profile['native_sha256'] != parent['profile']['native_sha256']
            or profile['scene_sha256'] != parent['profile']['scene_sha256']
            or profile['code_sources'] != parent['profile']['code_sources']
            or profile.get('stage_layout') != parent['profile'].get('stage_layout')):
        raise ValueError('River path fitting profile differs from selected infrastructure')
    policy = placement_policy_data(profile['policy'])

    def verify():
        if snapshot(args.rust_root) != parent['sources'] or any(
                fingerprint(args.rust_root, row['relative_path'], 8 * 1024**3) != row for row in receipts):
            raise ValueError('River object installation changed')

    verify()
    scripts = ('extract_river_objects.py', 'road_object_stages.py', 'road_stages.py',
               'monument_stages.py', 'tunnel_prefabs.py', 'dungeon_prefabs.py', 'prefab_paths.py',
               'prefab_geometry.py', 'monument_placement_attributes.py', 'extract_decor_source.py',
               'extract_verified_prefabs.py', 'lazy_unity_bundle.py', 'mapper_paths.py')
    def script_hash():
        return hashlib.sha256(b''.join(name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
                                      for name in scripts)).hexdigest()
    extractor_sha = script_hash()
    _, workflow = stages(args.rust_root, include_workflow=True, stage_layout=layout)
    programs = []
    collect(args.rust_root, programs, inventory_only=True, capture_fields=True)
    programs = [program for program in programs if program['identity'][2] == 'PlaceRiverObjects']
    globals_by_id = {row['object_id']: row for row in workflow['stages']}
    if not 1 <= len(programs) <= 32:
        raise ValueError('Unsupported river object program count')
    rows = [river_stage(program, index, globals_by_id[program['object_id']], layout)
            for index, program in enumerate(programs)]
    manifest = json.loads(bounded(args.rust_root / 'Bundles/AssetSceneManifest.json'))
    assets = {}
    for scene in manifest['Scenes']:
        for path in scene['IncludedAssets']:
            path = path.lower()
            if path in assets:
                raise ValueError('Duplicate river asset manifest path')
            assets[path] = scene['Name']
    selected = {}
    for stage in rows:
        for group in GROUPS:
            for rule in stage[group.lower()]:
                prefix = 'assets/bundled/prefabs/autospawn/' + rule['folder'] + '/'
                rule['prefabs'] = sorted(path for path in assets if rule['folder']
                                         and path.startswith(prefix) and path.endswith('.prefab'))
                if rule['folder'] and not rule['prefabs']:
                    raise ValueError('Empty river object resource folder')
                selected.update({path: assets[path] for path in rule['prefabs']})
    if not 1 <= len(selected) <= 4096:
        raise ValueError('Unsupported river object prefab roster')
    nodes = {'BuildPlayer-' + name for name in selected.values()}
    environment, stream = load(args.rust_root / 'Bundles/shared/assetscenes.bundle',
                               nodes=nodes, follow_dependencies=True)
    prefabs, environments = {}, {}
    try:
        for asset in environment.assets:
            names = {path for path, name in selected.items() if asset.name == 'BuildPlayer-' + name}
            if not names:
                continue
            read, script_type = component_readers(asset.objects)
            for obj in asset.objects.values():
                if int(obj.type) != 1 or obj.read().m_Name.lower() not in names:
                    continue
                name = obj.read().m_Name.lower()
                if name in prefabs:
                    raise ValueError('Duplicate river object prefab root')
                prefabs[name], environments[name] = prefab(obj.path_id, read, script_type, layout)
    finally:
        stream.close()
    if set(prefabs) != set(selected):
        raise ValueError('Incomplete river object prefab capture')
    verify()
    if bounded(args.infrastructure) != parent_raw or bounded(args.placement_profile) != profile_raw or script_hash() != extractor_sha:
        raise ValueError('River object inputs changed during extraction')
    document = dict(format='papyrus-road-object-source', schema=4, adapter='papyrus-path-objects-1',
                    path_family='river', environments=environments, infrastructure_sha256=hashlib.sha256(parent_raw).hexdigest(),
                    sources=receipts, extra_sources=[], resources={}, extractor_sha256=extractor_sha,
                    stages=rows, workflow=workflow, prefabs=prefabs, issues=[], path_policy=policy,
                    placement_profile_sha256=hashlib.sha256(profile_raw).hexdigest())
    if layout == 'mode-description':
        document.update(schema=5, stage_layout=layout)
    raw = json.dumps(document, separators=(',', ':'), sort_keys=True, ensure_ascii=False).encode()
    if len(raw) > LIMIT:
        raise ValueError('River object document exceeds its bound')
    digest = hashlib.sha256(raw).hexdigest()
    args.cache_root.mkdir(parents=True, exist_ok=True)
    destination = args.cache_root / digest
    if destination.exists():
        if bounded(destination / 'objects.json') != raw:
            raise ValueError('Existing river object snapshot differs from its content identity')
        print(json.dumps(dict(sha256=digest, prefabs=len(prefabs), stages=len(rows), reused=True)))
        return
    with tempfile.TemporaryDirectory(prefix='.river-objects-', dir=args.cache_root) as temporary:
        stage = Path(temporary)
        (stage / 'objects.json').write_bytes(raw)
        publish_directory(stage, destination)
    print(json.dumps(dict(sha256=digest, prefabs=len(prefabs), stages=len(rows))))


if __name__ == '__main__':
    main()
