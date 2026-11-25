"""Capture cable nodes for an explicit scene-root roster without executing Rust.

Every requested prefab receives a row, including prefabs with no cable nodes.
The roster can be assembled from all generation families during preparation.
"""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile

from extract_powerline_source import resolve_wires, SCRIPTS as POWERLINE_SCRIPTS
from extract_verified_prefabs import snapshot
from extract_uniform_cliffs import identity, local_pointer
from mapper_paths import publish_directory
from mapper_progress import report
from metadata_transforms import multiply
from monument_stages import flag
from prefab_geometry import IDENTITY, matrix

WIRE_TYPES = {'PowerlineNode', 'PowerLineWireConnectionHelper'}
WIRE_IDENTITIES = {('Assembly-CSharp', '', name) for name in WIRE_TYPES}
MAX_PREFABS = 8192
MAX_INPUT = 8 * 1024**2
MAX_OUTPUT = 64 * 1024**2
SCRIPTS = ('extract_scene_wires.py', 'metadata_transforms.py') + POWERLINE_SCRIPTS


def script_owner(asset, reference):
    from UnityPy.classes.PPtr import PPtr

    script = PPtr(m_FileID=reference[0], m_PathID=reference[1], assetsfile=asset).deref()
    if int(script.type) != 115:
        raise ValueError('Scene cable script type does not reference a MonoScript')
    data = script.read_typetree()
    return data['m_AssemblyName'], data['m_Namespace'], data['m_ClassName']


def wire_type_index(asset):
    """Resolve each serialized script type once, without component payloads."""
    result, scripts = {}, {}
    for obj in asset.objects.values():
        if int(obj.type) != 114 or obj.type_id in result:
            continue
        type_ = obj.serialized_type
        index = type_.script_type_index if type_ is not None else -1
        if index == -1:
            # Older layouts can omit this metadata. Their object-level script
            # pointer still supplies the existing exact identity path.
            result[obj.type_id] = None
            continue
        if not 0 <= index < len(asset.script_types):
            raise ValueError('Invalid scene cable script type index')
        script = asset.script_types[index]
        reference = (script.local_serialized_file_index, script.local_identifier_in_file)
        if reference[0] < 0 or reference[1] == 0:
            raise ValueError('Invalid scene cable script type reference')
        if reference not in scripts:
            scripts[reference] = script_owner(asset, reference)
        result[obj.type_id] = scripts[reference], reference
    return result


def capture(asset, root, types=None):
    """Read the hierarchy and wire payloads, including UI transform children."""
    rows, seen = [], set()
    payload_bytes = 0

    def walk(go, parent=IDENTITY, active=True, depth=0, incoming=None):
        nonlocal payload_bytes
        if depth > 128 or go.path_id in seen or len(seen) >= 65536:
            raise ValueError('Invalid scene cable prefab hierarchy')
        seen.add(go.path_id)
        data = go.read_typetree()
        local_active = flag(data['m_IsActive'])
        active = active and (depth == 0 or local_active)
        if len(data['m_Component']) > 4096:
            raise ValueError('Scene cable object component budget exhausted')
        objects = [local_pointer(asset, entry['component']) for entry in data['m_Component']]
        transforms = [obj for obj in objects if int(obj.type) in (4, 224)]
        if len(transforms) != 1:
            raise ValueError('Scene cable object requires one transform')
        reader = transforms[0]
        # Following a child pointer already read this transform. Still check
        # the owning GameObject's component list before reusing that read.
        transform = (incoming[1] if incoming is not None and incoming[0] is reader
                     else reader.read_typetree())
        local = parent if depth == 0 else multiply(parent, matrix(transform))
        for obj in objects:
            if int(obj.type) != 114:
                continue
            entry = None if types is None else types[obj.type_id]
            if entry is not None and entry[0] not in WIRE_IDENTITIES:
                continue
            if obj.byte_size > 1024**2:
                raise ValueError('Scene cable component byte budget exhausted')
            owner = identity(obj) if entry is None else entry[0]
            if owner not in WIRE_IDENTITIES:
                continue
            payload_bytes += obj.byte_size
            if len(rows) >= 8192 or payload_bytes > 8 * 1024**2:
                raise ValueError('Scene cable prefab payload budget exhausted')
            fields = obj.read_typetree()
            if entry is not None and fields.get('m_Script') != dict(
                    m_FileID=entry[1][0], m_PathID=entry[1][1]):
                raise ValueError('Scene cable script pointer differs from its serialized type')
            rows.append(dict(game_object=go.path_id, name=data['m_Name'], object_id=obj.path_id,
                             hierarchy_active_without_root=active, layer=data['m_Layer'],
                             local_to_root=local, component=owner, fields=fields))
        for child in transform['m_Children']:
            child_reader = local_pointer(asset, child)
            child_transform = child_reader.read_typetree()
            walk(local_pointer(asset, child_transform['m_GameObject']), local, active,
                 depth + 1, (child_reader, child_transform))

    walk(root)
    return rows


def producer():
    return hashlib.sha256(b''.join(
        name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes()
        for name in SCRIPTS)).hexdigest()


