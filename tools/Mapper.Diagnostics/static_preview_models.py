"""Bounded static renderer selection for local untextured preview LODs."""
from UnityPy.helpers.MeshHelper import MeshHandler
from model_preview_geometry import simplify


def renderer_selection(masters, groups):
    """Choose outermost HLOD replacements, else the last nonempty authored LOD."""
    master_ids = {identifier for depth, identifier in masters if depth == min(d for d, _ in masters)} if masters else set()
    all_lods = {identifier for choices in groups for choice in choices for identifier in choice}
    selected = {identifier for choices in groups for identifier in next((c for c in reversed(choices) if c), [])}
    return master_ids, all_lods, selected


def extract(nodes, resolve, asset, budget=256):
    model = dict(vertices=[], triangles=[], issues=[], complete=False, simplified=False)
    masters = []; groups = []
    for records, _, active, depth in nodes:
        if not active: continue
        for record in records:
            if int(record.type) == 137:
                model['issues'].append('Skinned renderer requires another adapter')
            elif int(record.type) == 205:
                fields = record.read_typetree()
                groups.append([[r.get('renderer', r)['m_PathID'] for r in lod.get('renderers', [])]
                               for lod in fields.get('m_LODs', [])])
            elif int(record.type) == 114:
                fields = record.read_typetree()
                if {'ReplacementMesh', 'ChildComponents', 'MeshBounds'}.issubset(fields):
                    ref = fields['ReplacementMesh']
                    if ref.get('m_FileID') == 0 and ref.get('m_PathID'):
                        masters.append((depth, ref['m_PathID']))
    master_ids, all_lods, selected = renderer_selection(masters, groups)
    for records, transform, active, _ in nodes:
        if not active: continue
        renderers = [r for r in records if int(r.type) == 23 and
                     (r.path_id in master_ids if master_ids else r.read_typetree().get('m_Enabled', False))]
        if not renderers or not any(r.path_id not in all_lods or r.path_id in selected for r in renderers): continue
        filters = [r for r in records if int(r.type) == 33]
        if len(filters) != 1:
            model['issues'].append('Renderer has no unambiguous MeshFilter'); continue
        try:
            ref = filters[0].read_typetree()['m_Mesh']
            if not master_ids:
                for record in records:
                    if int(record.type) != 114: continue
                    states = record.read_typetree().get('States', [])
                    if states and isinstance(states, list) and all(isinstance(s, dict) and 'distance' in s and 'mesh' in s for s in states):
                        ref = next((s['mesh'] for s in reversed(states) if s['mesh'].get('m_PathID')), ref)
            reader = resolve(asset, ref)
            if int(reader.type) != 43 or reader.byte_size > 16 * 1024**2:
                raise ValueError('Mesh object exceeds preview bounds')
            mesh = reader.read()
            if any(int(sub.topology) != 0 for sub in mesh.m_SubMeshes):
                raise ValueError('Non-triangle mesh topology')
            handler = MeshHandler(mesh); handler.process()
            vertices = handler.m_Vertices or []
            triangles = [t for group in handler.get_triangles() for t in group]
            if len(vertices) > 600000 or len(triangles) > 200000:
                raise ValueError('Raw model mesh exceeds preview bounds: '+str((len(vertices),len(triangles))))
            # The visual budget exceeds the collision adapter's per-mesh cap.
            import math
            if any(len(v) != 3 or any(not math.isfinite(x) or abs(x) > 10000 for x in v) for v in vertices):
                raise ValueError('Invalid static model vertex')
            if any(len(t) != 3 or any(type(i) is not int or i < 0 or i >= len(vertices) for i in t) for t in triangles):
                raise ValueError('Invalid static model triangle')
            # Keep Python's float summation and the source axis order.
            rows = transform[:3]
            transformed = [[sum((a * x, b * y, c * z)) + offset for a, b, c, offset in rows]
                           for x, y, z in vertices]
            if any(not math.isfinite(x) or abs(x) > 10000 for v in transformed for x in v):
                raise ValueError('Invalid transformed model vertex')
            count_before = len(triangles)
            transformed, triangles = simplify(transformed, triangles, budget)
            model['simplified'] |= len(triangles) < count_before
            if len(model['vertices']) + len(transformed) > 600000 or len(model['triangles']) + len(triangles) > 200000:
                raise ValueError('Combined model LOD exceeds preview bounds')
            base = len(model['vertices'])
            model['vertices'].extend(transformed)
            model['triangles'].extend([[base+i for i in t] for t in triangles])
        except (ValueError, KeyError, AttributeError, TypeError, IndexError, NotImplementedError) as error:
            model['issues'].append(str(error)[:160])
    before = len(model['triangles'])
    model['vertices'], model['triangles'] = simplify(model['vertices'], model['triangles'], budget)
    model['simplified'] |= len(model['triangles']) < before
    model['issues'] = sorted(set(model['issues']))[:256]
    model['complete'] = bool(model['triangles']) and not model['issues']
    return model
