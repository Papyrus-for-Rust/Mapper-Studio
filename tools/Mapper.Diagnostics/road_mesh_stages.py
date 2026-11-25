"""Inspect exact road mesh stage resources without loading game code."""
from road_stages import activation
RESOURCES = ('RoadMesh', 'RoadMeshes', 'RoadMaterial', 'RoadRingMaterial', 'RoadPhysicMaterial')


def validate_policy(policy):
    import math
    keys = {'engine', 'chunk_repetitions', 'maximum_hierarchy_exclusive', 'spline', 'snap_to_terrain',
            'snap_open_endpoints', 'normal_smoothing', 'random_scale', 'mesh_offset', 'noise_export',
            'noise_frequency', 'noise_unsigned_scale', 'noise_unsigned_bias', 'scale_width_with_length',
            'top_aligned', 'round_vertices'}
    if not isinstance(policy, dict) or set(policy) != keys or policy['engine'] != 'papyrus-road-surface-1':
        raise ValueError('Unsupported road surface policy schema')
    for key, maximum in [('chunk_repetitions', 32), ('maximum_hierarchy_exclusive', 4)]:
        if type(policy[key]) is not int or not 1 <= policy[key] <= maximum:
            raise ValueError('Invalid road surface scheduling')
    for key in ('spline', 'snap_to_terrain', 'snap_open_endpoints', 'scale_width_with_length', 'top_aligned'):
        if type(policy[key]) is not bool:
            raise ValueError('Invalid road surface boolean')
    if not policy['spline'] or policy['scale_width_with_length'] or type(policy['round_vertices']) is not int or policy['round_vertices'] != 0:
        raise ValueError('Unsupported road surface deformation feature')
    for key, low, high in [('normal_smoothing', 0, 0), ('random_scale', .01, 8), ('mesh_offset', -32, 32),
                           ('noise_frequency', 1e-6, 1), ('noise_unsigned_scale', -8, 8), ('noise_unsigned_bias', -8, 8)]:
        value = policy[key]
        if type(value) not in (float, int) or not math.isfinite(value) or not low <= value <= high:
            raise ValueError('Unsupported road surface numeric policy')
    if policy['noise_export'] != 'snoise1_32':
        raise ValueError('Unsupported road noise export')


def boolean(value):
    if type(value) not in (int, bool) or value not in (0, 1):
        raise ValueError('Invalid road mesh activation flag')
    return bool(value)


def stage_data(identity, fields, object_id, order, active, *, stage_layout='legacy'):
    if identity != ('Assembly-CSharp', '', 'GenerateRoadMeshes'):
        raise ValueError('Unsupported road mesh component identity')
    flags = activation(fields, set(RESOURCES), stage_layout)
    if not isinstance(fields['RoadMeshes'], list) or len(fields['RoadMeshes']) > 16:
        raise ValueError('Road mesh LOD roster exceeds bounds')
    return dict(object_id=object_id, order=order, **flags,
                hierarchy_active=boolean(active), lod_array_count=len(fields['RoadMeshes']), assets=[])


def mesh_stages(root, *, stage_layout='legacy'):
    if stage_layout not in ('legacy', 'mode-description'):
        raise ValueError('Unsupported road mesh activation layout')
    from UnityPy.classes.PPtr import PPtr
    from lazy_unity_bundle import load
    from extract_rail_source import reference
    scene_name = 'BuildPlayer-Procedural Map'
    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={scene_name}, follow_dependencies=True)
    try:
        scenes = [asset for asset in env.assets if asset.name == scene_name]
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]
        def identity(obj):
            data = obj.read().m_Script.read_typetree()
            return data['m_AssemblyName'], data['m_Namespace'], data['m_ClassName']
        setups = [obj for obj in scene.objects.values() if int(obj.type) == 114
                  and identity(obj) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous WorldSetup for road meshes')
        def ptr(ref):
            if ref['m_FileID'] != 0:
                raise ValueError('External procedural hierarchy')
            return PPtr(**ref, assetsfile=scene).deref()
        seen, stages = set(), []
        def walk(go, active=True, depth=0):
            if depth > 128 or go.path_id in seen or len(seen) >= 65536:
                raise ValueError('Invalid road mesh hierarchy')
            seen.add(go.path_id)
            fields = go.read_typetree()
            active = active and boolean(fields['m_IsActive'])
            records = [ptr(ref['component']) for ref in fields['m_Component']]
            for obj in records:
                if int(obj.type) != 114 or identity(obj) != ('Assembly-CSharp', '', 'GenerateRoadMeshes'):
                    continue
                data = obj.read_typetree()
                row = stage_data(identity(obj), data, obj.path_id, len(stages), active,
                                 stage_layout=stage_layout)
                for field in RESOURCES:
                    values = data[field] if field == 'RoadMeshes' else [data[field]]
                    for index, value in enumerate(values):
                        ref = reference(value, scene)
                        if ref is not None:
                            row['assets'].append(dict(field=field, index=index, **ref))
                stages.append(row)
            transforms = [obj.read_typetree() for obj in records if int(obj.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous procedural transform')
            for child in transforms[0]['m_Children']:
                walk(ptr(ptr(child).read_typetree()['m_GameObject']), active, depth + 1)
        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if not 1 <= len(stages) <= 16:
            raise ValueError('Unsupported road mesh stage count')
        return stages
    finally:
        stream.close()
