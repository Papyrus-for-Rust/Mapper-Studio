"""Capture the offshore root program and water-type coverage from each prefab."""
from prefab_geometry import MAX_DEPTH, MAX_NODES, reference, vector
from monument_stages import integer
import monument_placement_attributes as attributes


def inspect(root_id, read, script_type):
    seen, water_types = set(), []
    output = attributes.empty()
    root_pose = None

    def walk(owner, parent, depth):
        nonlocal root_pose
        if owner in seen or len(seen) >= MAX_NODES or depth > MAX_DEPTH:
            raise ValueError('Cyclic or excessive offshore hierarchy')
        seen.add(owner)
        kind, go = read(owner)
        if kind != 1 or len(go['m_Component']) > 256:
            raise ValueError('Invalid offshore GameObject')
        ids = [reference(row['component']) for row in go['m_Component']]
        if len(ids) != len(set(ids)):
            raise ValueError('Duplicate offshore component')
        transforms = [(oid, read(oid)[1]) for oid in ids if read(oid)[0] in (4, 224)]
        if len(transforms) != 1:
            raise ValueError('Ambiguous offshore transform')
        transform_id, transform = transforms[0]
        if reference(transform['m_GameObject']) != owner:
            raise ValueError('Offshore transform owner differs')
        if parent is None:
            if any(abs(v) > 1e-6 for v in vector(transform['m_LocalPosition'])):
                raise ValueError('Nonzero offshore prefab root position')
            root_pose = dict(rotation=vector(transform['m_LocalRotation'], 'xyzw'),
                             scale=vector(transform['m_LocalScale']))
        for oid in ids:
            kind, fields = read(oid)
            if kind != 114:
                continue
            identity = tuple(script_type(oid))
            if identity[:2] != ('Assembly-CSharp', ''):
                continue
            if reference(fields['m_GameObject']) != owner:
                raise ValueError('Offshore component owner differs')
            if identity[2] == 'WaterBody':
                if len(water_types) >= 1024:
                    raise ValueError('Excessive offshore water bodies')
                water_types.append(integer(fields['Type'], -2**31, 2**31-1))
            elif parent is None and identity[2].startswith('Decor'):
                attributes.capture(identity, fields, [0., 0., 0.], True, oid,
                                   go['m_IsActive'], output)
        for child in transform['m_Children']:
            child_kind, child_fields = read(reference(child))
            if child_kind not in (4, 224) or reference(child_fields['m_Father']) != transform_id:
                raise ValueError('Offshore child parent differs')
            walk(reference(child_fields['m_GameObject']), transform_id, depth+1)

    walk(root_id, None, 0)
    if root_pose is None or output['issues']:
        raise ValueError('Incomplete offshore root program')
    return dict(**root_pose, root_decor=output['root_decor'], water_types=water_types,
                nodes_visited=len(seen))


def capture(root, stages):
    import json
    from lazy_unity_bundle import load
    from prefab_paths import component_readers
    manifest = json.loads((root/'Bundles/AssetSceneManifest.json').read_bytes())
    prefixes = [folder for stage in stages for folder in stage['folders']]
    selected = {path.lower():scene['Name'] for scene in manifest['Scenes']
                for path in scene['IncludedAssets'] if path.lower().endswith('.prefab')
                and any(path.lower().startswith(prefix) for prefix in prefixes)}
    if not 1 <= len(selected) <= 128:
        raise ValueError('Invalid offshore prefab roster')
    environment, stream = load(root/'Bundles/shared/assetscenes.bundle',
                               nodes={'BuildPlayer-'+scene for scene in selected.values()},
                               follow_dependencies=True)
    result = {}
    try:
        for asset in environment.assets:
            names = {path for path, scene in selected.items() if asset.name == 'BuildPlayer-'+scene}
            if not names:
                continue
            read, script_type = component_readers(asset.objects)
            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                name = obj.read().m_Name.lower()
                if name in names:
                    if name in result:
                        raise ValueError('Duplicate offshore prefab root')
                    result[name] = inspect(obj.path_id, read, script_type)
        if set(result) != set(selected):
            raise ValueError('Incomplete offshore prefab capture')
        return dict(sorted(result.items()))
    finally:
        stream.close()
