"""Authored dungeon-grid cells and sockets in their original prefab frames."""
from dungeon_prefabs import pose
import math
from monument_stages import flag, integer
from prefab_geometry import IDENTITY, MAX_DEPTH, MAX_NODES, matrix, multiply, reference, vector
import monument_placement_attributes as placement_attributes

KINDS = {'DungeonGridCell', 'DungeonGridLink', 'DungeonGridInfo', 'DungeonVolume',
         'EnvironmentVolume', 'TerrainPathConnect', 'MonumentInfo', 'TerrainAnchor',
         'TerrainFilter'}
DIRECTIONS = ('North', 'South', 'West', 'East')


def positive_dimension(value):
    if type(value) not in (int, float) or not math.isfinite(value) or not 0 < value <= 4096:
        raise ValueError('Invalid tunnel entrance dimension')
    return float(value)


def inspect(root_id, read, script_type):
    nodes, components, seen = {}, [], set()
    registration = placement_attributes.empty()
    # Tunnel frames already include the authored root. The shared environment
    # adapter combines this identity frame with each captured hierarchy matrix.
    registration['environment']['root_transform'] = dict(
        position=[0., 0., 0.], rotation=[0., 0., 0., 1.], scale=[1., 1., 1.])

    def walk(game_object_id, parent_id, parent_matrix, depth, active, rotation_chain):
        if game_object_id in seen or len(seen) >= MAX_NODES or depth > MAX_DEPTH:
            raise ValueError('Cyclic or excessive tunnel hierarchy')
        seen.add(game_object_id)
        kind, owner = read(game_object_id)
        if kind != 1:
            raise ValueError('Tunnel owner is not a GameObject')
        ids = [reference(record['component']) for record in owner['m_Component']]
        if len(ids) > 256 or len(ids) != len(set(ids)):
            raise ValueError('Invalid tunnel component roster')
        records = [(oid, *read(oid)) for oid in ids]
        transforms = [(oid, fields) for oid, kind, fields in records if kind in (4, 224)]
        if len(transforms) != 1:
            raise ValueError('Ambiguous tunnel transform')
        transform_id, transform = transforms[0]
        if reference(transform['m_GameObject']) != game_object_id:
            raise ValueError('Tunnel transform owner mismatch')
        if parent_id is None:
            if transform['m_Father'] != {'m_FileID': 0, 'm_PathID': 0}:
                raise ValueError('Parented tunnel prefab root')
        elif reference(transform['m_Father']) != parent_id:
            raise ValueError('Tunnel child parent mismatch')
        local = pose(transform)
        rotations = rotation_chain + [local['rotation']]
        world = multiply(parent_matrix, matrix(transform))
        relative_active = active and (depth == 0 or flag(owner['m_IsActive']))
        nodes[transform_id] = dict(transform_id=transform_id, parent_transform=parent_id,
                                   matrix=world, rotation_chain=rotations,
                                   active_without_root=relative_active, **local)
        scripts = {oid: tuple(script_type(oid)) for oid, kind, _ in records if kind == 114}
        colliders = [(oid, kind, fields) for oid, kind, fields in records
                     if kind in (64, 65, 135, 136, 154)]
        for object_id, kind, fields in records:
            if kind != 114:
                continue
            identity = scripts[object_id]
            if identity[:2] != ('Assembly-CSharp', '') or not (
                    identity[2] in KINDS or identity[2].startswith(('Terrain', 'WaterCheck', 'Decor'))):
                continue
            if reference(fields['m_GameObject']) != game_object_id:
                raise ValueError('Tunnel component owner mismatch')
            if len(components) >= 4096:
                raise ValueError('Excessive tunnel attributes')
            components.append((object_id, transform_id, identity[2], fields))
            if identity[2] == 'EnvironmentVolume':
                if any(reference(collider['m_GameObject']) != game_object_id
                       for _, _, collider in colliders):
                    raise ValueError('Tunnel environment collider owner mismatch')
                facts = placement_attributes.environment_object(
                    game_object_id, owner['m_Layer'],
                    [oid for oid, identity in scripts.items()
                     if identity == ('Assembly-CSharp', '', 'EnvironmentVolume')],
                    [collider['m_Enabled'] for _, _, collider in colliders])
                placement_attributes.capture(identity, fields, [world[i][3] for i in range(3)],
                    depth == 0, object_id, relative_active, registration,
                    transform=world, collider_types=[kind for _, kind, _ in colliders],
                    collider_ids=[oid for oid, _, _ in colliders],
                    rotation_chain=rotations, game_object=facts)
        children = transform['m_Children']
        if len(children) > MAX_NODES:
            raise ValueError('Excessive tunnel children')
        for child in children:
            child_id = reference(child)
            child_kind, child_fields = read(child_id)
            if child_kind not in (4, 224):
                raise ValueError('Tunnel child is not a transform')
            if reference(child_fields['m_Father']) != transform_id:
                raise ValueError('Tunnel child parent mismatch')
            walk(reference(child_fields['m_GameObject']), transform_id, world,
                 depth + 1, relative_active, rotations)

    walk(root_id, None, IDENTITY, 0, True, [])

    def socket(value):
        object_id = reference(value)
        if object_id not in nodes:
            raise ValueError('Tunnel socket is outside its prefab')
        return nodes[object_id].copy()

    result = dict(cells=[], links=[], entrances=[], volumes=[], environments=[],
                  connectors=[], monument_infos=[], terrain_checks=[], nodes_visited=len(nodes),
                  environment=registration['environment'])
    for object_id, transform_id, kind, fields in components:
        record = dict(object_id=object_id, **nodes[transform_id])
        if kind == 'DungeonGridCell':
            record['connections'] = {name.lower(): integer(fields[name], 0, 255)
                                     for name in DIRECTIONS}
            record['variants'] = {name.lower(): integer(fields[name + 'Variant'], 0, 255)
                                  for name in DIRECTIONS}
            record['replaceable'] = flag(fields['Replaceable'])
            neighbours = fields['AvoidNeighbours']
            if not isinstance(neighbours, list) or len(neighbours) > 256:
                raise ValueError('Invalid tunnel neighbour exclusions')
            # Retain GameObjectRef identities. Resource IDs need the installed
            # string pool and must not be invented from a filename.
            if any(not isinstance(item, dict) or len(item) > 8 for item in neighbours):
                raise ValueError('Invalid tunnel neighbour reference')
            record['avoid_neighbours'] = neighbours
            result['cells'].append(record)
        elif kind == 'DungeonGridLink':
            record.update(up_socket=socket(fields['UpSocket']),
                          down_socket=socket(fields['DownSocket']),
                          up_type=integer(fields['UpType'], 0, 255),
                          down_type=integer(fields['DownType'], 0, 255),
                          priority=integer(fields['Priority'], -65536, 65536),
                          rotation_limit=integer(fields['Rotation'], 0, 360))
            result['links'].append(record)
        elif kind == 'DungeonGridInfo':
            record.update(cell_size=integer(fields['CellSize'], 1, 4096),
                          link_height=positive_dimension(fields['LinkHeight']),
                          link_radius=positive_dimension(fields['LinkRadius']))
            result['entrances'].append(record)
        elif kind == 'DungeonVolume':
            bounds = fields['bounds']
            record.update(center=vector(bounds['m_Center']),
                          size=[2 * value for value in vector(bounds['m_Extent'])])
            if any(value <= 0 for value in record['size']):
                raise ValueError('Invalid tunnel volume')
            result['volumes'].append(record)
        elif kind == 'EnvironmentVolume':
            record.update(mask=integer(fields['Type'], -2**31, 2**31-1),
                          center=vector(fields['Center']), size=vector(fields['Size']))
            if any(value <= 0 for value in record['size']):
                raise ValueError('Invalid tunnel environment volume')
            result['environments'].append(record)
        elif kind == 'TerrainPathConnect':
            record['infrastructure_type'] = integer(fields['Type'], 0, 64)
            result['connectors'].append(record)
        elif kind == 'MonumentInfo':
            result['monument_infos'].append(record)
        else:
            record.update(kind=kind, fields={key: value for key, value in fields.items()
                                            if not key.startswith('m_')})
            result['terrain_checks'].append(record)
    if len(result['cells']) > 1:
        raise ValueError('Ambiguous dungeon-grid cell')
    return result