def targets(manifest, paths):
    if (not isinstance(paths, list) or not 1 <= len(paths) <= MAX_PREFABS
            or any(not isinstance(path, str) for path in paths)):
        raise ValueError('Invalid scene cable prefab roster')
    normalized = [path.lower() for path in paths]
    if len(set(normalized)) != len(normalized):
        raise ValueError('Duplicate scene cable prefab')
    for path in normalized:
        if (not 1 <= len(path) <= 512 or not path.startswith('assets/')
                or not path.endswith('.prefab')
                or any(part in ('', '.', '..') for part in path.split('/'))
                or any(character in path for character in ('\\', '\0', ':'))):
            raise ValueError('Invalid scene cable prefab path')
    # Index once for the whole roster. Resolving each requested path through
    # a separate manifest scan makes large scenes unnecessarily expensive.
    requested, selected = set(normalized), {}
    for scene in manifest['Scenes']:
        for value in scene['IncludedAssets']:
            path = value.lower()
            if path not in requested:
                continue
            if path in selected:
                raise ValueError('Ambiguous scene cable prefab scene')
            selected[path] = 'BuildPlayer-' + scene['Name']
    if set(selected) != requested:
        raise ValueError('Missing scene cable prefab scene')
    return {path: selected[path] for path in sorted(selected)}


def capture_roots(root, selected):
    from lazy_unity_bundle import load

    by_scene = {}
    for path, scene in selected.items():
        by_scene.setdefault(scene, set()).add(path)
    env, stream = load(root / 'Bundles/shared/assetscenes.bundle',
                       nodes=set(by_scene), follow_dependencies=True)
    prefabs, output_bytes = {}, 0
    try:
        for asset in env.assets:
            requested = by_scene.get(asset.name)
            if requested is None:
                continue
            types = wire_type_index(asset)
            has_wires = any(entry is None or entry[0] in WIRE_IDENTITIES for entry in types.values())
            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                # Most GameObjects are children. Their names are enough to
                # rule them out without decoding component arrays.
                try:
                    name = obj.peek_name()
                except (AttributeError, ValueError, TypeError, IndexError):
                    name = None
                if isinstance(name, str) and name.lower() not in requested:
                    continue
                path = obj.read_typetree()['m_Name'].lower()
                if path not in requested:
                    continue
                if path in prefabs:
                    raise ValueError('Duplicate scene cable prefab root')
                rows = capture(asset, obj, types) if has_wires else []
                prefabs[path] = [row for row in rows if tuple(row['component']) in WIRE_IDENTITIES]
                output_bytes += len(json.dumps(prefabs[path], allow_nan=False).encode('utf-8'))
                if output_bytes > MAX_OUTPUT:
                    raise ValueError('Scene cable capture exceeds its output budget')
                report('Reading cable nodes', path, len(prefabs), len(selected))
        if set(prefabs) != set(selected):
            raise ValueError('Incomplete scene cable prefab capture')
    finally:
        stream.close()
    return {path: prefabs[path] for path in sorted(prefabs)}


def required_wires(prefabs):
    from road_object_stages import guid_reference

    required = set()
    for rows in prefabs.values():
        for row in rows:
            if tuple(row['component']) != ('Assembly-CSharp', '', 'PowerlineNode'):
                continue
            # Inactive descendants cannot register in the reviewed Awake path.
            # Their fields remain in the capture for future activation analysis.
            if not row['hierarchy_active_without_root']:
                continue
            guid = guid_reference(row['fields'].get('WirePrefab'))
            if guid is None:
                raise ValueError('Invalid scene cable wire reference')
            required.add(guid)
    return required


def inspect(root, paths, identity_fields=None):
    before, extractor = snapshot(root), producer()
    manifest = json.loads((root / 'Bundles/AssetSceneManifest.json').read_bytes())
    selected = targets(manifest, paths)
    prefabs = capture_roots(root, selected)
    resources, wires, extra_sources = resolve_wires(root, manifest, required_wires(prefabs), identity_fields)
    if before != snapshot(root) or extractor != producer():
        raise ValueError('Scene cable source changed during capture')
    return dict(format='mapper-scene-wire-source-reference', schema=1,
                inspection_only=True, game_code_executed=False, physics_executed=False,
                sources=before, extra_sources=extra_sources, extractor_sha256=extractor,
                prefabs=prefabs, wire_resources=resources, wire_prefabs=wires)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--prefabs', type=Path, required=True, help='JSON array of prefab paths')
    parser.add_argument('--output', type=Path, required=True, help='New capture directory')
    parser.add_argument('--identity-fields', type=Path)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError('Existing scene cable capture was retained')
    with args.prefabs.open('rb') as stream:
        raw = stream.read(MAX_INPUT + 1)
    if len(raw) > MAX_INPUT:
        raise ValueError('Scene cable request exceeds its input budget')
    result = inspect(args.rust_root, json.loads(raw), args.identity_fields)
    raw = json.dumps(result, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(raw) > MAX_OUTPUT:
        raise ValueError('Scene cable capture exceeds its output budget')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.output.parent) as directory:
        stage = Path(directory) / 'capture'
        stage.mkdir()
        (stage / 'source.json').write_bytes(raw)
        publish_directory(stage, args.output)
    report('Reading cable nodes', 'Complete', len(result['prefabs']), len(result['prefabs']), force=True)


if __name__ == '__main__':
    main()
