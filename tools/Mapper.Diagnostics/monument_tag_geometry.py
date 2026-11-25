"""Capture monument phrases and tagged collider inputs in prefab coordinates."""
import math

from metadata_transforms import multiply
from monument_stages import flag, integer
from prefab_geometry import IDENTITY, matrix, vector

COLLIDERS = {64, 65, 135, 136, 154}
MONUMENT_SCRIPTS = {('Assembly-CSharp', '', 'MonumentInfo'),
                    ('Assembly-CSharp', '', 'PreventBuildingMonumentTag')}


def phrase_data(value):
    if not isinstance(value, dict) or set(value) not in (
            {'token', 'english'}, {'token', 'legacyEnglish'}):
        raise ValueError('Unknown monument phrase layout')
    for text in value.values():
        if text is not None and (not isinstance(text, str) or len(text.encode('utf-8')) > 4096 or '\0' in text):
            raise ValueError('Invalid monument phrase text')
    return dict(value)


def pointer_data(value, external_count):
    if not isinstance(value, dict) or set(value) != {'m_FileID', 'm_PathID'}:
        raise ValueError('Invalid monument tag reference')
    return dict(file_id=integer(value['m_FileID'], 0, external_count),
                path_id=integer(value['m_PathID'], -2**63, 2**63 - 1))


def collider_data(kind, fields, external_count=0):
    result = dict(class_id=kind, enabled=flag(fields['m_Enabled']),
                  trigger=flag(fields['m_IsTrigger']))
    if kind == 64:
        return dict(result, mesh_reference=pointer_data(fields['m_Mesh'], external_count),
                    convex=flag(fields['m_Convex']),
                    cooking_options=integer(fields['m_CookingOptions'], 0, 2**31 - 1),
                    runtime_collider_bounds_required=True)
    if kind not in (65, 135, 136):
        raise ValueError('Tagged collider requires a runtime-bounds adapter')
    result['center'] = vector(fields['m_Center'])
    if kind == 65:
        result['size'] = vector(fields['m_Size'])
        if any(value < 0 for value in result['size']):
            raise ValueError('Invalid tagged box size')
    else:
        radius = fields['m_Radius']
        if type(radius) not in (int, float) or not math.isfinite(radius) or not 0 <= radius <= 10000:
            raise ValueError('Invalid tagged collider radius')
        result['radius'] = radius
        if kind == 136:
            height = fields['m_Height']
            if type(height) not in (int, float) or not math.isfinite(height) or not 0 <= height <= 20000:
                raise ValueError('Invalid tagged capsule height')
            result.update(height=height, direction=integer(fields['m_Direction'], 0, 2))
    return result


def mesh_bounds(asset, reference, sources, cache):
    selected, path_id = sources.resolve(asset, reference)
    key = selected.name, path_id
    if key not in cache:
        reader = selected.objects[path_id]
        if int(reader.type) != 43 or reader.byte_size > 16 * 1024**2:
            raise ValueError('Invalid tagged collider mesh')
        bounds = reader.read_typetree()['m_LocalAABB']
        center, extent = vector(bounds['m_Center']), vector(bounds['m_Extent'])
        if any(value < 0 for value in extent):
            raise ValueError('Invalid tagged mesh bounds')
        if len(cache) >= 4096:
            raise ValueError('Excessive tagged mesh bounds roster')
        cache[key] = dict(kind='serialized_mesh_local_aabb', center=center, extent=extent,
                          asset_file=selected.name, object_id=path_id)
    return cache[key]


def component_identity(asset, obj, identify, cache):
    """Use the asset's serialized script table before reading a component body."""
    type_id = getattr(obj, 'type_id', None)
    if type_id is None:
        return None
    if type_id not in cache:
        serialized_type = obj.serialized_type
        index = serialized_type.script_type_index if serialized_type is not None else -1
        identity = None
        if 0 <= index < len(asset.script_types):
            script = asset.script_types[index]
            file_id = script.local_serialized_file_index
            path_id = script.local_identifier_in_file
            if file_id >= 0 and path_id != 0:
                try:
                    identity = identify(asset, {'m_Script': {'m_FileID': file_id, 'm_PathID': path_id}})
                except (AttributeError, ValueError, KeyError, TypeError, IndexError, FileNotFoundError):
                    pass
        cache[type_id] = identity
    return cache[type_id]


