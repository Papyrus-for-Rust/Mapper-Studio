"""Exact roadside program fields and relative road-stage sequencing."""
from monument_stages import BASE, integer, flag, folders, filter_data
from road_stages import CURRENT_BASE, activation

FIELDS = BASE | {'Filter', 'ResourceFolder', 'Monuments', 'TargetCount',
                 'MinDistanceSameType', 'MinDistanceDifferentType', 'MinWorldSize',
                 'DistanceSameType', 'DistanceDifferentType', 'RoadType'}
ORDER = 'relative hierarchy MonoBehaviour order; not an official seed ordinal'
KINDS = {'GenerateRoadRing', 'GenerateRoadLayout', 'PlaceMonumentsRoadside',
         'GenerateRoadTopology', 'GenerateRoadTerrain', 'GenerateRoadTexture'}


def capture_dungeon(identity, object_id, result):
    if identity[:2] != ('Assembly-CSharp', '') or identity[2] not in ('DungeonGridLink', 'DungeonGridInfo'):
        return
    object_id = integer(object_id, 1, 2**63-1)
    if object_id in result['links'] or object_id in result['entrances'] or len(result['links'])+len(result['entrances']) >= 4096:
        raise ValueError('Invalid dungeon component inventory')
    result['links' if identity[2] == 'DungeonGridLink' else 'entrances'].append(object_id)


def capture_root_water(identity, fields, object_id, root_id, result):
    if identity != ('Assembly-CSharp', '', 'Monument'):
        return
    owner = fields['m_GameObject']
    if owner['m_FileID'] != 0 or owner['m_PathID'] != root_id:
        return
    if result:
        raise ValueError('Ambiguous root monument water policy')
    integer(object_id, 1, 2**63-1)
    result.append(flag(fields['WaterMap']))


def validate_road_owners(sockets, road_type):
    """One matching component per owner avoids undefined within-object order."""
    seen = set()
    for connector in sockets['connectors']:
        owner = integer(connector['gameObjectId'], 1, 2**63-1)
        if connector['infrastructureType'] == road_type:
            if owner in seen:
                raise ValueError('Multiple road sockets on one object need a runtime ordering adapter')
            seen.add(owner)


def stage_data(identity, fields, object_id, order, active, *, stage_layout='legacy'):
    if identity != ('Assembly-CSharp', '', 'PlaceMonumentsRoadside'):
        raise ValueError('Unsupported roadside program identity')
    extra = FIELDS - BASE
    flags = activation(fields, extra, stage_layout)
    expected = (BASE if stage_layout == 'legacy' else CURRENT_BASE) | extra
    if {k for k in fields if not k.startswith('m_')} != expected:
        raise ValueError('Unknown roadside program fields')
    if fields['Monuments'] != []:
        raise ValueError('Explicit roadside references need a GUID adapter')
    return dict(object_id=integer(object_id, 1, 2**63-1), order=integer(order, 0, 65535),
                **flags, hierarchy_active=flag(active),
                folders=folders(fields['ResourceFolder']), filter=filter_data(fields['Filter']),
                minimum_world_size=integer(fields['MinWorldSize'], 0, 6000),
                target_count=integer(fields['TargetCount'], 0, 4096),
                minimum_distance_same_type=integer(fields['MinDistanceSameType'], 0, 10000),
                minimum_distance_different_type=integer(fields['MinDistanceDifferentType'], 0, 10000),
                distance_same_type=integer(fields['DistanceSameType'], 0, 2),
                distance_different_type=integer(fields['DistanceDifferentType'], 0, 2),
                road_mode=integer(fields['RoadType'], 0, 4))


def sequence_data(identity, fields, object_id, order, active, *, stage_layout='legacy'):
    if identity[:2] != ('Assembly-CSharp', '') or identity[2] not in KINDS:
        raise ValueError('Unsupported roadside sequence component')
    if stage_layout == 'legacy':
        flags = dict(mode=integer(fields['Mode'], -1, 3), disabled=flag(fields['Disabled']),
                     component_enabled=flag(fields['m_Enabled']))
    else:
        extra = {'GenerateRoadRing': {'MinWorldSize'}, 'GenerateRoadLayout': {'RoadType'},
                 'PlaceMonumentsRoadside': FIELDS - BASE}.get(identity[2], set())
        flags = activation(fields, extra, stage_layout)
    result = dict(kind=identity[2], object_id=integer(object_id, 1, 2**63-1),
                  order=integer(order, 0, 65535), **flags,
                  hierarchy_active=flag(active))
    if identity[2] == 'GenerateRoadLayout':
        result['infrastructure_type'] = integer(fields['RoadType'], 0, 64)
    return result


def collect(root, *, stage_layout='legacy'):
    if stage_layout not in ('legacy', 'mode-description'):
        raise ValueError('Unsupported roadside stage activation layout')
    from UnityPy.classes.PPtr import PPtr
    from lazy_unity_bundle import load
    env, stream = load(root/'Bundles/maps/maps.bundle')
    try:
        scenes = [a for a in env.assets if a.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]

        def identity(obj):
            f = obj.read().m_Script.read_typetree()
            return f['m_AssemblyName'], f['m_Namespace'], f['m_ClassName']

        setups = [o for o in scene.objects.values() if int(o.type) == 114
                  and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous WorldSetup')

        def ptr(ref):
            if ref['m_FileID'] != 0:
                raise ValueError('External procedural hierarchy')
            return PPtr(**ref, assetsfile=scene).deref()

        stages, sequence, seen = [], [], set()
        ordinal = 0

        def walk(go, active=True, depth=0):
            nonlocal ordinal
            if depth > 128 or go.path_id in seen or len(seen) >= 65536:
                raise ValueError('Invalid procedural hierarchy')
            seen.add(go.path_id)
            f = go.read_typetree()
            active = active and flag(f['m_IsActive'])
            records = [ptr(c['component']) for c in f['m_Component']]
            for record in records:
                if int(record.type) != 114:
                    continue
                ident = identity(record)
                if ident[2] in KINDS:
                    data = record.read_typetree()
                    sequence.append(sequence_data(ident, data, record.path_id, ordinal, active,
                                                  stage_layout=stage_layout))
                    if ident[2] == 'PlaceMonumentsRoadside':
                        stages.append(stage_data(ident, data, record.path_id, ordinal, active,
                                                 stage_layout=stage_layout))
                ordinal += 1
            transforms = [r for r in records if int(r.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous procedural transform')
            for child in transforms[0].read_typetree()['m_Children']:
                walk(ptr(ptr(child).read_typetree()['m_GameObject']), active, depth+1)

        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if len(stages) != 1 or not 1 <= len(sequence) <= 32:
            raise ValueError('Unsupported roadside stage roster')
        return stages, sequence
    finally:
        stream.close()
