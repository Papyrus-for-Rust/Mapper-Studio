"""Numeric source capture for later coastal fitting; never executes prefab attributes."""
import math
from prefab_geometry import vector as transform_vector


def finite(value):
    if type(value) not in (int, float) or not math.isfinite(value) or abs(value) > 10000:
        raise ValueError('Invalid placement attribute number')
    return float(value)


def vector(value):
    return [finite(value[c]) for c in 'xyz']


def flag(value):
    if type(value) not in (int, bool) or value not in (0, 1):
        raise ValueError('Invalid placement attribute flag')
    return bool(value)


def attribute_fields(fields):
    """Separate the current serialized PrefabAttribute base from parameters.

    Capture uses the caller's hierarchy-derived world position, as PreProcess
    does before restoring authored local values. These base values are never
    substituted for that position. Zero rotation/scale defaults are serialized
    on unflattened roots and must remain valid metadata.
    """
    names = {'authoredLocalPosition', 'authoredLocalRotation', 'authoredLocalScale',
             'flattenedParent', 'flattenedLocalPosition', 'flattenedLocalRotation',
             'flattenedLocalScale'}
    present = names.intersection(fields)
    if not present:
        return fields
    if present != names:
        raise ValueError('Incomplete serialized PrefabAttribute transform layout')
    for name in names - {'flattenedParent'}:
        value = fields[name]
        components = 'xyzw' if name.endswith('Rotation') else 'xyz'
        if not isinstance(value, dict) or set(value) != set(components):
            raise ValueError('Invalid serialized PrefabAttribute transform vector')
        for component in components:
            finite(value[component])
    parent = fields['flattenedParent']
    if (not isinstance(parent, dict) or set(parent) != {'m_FileID', 'm_PathID'}
            or type(parent['m_FileID']) is not int or not 0 <= parent['m_FileID'] < 2**31
            or type(parent['m_PathID']) is not int or abs(parent['m_PathID']) >= 2**63
            or parent['m_PathID'] == 0 and parent['m_FileID'] != 0):
        raise ValueError('Invalid serialized PrefabAttribute parent reference')
    return {name: value for name, value in fields.items() if name not in names}


def environment_object(object_id, layer, volume_components, collider_enabled):
    if (type(object_id) is not int or object_id == 0 or abs(object_id) >= 2**63
            or type(layer) is not int or not 0 <= layer <= 31
            or len(volume_components) > 64
            or any(type(v) is not int or v == 0 or abs(v) >= 2**63 for v in volume_components)
            or len(set(volume_components)) != len(volume_components)
            or len(collider_enabled) > 16):
        raise ValueError('Invalid environment GameObject facts')
    return dict(object_id=object_id, layer=layer, volume_components=list(volume_components),
                collider_enabled=[flag(v) for v in collider_enabled])


def collider_mesh(object_id, fields, asset_name, mesh_id, vertices, triangles):
    """Preserve raw local collision geometry; never simplify or cook a hull."""
    if (type(object_id) is not int or object_id == 0 or abs(object_id) >= 2**63
            or type(mesh_id) is not int or mesh_id == 0 or abs(mesh_id) >= 2**63
            or not isinstance(asset_name, str) or not asset_name or len(asset_name) > 256
            or any(c in asset_name for c in '\0/\\')):
        raise ValueError('Invalid environment collider mesh identity')
    cooking = fields['m_CookingOptions']
    if type(cooking) is not int or not 0 <= cooking <= 2**32-1:
        raise ValueError('Invalid environment collider cooking options')
    if not 3 <= len(vertices) <= 65536 or not 1 <= len(triangles) <= 131072:
        raise ValueError('Environment collider mesh exceeds geometry bounds')
    if (any(len(p) != 3 or any(type(v) not in (int, float) for v in p) for p in vertices)
            or any(len(t) != 3 or any(type(i) is not int for i in t) or min(t) < 0 or max(t) >= len(vertices) for t in triangles)):
        raise ValueError('Invalid environment collider mesh coordinates or indices')
    return dict(collider_id=object_id, mesh_asset=asset_name, mesh_id=mesh_id,
                enabled=flag(fields['m_Enabled']), is_trigger=flag(fields['m_IsTrigger']),
                convex=flag(fields['m_Convex']), cooking_options=cooking,
                vertices=[[finite(v) for v in p] for p in vertices],
                triangles=[list(t) for t in triangles])


