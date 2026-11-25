"""Capture installation-owned utility stages, attachment rules and prefab inputs."""
import argparse
from contextlib import ExitStack
import hashlib
import json
from pathlib import Path
import tempfile

from extract_water_treatment_source import sources
from inspect_prefab_runtime import capture, RUNTIME_TYPES
from mapper_paths import publish_directory
from monument_stages import flag, integer
from road_object_stages import GROUPS, guid_reference, rule_data, workflow_data
from road_stages import activation

KINDS = {'GeneratePowerlineLayout', 'GeneratePowerlineTopology',
         'PlacePowerlineObjects', 'GenerateWireMeshes'}
WIRE_TYPES = RUNTIME_TYPES | {'PowerlineNode', 'PathSequencePowerline',
                             'PowerLineWireConnectionHelper', 'PowerLineWire',
                             'PowergridPowerline', 'PowerlinePowergridAccessPointSpawn'}
LIMIT = 32 * 1024**2
SCRIPTS = (
    'extract_powerline_source.py', 'extract_water_treatment_source.py',
    'inspect_prefab_runtime.py', 'extract_uniform_cliffs.py', 'extract_decor_source.py',
    'extract_spawn_source.py', 'extract_verified_prefabs.py', 'lazy_unity_bundle.py',
    'prefab_geometry.py', 'decor_collider_meshes.py', 'gameplay_sources.py',
    'road_stages.py', 'monument_placement_attributes.py', 'mapper_paths.py',
    'road_object_stages.py', 'monument_stages.py', 'mapper_progress.py',
    'terrain_hole_shapes.py',
    'resolve_prefab_textures.py', 'texture_payload_cache.py',
)


def producer():
    return hashlib.sha256(b''.join(name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
                                   for name in SCRIPTS)).hexdigest()


def stage_data(identity, fields, object_id, order, active, layout):
    if identity[:2] != ('Assembly-CSharp', '') or identity[2] not in KINDS:
        raise ValueError('Unsupported powerline stage identity')
    extras = set(GROUPS) if identity[2] == 'PlacePowerlineObjects' else set()
    flags = activation(fields, extras, layout)
    if not isinstance(fields.get('Description'), str) or len(fields['Description']) > 1024:
        raise ValueError('Invalid powerline stage description')
    if '\0' in fields['Description']:
        raise ValueError('Invalid powerline stage description')
    row = dict(object_id=integer(object_id, 1, 2**63 - 1), order=integer(order, 0, 511),
               kind=identity[2], description=fields['Description'],
               hierarchy_active=flag(active), **flags)
    if extras:
        for group in GROUPS:
            rules = fields.get(group)
            if not isinstance(rules, list) or len(rules) > 128:
                raise ValueError('Invalid powerline attachment rule roster')
            row[group.lower()] = [rule_data(group, rule) for rule in rules]
    return row