def capture(asset, root, identify, local_pointer, mesh_sources=None, mesh_cache=None, *, script_types=None):
    result = dict(space='prefab-root-local-unity', monuments=[], tags=[], issues=[], complete=True)
    seen = set()
    mesh_cache = {} if mesh_cache is None else mesh_cache
    script_types = {} if script_types is None else script_types

    def issue(oid, error):
        result['complete'] = False
        if len(result['issues']) >= 256:
            raise ValueError('Excessive monument tag issues')
        result['issues'].append(dict(object_id=oid, reason=str(error)[:256]))

    def walk(go, parent=IDENTITY, active=True, depth=0, rotations=(), child_transform=None):
        if depth > 128 or len(seen) >= 65536 or go.path_id in seen:
            raise ValueError('Invalid monument tag hierarchy')
        seen.add(go.path_id)
        fields = go.read_typetree()
        is_root = go.path_id == root.path_id
        local_active = flag(fields['m_IsActive'])
        active = active and (is_root or local_active)
        objects = [local_pointer(asset, item['component']) for item in fields['m_Component']]
        if len({obj.path_id for obj in objects}) != len(objects):
            raise ValueError('Duplicate monument tag component')
        transforms = [obj for obj in objects if int(obj.type) in (4, 224)]
        if len(transforms) != 1:
            raise ValueError('Ambiguous monument tag transform')
        transform_reader = transforms[0]
        transform = (child_transform[1] if child_transform is not None
                     and child_transform[0] is transform_reader else transform_reader.read_typetree())
        local = parent if is_root else multiply(parent, matrix(transform))
        if is_root:
            result.update(root_rotation=vector(transform['m_LocalRotation'], 'xyzw'),
                          root_scale=vector(transform['m_LocalScale']))
        chain = rotations if is_root else rotations + (vector(transform['m_LocalRotation'], 'xyzw'),)
        colliders = [obj for obj in objects if int(obj.type) in COLLIDERS]
        for obj in objects:
            if int(obj.type) != 114:
                continue
            # Older assets can omit the script index. Their bodies still carry the exact pointer.
            indexed = component_identity(asset, obj, identify, script_types)
            if indexed is not None and indexed not in MONUMENT_SCRIPTS:
                continue
            data = obj.read_typetree()
            identity = identify(asset, data)
            if identity not in MONUMENT_SCRIPTS:
                continue
            if local_pointer(asset, data['m_GameObject']).path_id != go.path_id:
                raise ValueError('Monument tag component ownership mismatch')
            common = dict(object_id=obj.path_id, root=is_root, hierarchy_active=active,
                          component_enabled=flag(data['m_Enabled']),
                          local_to_root=[list(row) for row in local], rotation_chain=list(chain))
            try:
                if identity[2] == 'MonumentInfo':
                    result['monuments'].append(dict(display_phrase=phrase_data(data['displayPhrase']), **common))
                else:
                    if {name for name in data if not name.startswith('m_')} != {'autoFindMonument', 'AttachedMonument'}:
                        raise ValueError('Unknown PreventBuildingMonumentTag fields')
                    tag = dict(auto_find_monument=flag(data['autoFindMonument']),
                               attached_monument=pointer_data(data['AttachedMonument'], len(asset.externals)),
                               colliders=[], collider_order='serialized_component_order', **common)
                    for collider in colliders:
                        entry = dict(object_id=collider.path_id, class_id=int(collider.type))
                        try:
                            values = collider.read_typetree()
                            if local_pointer(asset, values['m_GameObject']).path_id != go.path_id:
                                raise ValueError('Tagged collider ownership mismatch')
                            entry.update(collider_data(int(collider.type), values, len(asset.externals)))
                            if int(collider.type) == 64:
                                if mesh_sources is None:
                                    raise ValueError('Tagged mesh needs a referenced mesh-bounds source')
                                entry['mesh_bounds'] = mesh_bounds(asset, values['m_Mesh'], mesh_sources, mesh_cache)
                        except (ValueError, KeyError, TypeError, OverflowError) as error:
                            issue(collider.path_id, error)
                        tag['colliders'].append(entry)
                    result['tags'].append(tag)
                if len(result['monuments']) > 1024 or len(result['tags']) > 4096:
                    raise ValueError('Excessive monument tag records')
            except (ValueError, KeyError, TypeError, OverflowError) as error:
                issue(obj.path_id, error)
        for child in transform['m_Children']:
            reader = local_pointer(asset, child)
            fields = reader.read_typetree()
            walk(local_pointer(asset, fields['m_GameObject']), local, active, depth + 1, chain, (reader, fields))

    try:
        walk(root)
    except (ValueError, KeyError, TypeError, OverflowError) as error:
        issue(root.path_id, error)
    result['nodes_visited'] = len(seen)
    return result
