"""Capture PlaceMonuments scene data without executing game code.

Hierarchy order is relative MonoBehaviour order, not an official seed ordinal.
The legacy execution adapter treats Unity enabled/active flags as provenance.
Inventory also recognizes newer Mode/Description-only bases without enabling
their execution through that adapter.
"""
import math

BASE = {'Mode', 'Disabled', 'Description', 'HideInMapCreator', 'MapCreatorCategory'}
CURRENT_BASE = {'Mode', 'Description'}
LAND = BASE | {'Filter', 'ResourceFolder', 'Monuments', 'TargetCount',
               'TargetCountWorldSizeMultiplier', 'MinDistanceSameType',
               'MinDistanceDifferentType', 'MinWorldSize', 'DistanceSameType',
               'DistanceDifferentType', 'NexusOnly'}
PREFIX = 'assets/bundled/prefabs/autospawn/'
CURRENT_LAND = LAND - (BASE - CURRENT_BASE)


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError('Invalid monument stage integer')
    return value


def flag(value):
    if type(value) not in (int, bool) or value not in (0, 1):
        raise ValueError('Invalid monument stage activation flag')
    return bool(value)


def folders(value):
    if not isinstance(value, str) or not 1 <= len(value) <= 2048:
        raise ValueError('Invalid monument resource folders')
    out = []
    for part in value.split(','):
        if (not part or part != part.strip() or len(part) > 256
                or any(not (c.isascii() and (c.isalnum() or c in '_-/')) for c in part)
                or any(p in ('', '.', '..') for p in part.split('/'))):
            raise ValueError('Unsafe monument resource folder')
        path = PREFIX + part.lower() + '/'
        if path in out:
            raise ValueError('Duplicate monument resource folder')
        out.append(path)
    if len(out) > 32:
        raise ValueError('Too many monument resource folders')
    return out


def filter_data(value):
    names = {'SplatType', 'BiomeType', 'TopologyAny', 'TopologyAll', 'TopologyNot'}
    if not isinstance(value, dict) or set(value) != names:
        raise ValueError('Unknown monument stage filter layout')
    for name in names:
        integer(value[name], -2**31, 2**31 - 1)
    if any(value[n] == 0 for n in ('SplatType', 'BiomeType', 'TopologyAny')):
        raise ValueError('Empty monument stage filter')
    return dict(value)


def curve_data(value):
    if not isinstance(value, dict) or set(value) != {'m_Curve', 'm_PreInfinity', 'm_PostInfinity', 'm_RotationOrder'}:
        raise ValueError('Unknown monument target curve layout')
    keys = value['m_Curve']
    if not isinstance(keys, list) or not 1 <= len(keys) <= 64:
        raise ValueError('Invalid monument target curve keys')
    previous = -math.inf
    normalized = []
    for key in keys:
        if set(key) != {'time', 'value', 'inSlope', 'outSlope', 'weightedMode', 'inWeight', 'outWeight'}:
            raise ValueError('Unknown monument target curve key')
        integer(key['weightedMode'], 0, 3)
        captured = dict(key)
        for name in ('time', 'value', 'inSlope', 'outSlope', 'inWeight', 'outWeight'):
            v = key[name]
            if type(v) in (int, float) and math.isinf(v) and name in ('inSlope', 'outSlope'):
                captured[name] = 'positive_infinity' if v > 0 else 'negative_infinity'
                continue
            if type(v) not in (int, float) or not math.isfinite(v) or abs(v) > 1_000_000:
                raise ValueError('Invalid monument target curve value')
        if key['time'] <= previous or key['value'] < 0:
            raise ValueError('Invalid monument target curve ordering')
        previous = key['time']
        normalized.append(captured)
    for name in ('m_PreInfinity', 'm_PostInfinity'):
        integer(value[name], 0, 8)
    integer(value['m_RotationOrder'], 0, 8)
    return dict(value, m_Curve=normalized)