def capture(root, selected):
    from lazy_unity_bundle import load
    from prefab_paths import component_readers
    wanted = {'BuildPlayer-' + scene for scene in selected.values()}
    environment, stream = load(root / 'Bundles/shared/assetscenes.bundle',
                               nodes=wanted, follow_dependencies=True)
    prefabs = {}
    try:
        for asset in environment.assets:
            names = {path for path, scene in selected.items() if asset.name == 'BuildPlayer-' + scene}
            if not names:
                continue
            read, script_type = component_readers(asset.objects)
            for obj in asset.objects.values():
                if int(obj.type) != 1:
                    continue
                name = obj.read().m_Name.lower()
                if name not in names:
                    continue
                if name in prefabs:
                    raise ValueError('Duplicate tunnel prefab root')
                prefabs[name] = inspect(obj.path_id, read, script_type)
        if set(prefabs) != set(selected):
            raise ValueError('Incomplete tunnel prefab capture')
    finally:
        stream.close()
    return prefabs


def validate_roles(program, prefabs):
    for role, selection in program['selectors'].items():
        for path in selection['prefabs']:
            prefab = prefabs[path]
            if role == 'LinkFolder':
                if len(prefab['links']) != 1:
                    raise ValueError('Tunnel link selector requires one authored socket pair')
            elif len(prefab['cells']) != 1:
                raise ValueError('Tunnel cell selector requires one authored grid cell')
            if role == 'StationFolder' and (len(prefab['links']) != 1 or not prefab['volumes']):
                raise ValueError('Tunnel station requires a link and placement volume')
