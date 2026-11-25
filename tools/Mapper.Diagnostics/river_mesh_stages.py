"""Versioned river stage activation and ordered fresh-world coverage."""
from monument_stages import BASE, CURRENT_BASE, flag, integer

KINDS = ('GenerateRiverLayout', 'GenerateRiverTopology', 'GenerateRiverTerrain',
         'GenerateRiverTexture', 'GenerateRiverMeshes')
RESOURCES = ('RiverMesh', 'RiverInteriorMesh', 'RiverInteriorFrontCapMesh',
             'RiverInteriorBackCapMesh', 'RiverMeshes', 'RiverMaterial', 'RiverPhysicMaterial')


def stage_data(identity, fields, object_id, order, active, *, layout='legacy'):
    if tuple(identity[:2]) != ('Assembly-CSharp', '') or identity[2] not in KINDS:
        raise ValueError('Unsupported river stage identity')
    if layout not in ('legacy', 'mode-description'):
        raise ValueError('Unsupported river stage activation adapter')
    base = BASE if layout == 'legacy' else CURRENT_BASE
    resources = set(RESOURCES) if identity[2] == 'GenerateRiverMeshes' else set()
    authored = {k for k in fields if not k.startswith('m_')}
    required = {'Mode', 'Disabled'} if layout == 'legacy' else CURRENT_BASE
    if authored - (base | resources) or not (required | resources) <= authored:
        raise ValueError('Unknown or incomplete river stage layout')
    return dict(kind=identity[2], object_id=integer(object_id, 1, 2**63-1),
                order=integer(order, 0, 65535), mode=integer(fields['Mode'], -1, 3),
                disabled=flag(fields.get('Disabled')) if layout == 'legacy' else None,
                hierarchy_active=flag(active), component_enabled=flag(fields.get('m_Enabled')))


def validate_sequence(stages):
    if [s['kind'] for s in stages] != list(KINDS):
        raise ValueError('Unsupported river generation stage sequence')
    if (len({s['object_id'] for s in stages}) != len(KINDS)
            or any(a['order'] >= b['order'] for a, b in zip(stages, stages[1:]))
            or any(s['disabled'] is not None or not s['hierarchy_active']
                   or s['mode'] & 1 == 0 for s in stages)):
        raise ValueError('Unsupported fresh-client river stage activation')