def stage_data(identity, fields, object_id, order, hierarchy_active, *, layout='legacy'):
    if identity != ('Assembly-CSharp', '', 'PlaceMonuments'):
        raise ValueError('Unsupported land monument program identity')
    layouts = {'legacy': LAND, 'hierarchy': CURRENT_LAND}
    if layout not in layouts:
        raise ValueError('Unsupported monument stage activation layout')
    if {k for k in fields if not k.startswith('m_')} != layouts[layout]:
        raise ValueError('Unknown land monument program fields')
    # Explicit-reference selectors need a separate GUID-resolution adapter.
    if fields['Monuments'] != []:
        raise ValueError('Explicit monument references need a selector adapter')
    return dict(object_id=integer(object_id, 1, 2**63-1),
                order=integer(order, 0, 65535),
                mode=integer(fields['Mode'], -1, 3),
                disabled=flag(fields['Disabled']) if layout == 'legacy' else None,
                component_enabled=flag(fields['m_Enabled']),
                hierarchy_active=flag(hierarchy_active),
                folders=folders(fields['ResourceFolder']),
                filter=filter_data(fields['Filter']),
                minimum_world_size=integer(fields['MinWorldSize'], 0, 6000),
                target_count=integer(fields['TargetCount'], 0, 4096),
                target_count_curve=curve_data(fields['TargetCountWorldSizeMultiplier']),
                minimum_distance_same_type=integer(fields['MinDistanceSameType'], 0, 10000),
                minimum_distance_different_type=integer(fields['MinDistanceDifferentType'], 0, 10000),
                distance_same_type=integer(fields['DistanceSameType'], 0, 2),
                distance_different_type=integer(fields['DistanceDifferentType'], 0, 2),
                nexus_only=flag(fields['NexusOnly']))


def program_inventory(identity, fields, object_id, order, active):
    """Discover procedural components structurally, including unknown families.

    This records coverage, never certifies execution of a captured component.
    Selector values remain descriptive strings, never local filesystem paths.
    """
    if tuple(identity[:2]) != ('Assembly-CSharp', ''):
        return None
    legacy = BASE.issubset(fields)
    current = (CURRENT_BASE.issubset(fields)
               and not (BASE - CURRENT_BASE).intersection(fields))
    if not legacy and not current:
        return None
    selectors = {k: v for k, v in fields.items()
                 if ('folder' in k.lower() or k == 'Monuments')
                 and isinstance(v, (str, list))}
    row = dict(identity=list(identity), object_id=object_id, order=order,
                disabled=flag(fields['Disabled']) if legacy else None,
                mode=integer(fields['Mode'], -1, 3),
                hierarchy_active=flag(active), selectors=selectors,
                serialized_fields=sorted(k for k in fields if not k.startswith('m_')))
    if current:
        # Preserve the absent field. Synthesizing Disabled=false would imply the
        # legacy execution gate, which does not describe this serialized layout.
        row['serialized_base'] = 'mode-description'
        row['component_enabled'] = flag(fields['m_Enabled'])
    return row


def collect(root, inventory=None, *, inventory_only=False, capture_fields=False, stage_layout='legacy'):
    if stage_layout not in ('legacy', 'hierarchy'):
        raise ValueError('Unsupported monument stage activation layout')
    from UnityPy.classes.PPtr import PPtr
    from lazy_unity_bundle import load
    scene_name = 'BuildPlayer-Procedural Map'
    env, stream = load(root / 'Bundles/maps/maps.bundle',
                       nodes={scene_name}, follow_dependencies=True)
    try:
        scenes = [a for a in env.assets if a.name == scene_name]
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]

        identities = {}

        def identity(obj):
            script = obj.read().m_Script
            key = script.m_FileID, script.m_PathID
            if key not in identities:
                fields = script.read_typetree()
                identities[key] = fields['m_AssemblyName'], fields['m_Namespace'], fields['m_ClassName']
            return identities[key]

        setups = [o for o in scene.objects.values() if int(o.type) == 114
                  and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous procedural WorldSetup')

        def ptr(ref):
            if ref['m_FileID'] != 0:
                raise ValueError('External procedural hierarchy')
            return PPtr(**ref, assetsfile=scene).deref()

        stages, other, seen = [], [], set()
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
                if inventory is not None:
                    fields = record.read_typetree()
                    item = program_inventory(ident, fields, record.path_id, ordinal, active)
                    if item is not None:
                        if capture_fields:
                            item['fields'] = {k: v for k, v in fields.items() if not k.startswith('m_')}
                        inventory.append(item)
                if inventory_only:
                    ordinal += 1
                    continue
                if ident[2] == 'PlaceMonuments':
                    stages.append(stage_data(ident, record.read_typetree(), record.path_id, ordinal, active, layout=stage_layout))
                elif ident[2].startswith('PlaceMonuments'):
                    data = record.read_typetree()
                    other.append(dict(object_id=record.path_id, order=ordinal,
                                      kind=ident[2], folders=folders(data['ResourceFolder']),
                                      reason='Separate placement family; not a land-stage program'))
                ordinal += 1
            transforms = [o for o in records if int(o.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous procedural transform')
            for child in transforms[0].read_typetree()['m_Children']:
                walk(ptr(ptr(child).read_typetree()['m_GameObject']), active, depth+1)

        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if not inventory_only and (not 1 <= len(stages) <= 64 or len(other) > 64):
            raise ValueError('Unsupported monument stage roster')
        return stages, other
    finally:
        stream.close()