def stages(root, layout):
    from extract_uniform_cliffs import identity, local_pointer
    from lazy_unity_bundle import load

    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={'BuildPlayer-Procedural Map'}, follow_dependencies=True)
    try:
        scenes = [asset for asset in env.assets if asset.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]
        setups = [obj for obj in scene.objects.values() if int(obj.type) == 114
                  and identity(obj) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous WorldSetup')
        result, workflow, seen = [], [], set()

        def walk(go, active=True, depth=0):
            if depth > 128 or go.path_id in seen or len(seen) >= 65536:
                raise ValueError('Invalid powerline procedural hierarchy')
            seen.add(go.path_id)
            fields = go.read_typetree()
            active = active and flag(fields['m_IsActive'])
            objects = [local_pointer(scene, entry['component']) for entry in fields['m_Component']]
            for obj in objects:
                if int(obj.type) != 114:
                    continue
                owner = identity(obj)
                if owner[:2] != ('Assembly-CSharp', ''):
                    continue
                data = obj.read_typetree()
                header = {'Mode', 'Description'} | ({'Disabled'} if layout == 'legacy' else set())
                if header.issubset(data):
                    order = len(workflow)
                    if order >= 512:
                        raise ValueError('Excessive procedural workflow')
                    workflow.append(workflow_data(owner, data, obj.path_id, order, active, stage_layout=layout))
                    if owner[2] in KINDS:
                        result.append(stage_data(owner, data, obj.path_id, order, active, layout))
                elif owner[2] in KINDS:
                    raise ValueError('Powerline stage lacks its procedural header')
            transforms = [obj.read_typetree() for obj in objects if int(obj.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous powerline procedural transform')
            for child in transforms[0]['m_Children']:
                transform = local_pointer(scene, child).read_typetree()
                walk(local_pointer(scene, transform['m_GameObject']), active, depth + 1)

        walk(local_pointer(scene, setups[0].read_typetree()['m_GameObject']))
        if {row['kind'] for row in result} != KINDS or len(result) > 32:
            raise ValueError('Incomplete powerline procedural roster')
        return result, dict(format='papyrus-procedural-order',
                            schema=1 if layout == 'legacy' else 2, stages=workflow)
    finally:
        stream.close()


def targets(manifest, programs):
    result = {}
    for program in programs:
        if program['kind'] != 'PlacePowerlineObjects':
            continue
        for group in GROUPS:
            for rule in program[group.lower()]:
                if not rule['folder']:
                    continue
                prefix = 'assets/bundled/prefabs/autospawn/' + rule['folder'] + '/'
                selected = set()
                for scene in manifest['Scenes']:
                    for name in scene['IncludedAssets']:
                        path = name.lower()
                        if path.startswith(prefix) and path.endswith('.prefab'):
                            node = 'BuildPlayer-' + scene['Name']
                            if path in result and result[path] != node:
                                raise ValueError('Ambiguous powerline prefab scene')
                            result[path] = node
                            selected.add(path)
                if not selected or len(selected) > 256:
                    raise ValueError('Missing or excessive powerline prefab family')
                rule['prefabs'] = sorted(selected)
    if len(result) > 1024:
        raise ValueError('Excessive powerline prefab roster')
    return result


def wire_guids(prefabs):
    required = set()
    for prefab in prefabs.values():
        for component in prefab['runtime']:
            if component['component'] != ('Assembly-CSharp', '', 'PowerlineNode'):
                continue
            guid = guid_reference(component['fields'].get('WirePrefab'))
            if guid is None:
                raise ValueError('Invalid powerline wire prefab reference')
            required.add(guid)
    return required


def resolve_wires(root, manifest, required, identity_fields=None):
    if not required:
        return {}, {}, []
    from extract_verified_prefabs import fingerprint
    from lazy_unity_bundle import load
    from resolve_prefab_textures import container_index, guid_paths, unique_asset
    from inspect_prefab_runtime import prefab_node

    receipt = fingerprint(root, 'Bundles/shared/content.bundle', 8 * 1024**3)
    resources = {}
    if identity_fields is not None:
        try:
            with identity_fields.open('rb') as stream:
                raw = stream.read(16 * 1024**2 + 1)
            if len(raw) <= 16 * 1024**2:
                resources = guid_paths(json.loads(raw), required)
        except (OSError, ValueError, TypeError, AttributeError):
            pass
    if set(resources) != required:
        content, stream = load(root / 'Bundles/shared/content.bundle')
        try:
            game_manifest = unique_asset(container_index(content), 'assets/manifest.asset')
            if game_manifest is None:
                raise ValueError('Missing GameManifest for wire references')
            resources = guid_paths(game_manifest.read_typetree(), required)
            if set(resources) != required:
                raise ValueError('Incomplete powerline wire GUID resolution')
        finally:
            stream.close()
    selected = {path: prefab_node(manifest, path) for path in resources.values()}
    env, stream = load(root / 'Bundles/shared/assetscenes.bundle',
                       nodes=set(selected.values()), follow_dependencies=True)
    prefabs = {}
    try:
        for asset in env.assets:
            paths = {path for path, node in selected.items() if node == asset.name}
            if not paths:
                continue
            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                path = obj.read_typetree()['m_Name'].lower()
                if path in paths:
                    if path in prefabs:
                        raise ValueError('Duplicate wire prefab root')
                    prefabs[path] = capture(asset, obj, runtime_types=WIRE_TYPES)
        if set(prefabs) != set(selected):
            raise ValueError('Incomplete powerline wire prefab capture')
    finally:
        stream.close()
    if receipt != fingerprint(root, 'Bundles/shared/content.bundle', 8 * 1024**3):
        raise ValueError('Wire resource source changed during capture')
    return resources, prefabs, [receipt]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--stage-layout', choices=('legacy', 'mode-description'), required=True)
    parser.add_argument('--output', type=Path, required=True, help='New capture directory')
    parser.add_argument('--identity-fields', type=Path)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Existing powerline capture was retained')
    from extract_uniform_cliffs import capture_prefab
    from lazy_unity_bundle import load
    from terrain_hole_shapes import HoleShapeSources

    before, extractor = sources(args.rust_root), producer()
    programs, workflow = stages(args.rust_root, args.stage_layout)
    manifest = json.loads((args.rust_root / 'Bundles/AssetSceneManifest.json').read_bytes())
    selected = targets(manifest, programs)
    prefabs = {}
    if selected:
        env, stream = load(args.rust_root / 'Bundles/shared/assetscenes.bundle',
                           nodes=set(selected.values()), follow_dependencies=True)
        with ExitStack() as cleanup:
            cleanup.callback(stream.close)
            meshes = HoleShapeSources(args.rust_root, env, False)
            cleanup.callback(meshes.close)
            for asset in env.assets:
                paths = {path for path, node in selected.items() if node == asset.name}
                if not paths:
                    continue
                for obj in asset.objects.values():
                    if int(obj.type) != 1:
                        continue
                    path = obj.read_typetree()['m_Name'].lower()
                    if path in paths:
                        if path in prefabs:
                            raise ValueError('Duplicate powerline prefab root')
                        prefabs[path] = dict(placement=capture_prefab(asset, obj, meshes),
                                            runtime=capture(asset, obj, runtime_types=WIRE_TYPES))
            if set(prefabs) != set(selected):
                raise ValueError('Incomplete powerline prefab capture')
            meshes.verify()
            extra_sources = list(meshes.sources.values())
    else:
        extra_sources = []
    resources, wires, wire_sources = resolve_wires(args.rust_root, manifest, wire_guids(prefabs), args.identity_fields)
    for receipt in wire_sources:
        matching = [source for source in extra_sources if source['relative_path'] == receipt['relative_path']]
        if matching and matching != [receipt]:
            raise ValueError('Powerline captures used different source revisions')
        if not matching:
            extra_sources.append(receipt)
    if before != sources(args.rust_root) or extractor != producer():
        raise ValueError('Powerline source changed during capture')
    report = dict(format='mapper-powerline-source-reference', schema=1,
                  inspection_only=True, game_code_executed=False,
                  sources=before, extra_sources=extra_sources, extractor_sha256=extractor,
                  stage_layout=args.stage_layout, stages=programs,
                  procedural_order=workflow, prefabs=prefabs,
                  wire_resources=resources, wire_prefabs=wires)
    raw = json.dumps(report, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(raw) > LIMIT:
        raise ValueError('Powerline capture exceeds its output budget')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.output.parent) as folder:
        stage = Path(folder) / 'capture'
        stage.mkdir()
        (stage / 'source.json').write_bytes(raw)
        publish_directory(stage, args.output)


if __name__ == '__main__':
    main()
