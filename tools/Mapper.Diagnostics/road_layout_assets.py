"""Ordered road-layout sockets and exact root MonumentInfo fields.

The monument enum remains opaque until matched to a selected-code role map.
Legacy infrastructure's object-ID-sorted connector representation is untouched.
"""
import math
from prefab_paths import inspect_ordered, read_path_object as read_layout_object
from prefab_geometry import reference


def inspect_layout(root_id, read, script_type):
    monument_components = set()
    def identity(oid):
        value = script_type(oid)
        if value == ('Assembly-CSharp', '', 'MonumentInfo'):
            monument_components.add(oid)
        return value
    result = inspect_ordered(root_id, read, identity)
    result['version'] = 1
    result['monument'] = None

    def issue(oid, error):
        result['complete'] = False
        if len(result['issues']) < 128:
            result['issues'].append(dict(objectId=oid, reason=str(error)[:256]))

    def vector(value, nonnegative=False):
        out = []
        for axis in 'xyz':
            number = value[axis]
            if type(number) not in (int, float) or not math.isfinite(number) or abs(number) > 10000 or (nonnegative and number < 0):
                raise ValueError('Unsupported root MonumentInfo bounds')
            out.append(float(number))
        return out

    try:
        kind, root = read(root_id)
        if kind != 1:
            raise ValueError('Layout root is not a GameObject')
        components = root['m_Component']
        if not isinstance(components, list) or len(components) > 256:
            raise ValueError('Invalid layout root components')
        for item in components:
            oid = reference(item['component'])
            kind, fields = read(oid)
            if kind != 114 or identity(oid) != ('Assembly-CSharp', '', 'MonumentInfo'):
                continue
            if result['monument'] is not None:
                raise ValueError('Multiple root MonumentInfo components')
            if reference(fields['m_GameObject']) != root_id:
                raise ValueError('Root MonumentInfo ownership mismatch')
            value = fields['Type']
            if type(value) is not int or not 0 <= value <= 1024:
                raise ValueError('Missing bounded MonumentInfo enum')
            bounds = fields['Bounds']
            result['monument'] = dict(objectId=oid, monumentType=value,
                boundsCenter=vector(bounds['m_Center']), boundsExtent=vector(bounds['m_Extent'], True))
        if (result['monument'] is None and monument_components) or (
            result['monument'] is not None and monument_components != {result['monument']['objectId']}):
            raise ValueError('Child MonumentInfo requires a separate instance-transform adapter')
    except (ValueError, KeyError, TypeError, OverflowError, FileNotFoundError) as error:
        issue(root_id, error)
    return result
