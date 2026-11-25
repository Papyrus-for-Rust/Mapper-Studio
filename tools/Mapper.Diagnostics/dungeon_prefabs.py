"""Capture authored dungeon sockets and volumes without executing game code."""
from prefab_geometry import (MAX_DEPTH, MAX_NODES, IDENTITY, matrix, multiply,
                             reference, vector)
from monument_stages import flag, integer


def pose(fields):
    position = vector(fields['m_LocalPosition'])
    rotation = vector(fields['m_LocalRotation'], 'xyzw')
    scale = vector(fields['m_LocalScale'])
    if abs(sum(v * v for v in rotation) - 1.) > .001:
        raise ValueError('Invalid dungeon quaternion')
    return dict(position=position, rotation=rotation, scale=scale)


def inspect(root_id, read, script_type):
    """Preserve Unity traversal order, inactive children and local socket poses.

    DungeonBaseLink reads GetComponentsInChildren(true). Socket/volume methods
    consume local transforms while EnvironmentVolumeEx uses hierarchy transforms.
    Both are retained so these conventions cannot be confused by the assembler.
    """
    nodes, components, seen = {}, [], set()

    def walk(game_object_id, parent_id, parent_matrix, rotation_chain, depth):
        if (game_object_id in seen or len(seen) >= MAX_NODES or depth > MAX_DEPTH):
            raise ValueError('Cyclic or excessive dungeon hierarchy')
        seen.add(game_object_id)
        kind, game_object = read(game_object_id)
        if kind != 1:
            raise ValueError('Dungeon hierarchy owner is not a GameObject')
        records = game_object['m_Component']
        if not isinstance(records, list) or len(records) > 256:
            raise ValueError('Invalid dungeon component roster')
        ids = [reference(item['component']) for item in records]
        if len(set(ids)) != len(ids):
            raise ValueError('Duplicate dungeon component')
        transforms = [(oid, read(oid)[1]) for oid in ids if read(oid)[0] in (4, 224)]
        if len(transforms) != 1:
            raise ValueError('Ambiguous dungeon Transform')
        transform_id, transform = transforms[0]
        if reference(transform['m_GameObject']) != game_object_id:
            raise ValueError('Dungeon transform ownership mismatch')
        local = pose(transform)
        rotations = rotation_chain + [local['rotation']]
        world = multiply(parent_matrix, matrix(transform))
        nodes[transform_id] = dict(parent=parent_id, pose=local, matrix=world,
                                   rotation_chain=rotations)
        for object_id in ids:
            kind, fields = read(object_id)
            if kind == 114:
                identity = script_type(object_id)
                if tuple(identity[:2]) == ('Assembly-CSharp', '') and identity[2] in (
                    'DungeonBaseLink', 'DungeonBaseSocket', 'DungeonBaseTransition',
                    'DungeonVolume', 'DungeonBaseInfo', 'EnvironmentVolume'
                ):
                    if reference(fields['m_GameObject']) != game_object_id:
                        raise ValueError('Dungeon component ownership mismatch')
                    components.append((object_id, transform_id, identity[2], fields))
        children = transform['m_Children']
        if not isinstance(children, list) or len(children) > MAX_NODES:
            raise ValueError('Excessive dungeon children')
        for child in children:
            child_id = reference(child)
            child_kind, child_fields = read(child_id)
            if child_kind not in (4, 224):
                raise ValueError('Dungeon child is not a Transform')
            if reference(child_fields['m_Father']) != transform_id:
                raise ValueError('Dungeon child parent mismatch')
            walk(reference(child_fields['m_GameObject']), transform_id, world, rotations, depth + 1)

    walk(root_id, None, IDENTITY, [], 0)
    if len(components) > 4096:
        raise ValueError('Excessive dungeon attributes')
    roots = [oid for oid, node in nodes.items() if node['parent'] is None]
    root_transform = nodes[roots[0]]['pose']
    if (any(abs(v) > 1e-6 for v in root_transform['position'])
            or any(abs(a - b) > 1e-6 for a, b in zip(root_transform['rotation'], (0., 0., 0., 1.)))
            or any(abs(v - 1.) > 1e-6 for v in root_transform['scale'])):
        raise ValueError('Nonidentity dungeon root transform')

    def descendant(node_id, ancestor):
        while node_id is not None:
            if node_id == ancestor:
                return True
            node_id = nodes[node_id]['parent']
        return False

    def attribute(object_id, transform_id):
        node = nodes[transform_id]
        return dict(object_id=object_id, transform_id=transform_id,
                    parent_transform=node['parent'], **node['pose'])

    links, transitions, environments, entrances = [], [], [], []
    for object_id, transform_id, kind, fields in components:
        if kind == 'DungeonBaseLink':
            record = attribute(object_id, transform_id)
            record.update(kind=integer(fields['Type'], 0, 2),
                          cost=integer(fields['Cost'], 0, 4096),
                          maximum_floor=integer(fields['MaxFloor'], -1, 4096),
                          maximum_local=integer(fields['MaxCountLocal'], -1, 4096),
                          maximum_global=integer(fields['MaxCountGlobal'], -1, 4096),
                          count_identifier=integer(fields['MaxCountIdentifier'], -1, 2**31 - 1),
                          sockets=[], volumes=[])
            for oid, tid, child_kind, child_fields in components:
                if not descendant(tid, transform_id):
                    continue
                child = attribute(oid, tid)
                if child_kind == 'DungeonBaseSocket':
                    child.update(kind=integer(child_fields['Type'], 0, 2),
                                 male=flag(child_fields['Male']), female=flag(child_fields['Female']),
                                 matrix=nodes[tid]['matrix'], rotation_chain=nodes[tid]['rotation_chain'])
                    record['sockets'].append(child)
                elif child_kind == 'DungeonVolume':
                    bounds = child_fields['bounds']
                    child.update(center=vector(bounds['m_Center']),
                                 size=[v * 2. for v in vector(bounds['m_Extent'])])
                    if any(v <= .01 for v in child['size']):
                        raise ValueError('Invalid dungeon volume dimensions')
                    record['volumes'].append(child)
            if not 1 <= len(record['sockets']) <= 256 or len(record['volumes']) > 256:
                raise ValueError('Invalid dungeon socket or volume roster')
            links.append(record)
        elif kind == 'DungeonBaseTransition':
            record = attribute(object_id, transform_id)
            record.update(kind=integer(fields['Type'], 0, 2),
                          neighbour1=integer(fields['Neighbour1'], 0, 2),
                          neighbour2=integer(fields['Neighbour2'], 0, 2))
            transitions.append(record)
        elif kind == 'EnvironmentVolume':
            record = attribute(object_id, transform_id)
            record.update(mask=integer(fields['Type'], -2**31, 2**31 - 1),
                          center=vector(fields['Center']), size=vector(fields['Size']),
                          matrix=nodes[transform_id]['matrix'],
                          rotation_chain=nodes[transform_id]['rotation_chain'])
            if any(v <= 0 for v in record['size']):
                raise ValueError('Invalid dungeon environment dimensions')
            environments.append(record)
        elif kind == 'DungeonBaseInfo':
            record = attribute(object_id, transform_id)
            record['matrix'] = nodes[transform_id]['matrix']
            record['rotation_chain'] = nodes[transform_id]['rotation_chain']
            entrances.append(record)
    if len(links) + len(transitions) != 1:
        raise ValueError('Dungeon prefab requires one link or transition')
    return dict(links=links, transitions=transitions, environment_volumes=environments,
                entrances=entrances,
                nodes_visited=len(nodes))
