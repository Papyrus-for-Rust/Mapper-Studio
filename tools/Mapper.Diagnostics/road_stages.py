"""Read the installation's ordered road components without executing game code."""
BASE = {'Mode', 'Disabled', 'Description', 'HideInMapCreator', 'MapCreatorCategory'}
CURRENT_BASE = {'Mode', 'Description'}


def source_layout(source):
    if (not isinstance(source, dict) or source.get('format') != 'papyrus-infrastructure-source'
            or type(source.get('schema')) is not int or not isinstance(source.get('profile'), dict)):
        raise ValueError('Unsupported infrastructure source')
    layout = source['profile'].get('stage_layout')
    if (source['schema'], source.get('adapter'), layout) not in (
            (1, 'papyrus-path-connectors-1', None),
            (2, 'papyrus-path-connectors-2', 'mode-description')):
        raise ValueError('Infrastructure stage layout differs from its source version')
    return 'legacy' if layout is None else layout


def activation(fields, extra, layout):
    if layout not in ('legacy', 'mode-description'):
        raise ValueError('Unsupported road stage activation layout')
    base = BASE if layout == 'legacy' else CURRENT_BASE
    if any(not name.startswith('m_') and name not in base | extra for name in fields):
        raise ValueError('Unknown road stage field')
    required = {'Mode', 'm_Enabled'} | ({'Disabled'} if layout == 'legacy' else CURRENT_BASE)
    if not required.issubset(fields):
        raise ValueError('Missing road stage activation field')
    if type(fields['Mode']) is not int or fields['Mode'] not in (-1, 0, 1, 2, 3):
        raise ValueError('Unknown road realm')
    for name in ('m_Enabled', 'Disabled') if layout == 'legacy' else ('m_Enabled',):
        if type(fields[name]) not in (bool, int) or fields[name] not in (0, 1):
            raise ValueError('Invalid road activation flag')
    return dict(mode=fields['Mode'],
                disabled=bool(fields['Disabled']) if layout == 'legacy' else None,
                component_enabled=bool(fields['m_Enabled']))


def stage_data(identity, fields, object_id, order, active, *, layout='legacy'):
    if identity != ('Assembly-CSharp', '', 'GenerateRoadLayout'):
        raise ValueError('Unsupported road layout identity')
    flags = activation(fields, {'RoadType'}, layout)
    if type(fields.get('RoadType')) is not int or not 0 <= fields['RoadType'] <= 64:
        raise ValueError('Unknown road infrastructure type')
    return dict(object_id=object_id, order=order, infrastructure_type=fields['RoadType'],
                **flags, hierarchy_active=bool(active))


def ring_stage_data(identity, data, object_id, order, active, *, layout='legacy'):
    if identity != ('Assembly-CSharp', '', 'GenerateRoadRing'):
        raise ValueError('Unsupported road ring identity')
    flags = activation(data, {'MinWorldSize'}, layout)
    if type(data.get('MinWorldSize')) is not int or not 1000 <= data['MinWorldSize'] <= 6000:
        raise ValueError('Unsupported road ring minimum world size')
    return dict(object_id=object_id, order=order, **flags, hierarchy_active=bool(active),
                minimum_world_size=data['MinWorldSize'])


def topology_stage_data(identity, data, object_id, order, active, *, layout='legacy'):
    if identity != ('Assembly-CSharp', '', 'GenerateRoadTopology'):
        raise ValueError('Unsupported road topology identity')
    flags = activation(data, set(), layout)
    return dict(object_id=object_id, order=order, **flags, hierarchy_active=bool(active))


def layout_stages(root, include_ring=False, include_topology=False, *, stage_layout='legacy'):
    if stage_layout not in ('legacy', 'mode-description'):
        raise ValueError('Unsupported road stage activation layout')
    from UnityPy.classes.PPtr import PPtr
    from lazy_unity_bundle import load
    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={'BuildPlayer-Procedural Map'}, follow_dependencies=True)
    try:
        scenes = [a for a in env.assets if a.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]
        def identity(obj):
            fields = obj.read().m_Script.read_typetree()
            return fields['m_AssemblyName'], fields['m_Namespace'], fields['m_ClassName']
        setups = [o for o in scene.objects.values() if int(o.type) == 114
                  and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous WorldSetup')
        def ptr(ref):
            if ref['m_FileID'] != 0:
                raise ValueError('External procedural hierarchy')
            return PPtr(**ref, assetsfile=scene).deref()
        stages, seen = [], set()
        def walk(go, active=True, depth=0):
            if depth > 128 or go.path_id in seen or len(seen) >= 65536:
                raise ValueError('Invalid procedural hierarchy')
            seen.add(go.path_id)
            fields = go.read_typetree()
            active = active and bool(fields['m_IsActive'])
            records = [ptr(c['component']) for c in fields['m_Component']]
            for record in records:
                if int(record.type) == 114:
                    ident = identity(record)
                    if ident[2] == 'GenerateRoadLayout':
                        row=stage_data(ident, record.read_typetree(), record.path_id, len(stages), active, layout=stage_layout)
                        if include_ring: row['kind']='layout'
                        stages.append(row)
                    elif include_ring and ident[2] == 'GenerateRoadRing':
                        row=ring_stage_data(ident,record.read_typetree(),record.path_id,len(stages),active,layout=stage_layout)
                        row['kind']='ring'
                        stages.append(row)
                    elif include_topology and ident[2] == 'GenerateRoadTopology':
                        row=topology_stage_data(ident,record.read_typetree(),record.path_id,len(stages),active,layout=stage_layout)
                        row['kind']='topology'
                        stages.append(row)
            transform = next(o.read_typetree() for o in records if int(o.type) == 4)
            for child in transform['m_Children']:
                walk(ptr(ptr(child).read_typetree()['m_GameObject']), active, depth + 1)
        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if not 1 <= len(stages) <= 16:
            raise ValueError('Unsupported road layout roster')
        return stages
    finally:
        stream.close()
