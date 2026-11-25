"""Strict numeric offshore-stage capture; does not execute placement."""
from monument_stages import BASE, CURRENT_BASE, flag, folders, integer

FIELDS = BASE | {'ResourceFolder', 'TargetCount', 'MinDistanceFromTerrain',
                 'MaxDistanceFromTerrain', 'DistanceBetweenMonuments', 'MinWorldSize'}
CURRENT_FIELDS = (FIELDS - BASE) | CURRENT_BASE


def stage_data(row, *, layout='legacy'):
    if row['identity'] != ['Assembly-CSharp', '', 'PlaceMonumentsOffshore']:
        raise ValueError('Unsupported offshore stage identity')
    fields = row['fields']
    if layout not in ('legacy', 'hierarchy'):
        raise ValueError('Unsupported offshore activation adapter')
    if set(fields) != (FIELDS if layout == 'legacy' else CURRENT_FIELDS):
        raise ValueError('Unknown offshore stage layout; requires an adapter')
    if layout == 'hierarchy' and row.get('serialized_base') != 'mode-description':
        raise ValueError('Offshore serialized base differs from activation adapter')
    minimum = integer(fields['MinDistanceFromTerrain'], 0, 4096)
    maximum = integer(fields['MaxDistanceFromTerrain'], 1, 4096)
    if minimum >= maximum:
        raise ValueError('Invalid offshore search band')
    result = dict(object_id=integer(row['object_id'], 1, 2**63-1),
                order=integer(row['order'], 0, 65535),
                mode=integer(fields['Mode'], -1, 3),
                disabled=flag(fields['Disabled']) if layout == 'legacy' else None,
                folders=folders(fields['ResourceFolder']),
                target_count=integer(fields['TargetCount'], 0, 128),
                minimum_world_size=integer(fields['MinWorldSize'], 0, 6000),
                minimum_distance=minimum, maximum_distance=maximum,
                spacing=integer(fields['DistanceBetweenMonuments'], 0, 10000))
    if layout == 'hierarchy':
        result.update(hierarchy_active=flag(row.get('hierarchy_active')),
                      component_enabled=flag(row.get('component_enabled')))
    return result