def root_transform(fields, output):
    parent = fields['m_Father']
    if type(parent.get('m_PathID')) is not int or parent['m_PathID'] != 0:
        raise ValueError('Parented prefab root needs an environment frame adapter')
    position = [finite(v) for v in transform_vector(fields['m_LocalPosition'])]
    rotation = transform_vector(fields['m_LocalRotation'], 'xyzw')
    scale = transform_vector(fields['m_LocalScale'])
    if abs(sum(v*v for v in rotation)-1) > .001 or any(not 1e-6 <= abs(v) <= 10000 for v in scale):
        raise ValueError('Invalid environment prefab root transform')
    output['environment']['root_transform'] = dict(position=position, rotation=rotation, scale=scale)


def capture(identity, fields, position, root, object_id, active, output, *, transform=None, collider_types=(), collider_ids=(), rotation_chain=(), game_object=None):
    if identity[:2] != ('Assembly-CSharp', ''):
        return
    kind = identity[2]
    if kind == 'WaterBody':
        water = output['environment']['water_placement']
        try:
            body_type = fields['Type']
            if (type(object_id) is not int or object_id == 0 or abs(object_id) >= 2**63
                    or type(body_type) is not int or not -(2**31) <= body_type < 2**31
                    or len(water['bodies']) >= 1024):
                raise ValueError('Invalid water placement identity or type')
            refs = fields['Triggers']
            if not isinstance(refs, list) or len(refs) > 1024:
                raise ValueError('Water trigger references exceed allowance')
            triggers = []
            for ref in refs:
                if (not isinstance(ref, dict) or type(ref.get('m_FileID')) is not int
                        or not 0 <= ref['m_FileID'] < 2**31
                        or type(ref.get('m_PathID')) is not int or abs(ref['m_PathID']) >= 2**63
                        or (ref['m_PathID'] == 0 and ref['m_FileID'] != 0)):
                    raise ValueError('Invalid water trigger reference')
                triggers.append(dict(file_id=ref['m_FileID'], path_id=ref['m_PathID']))
            if sum(len(b['triggers']) for b in water['bodies']) + len(triggers) > 4096:
                raise ValueError('Water trigger reference total exceeds allowance')
            if len(position) != 3:
                raise ValueError('Invalid water body local position')
            water['bodies'].append(dict(object_id=object_id, body_type=body_type,
                component_enabled=flag(fields['m_Enabled']), hierarchy_active_without_root=flag(active),
                position=[finite(v) for v in position], triggers=triggers))
        except (ValueError, KeyError, TypeError) as error:
            if len(water['issues']) >= 256:
                raise ValueError('Water placement issue allowance exceeded')
            water['issues'].append(str(error)[:160])
        return
    if kind in ('EnvironmentVolume', 'EnvironmentVolumeCheck'):
        try:
            if transform is None or len(transform) != 4 or any(len(row) != 4 for row in transform):
                raise ValueError('Missing environment hierarchy transform')
            matrix = [[finite(v) for v in row] for row in transform]
            if matrix[3] != [0.,0.,0.,1.]:
                raise ValueError('Non-affine environment transform')
            mask = fields['Type']
            if type(mask) is not int or not -(2**31) <= mask < 2**31:
                raise ValueError('Invalid environment type mask')
            size = vector(fields['Size'])
            if any(v <= 0 for v in size):
                raise ValueError('Invalid environment volume size')
            names = sorted(k for k in fields if not k.startswith('m_'))
            if len(names) > 256 or any(not k or len(k) > 256 or '\0' in k for k in names):
                raise ValueError('Invalid environment field inventory')
            if kind == 'EnvironmentVolumeCheck' and names != ['Center','Size','Type']:
                raise ValueError('Unknown environment check layout')
            if len(collider_types) > 16 or any(type(v) is not int or v not in (64,65,135,136,154) for v in collider_types):
                raise ValueError('Invalid environment collider inventory')
            if len(collider_ids) != len(collider_types) or any(type(v) is not int or v == 0 or abs(v)>=2**63 for v in collider_ids) or len(set(collider_ids))!=len(collider_ids):
                raise ValueError('Invalid environment collider references')
            if len(rotation_chain)>128 or any(len(q)!=4 or abs(sum(finite(v)**2 for v in q)-1)>.001 for q in rotation_chain):
                raise ValueError('Invalid authored environment rotation chain')
            row = dict(object_id=object_id, hierarchy_active=bool(active),
                       component_enabled=flag(fields.get('m_Enabled',1)), type_mask=mask,
                       center=vector(fields['Center']), size=size, local_to_root=matrix,
                       collider_types=list(collider_types), collider_ids=list(collider_ids),
                       rotation_chain=[list(q) for q in rotation_chain], field_names=names)
            if game_object is not None:
                facts = environment_object(**game_object)
                if (len(facts['collider_enabled']) != len(collider_ids)
                        or kind == 'EnvironmentVolume' and object_id not in facts['volume_components']):
                    raise ValueError('Environment GameObject component associations differ')
                row['game_object'] = facts
            target = 'volumes' if kind == 'EnvironmentVolume' else 'checks'
            if len(output['environment'][target]) >= 4096:
                raise ValueError('Environment component count exceeds allowance')
            output['environment'][target].append(row)
        except (ValueError,KeyError,TypeError) as error:
            output['environment']['issues'].append(str(error)[:160])
        return
    known = {
        'DecorSwim': set(), 'DecorAlign': {'NormalAlignment','GradientAlignment','SlopeOffset','SlopeScale'},
        'DecorTransform': {'Position','Rotation','Scale'}, 'DecorScale': {'MinScale','MaxScale'},
        'DecorRotate': {'MinRotation','MaxRotation'}, 'TerrainCheck': {'Rotate','Extents'},
        'WaterCheck': {'Rotate'}, 'TerrainPathConnect': {'Type'},
        'TerrainPlacementBlocked': {'Radius','Fade','Opacity'},
    }
    if (root and kind.startswith('Decor') or kind in ('TerrainCheck','WaterCheck','TerrainPathConnect','TerrainPlacementBlocked')) and kind in known:
        # TerrainPathConnect derives from MonoBehaviour, not PrefabAttribute.
        if kind != 'TerrainPathConnect':
            fields = attribute_fields(fields)
        unexpected = sorted(k for k in fields if not k.startswith('m_') and k not in known[kind])
        if unexpected:
            raise ValueError('Unhandled placement fields for '+kind+': '+','.join(unexpected))
    row = dict(object_id=object_id, active=bool(active))
    if root and kind.startswith('Decor'):
        row['kind'] = kind
        if kind == 'DecorSwim':
            row['values'] = []
        elif kind == 'DecorAlign':
            row['values'] = [finite(fields[k]) for k in ('NormalAlignment', 'GradientAlignment')] + vector(fields['SlopeOffset']) + vector(fields['SlopeScale'])
        elif kind == 'DecorTransform':
            row['values'] = vector(fields['Position']) + vector(fields['Rotation']) + vector(fields['Scale'])
        elif kind == 'DecorScale':
            row['values'] = vector(fields['MinScale']) + vector(fields['MaxScale'])
        elif kind == 'DecorRotate':
            row['values'] = vector(fields['MinRotation']) + vector(fields['MaxRotation'])
        else:
            row['values'] = []
            output['issues'].append('Unsupported root decorator: ' + kind)
        output['root_decor'].append(row)
    elif kind == 'TerrainPlacementBlocked':
        radius,fade,opacity=(finite(fields[k]) for k in ('Radius','Fade','Opacity'))
        if not 0<=radius<=2048 or not 0<=fade<=2048 or not 0<=opacity<=1:
            raise ValueError('Invalid monument placement blocker')
        row.update(position=[finite(v) for v in position],radius=radius,fade=fade,opacity=opacity)
        output['placement_blockers'].append(row)
    elif kind in ('TerrainCheck', 'WaterCheck', 'TerrainPathConnect'):
        row['position'] = [finite(v) for v in position]
        if kind == 'TerrainPathConnect':
            value = fields['Type']
            if type(value) is not int or not 0 <= value <= 64:
                raise ValueError('Invalid infrastructure type')
            row['infrastructure_type'] = value
            output['path_connectors'].append(row)
        else:
            row['rotate'] = flag(fields['Rotate'])
            if kind == 'TerrainCheck':
                row['extents'] = finite(fields['Extents'])
                if row['extents'] < 0:
                    raise ValueError('Negative terrain-check extents')
                output['terrain_checks'].append(row)
            else:
                output['water_checks'].append(row)


def empty():
    return dict(version=3, root_decor=[], terrain_checks=[], water_checks=[], path_connectors=[], placement_blockers=[], issues=[],
                environment=dict(root_transform=None, volumes=[], checks=[], issues=[],
                    water_placement=dict(version=1, bodies=[], issues=[])))
