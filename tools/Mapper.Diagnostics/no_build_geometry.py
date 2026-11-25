"""Inspect authored Prevent Building colliders, without loading render meshes."""
import math
from prefab_geometry import IDENTITY, MAX_DEPTH, MAX_NODES, MAX_OBJECTS, reference, matrix, multiply, point, vector, box


def layer_index(path):
    import UnityPy
    env = UnityPy.load(str(path))
    managers = [obj for obj in env.objects if int(obj.type) == 78]
    if len(managers) != 1:
        raise ValueError('Installation needs one TagManager')
    layers = managers[0].read_typetree()['layers']
    if not isinstance(layers, list) or len(layers) != 32:
        raise ValueError('Unsupported layer table')
    matches = [i for i, name in enumerate(layers) if str(name).lower().replace(' ', '').replace('_', '') == 'preventbuilding']
    if len(matches) != 1:
        raise ValueError('Prevent Building layer is missing or ambiguous')
    return matches[0]


class Meshes:
    """Resolve only requested collider meshes; never enumerate texture pixels."""
    def __init__(self, root):
        self.root=root;self.assets={};self.streams=[];self.buffers={};self.vertices=0;self.faces=0

    def close(self):
        for stream in self.streams:stream.close()

    def decode(self, asset, fields, transform):
        from lazy_unity_bundle import load
        from UnityPy.helpers.MeshHelper import MeshHandler
        from decor_collider_meshes import geometry
        ref=fields['m_Mesh'];file,path_id=ref['m_FileID'],ref['m_PathID']
        if type(file) is not int or not 0<=file<=len(asset.externals) or type(path_id) is not int or path_id==0:
            raise ValueError('Invalid restriction mesh reference')
        selected=asset
        if file:
            external=asset.externals[file-1].path.replace('\\','/')
            if not external.startswith('archive:/'):raise ValueError('Unsupported restriction mesh reference')
            name=external.rsplit('/',1)[-1]
            if name not in self.assets:
                for relative in ('assetscenes','content','monuments'):
                    path=self.root/'Bundles/shared'/f'{relative}.bundle'
                    if not path.is_file():continue
                    try:env,stream=load(path,nodes={name})
                    except ValueError as error:
                        if str(error).startswith('Requested UnityFS nodes are missing:'):continue
                        raise
                    self.streams.append(stream)
                    matches=[a for a in env.assets if a.name==name]
                    if len(matches)!=1:raise ValueError('Ambiguous restriction mesh asset')
                    self.assets[name]=matches[0];break
                if name not in self.assets:raise ValueError('Missing restriction mesh asset')
            selected=self.assets[name]
        key=(selected.name,path_id)
        if key not in self.buffers:
            reader=selected.objects[path_id]
            if int(reader.type)!=43 or reader.byte_size>16*1024**2:raise ValueError('Invalid restriction mesh object')
            mesh=reader.read()
            if any(int(s.topology)!=0 for s in mesh.m_SubMeshes):raise ValueError('Restriction mesh needs triangles')
            handler=MeshHandler(mesh);handler.process()
            data=geometry(handler.m_Vertices or [],[t for sub in handler.get_triangles() for t in sub])
            self.vertices+=len(data['vertices']);self.faces+=len(data['triangles'])
            if self.vertices>262144 or self.faces>524288:raise ValueError('Restriction mesh buffer allowance exceeded')
            self.buffers[key]=data
        data=self.buffers[key]
        if type(fields['m_Convex']) not in (bool,int) or fields['m_Convex'] not in (False,True,0,1):raise ValueError('Invalid convex flag')
        return dict(shape='mesh',convex=bool(fields['m_Convex']),vertices=[point(transform,v) for v in data['vertices']],triangles=data['triangles'])


def inspect(root_id, read, layer, mesh=None, *, class_id=None):
    result = dict(version=2, space='prefab-root-local-unity', layerIndex=layer,
                  nodesVisited=0, volumes=[], issues=[], complete=True)
    cache = {}

    def get(oid):
        if oid not in cache:
            if len(cache) >= MAX_OBJECTS:
                raise ValueError('Object allowance exceeded')
            cache[oid] = read(oid)
        return cache[oid]

    def object_type(oid):
        return get(oid)[0] if class_id is None else class_id(oid)

    def flag(value):
        if type(value) not in (bool, int) or value not in (0, 1):
            raise ValueError('Unknown active state')
        return bool(value)

    def issue(oid, reason):
        result['complete'] = False
        if len(result['issues']) < 256:
            result['issues'].append(dict(objectId=oid, reason=str(reason)[:256]))

    def components(go):
        ids = go['m_Component']
        if not isinstance(ids, list) or len(ids) > 256:
            raise ValueError('Component allowance exceeded')
        return [reference(v['component']) for v in ids]

    def geometry(kind, fields, transform):
        if kind == 65:
            shape = box(transform, fields)
            axes = shape['halfAxes']
            for a in range(3):
                for b in range(a):
                    if abs(sum(x*y for x, y in zip(axes[a], axes[b]))) > math.sqrt(sum(x*x for x in axes[a])*sum(x*x for x in axes[b]))*1e-6:
                        raise ValueError('Sheared box needs a physics-shape adapter')
            return shape
        center = point(transform, vector(fields['m_Center']))
        scales = [math.sqrt(sum(transform[i][j]**2 for i in range(3))) for j in range(3)]
        radius = float(fields['m_Radius'])
        if not math.isfinite(radius) or not 0 < radius <= 10000 or min(scales) < 1e-8:
            raise ValueError('Invalid collider radius or scale')
        if kind == 135:
            return dict(shape='sphere', center=center, radius=radius*max(scales))
        axis = fields['m_Direction']
        height = float(fields['m_Height'])
        if type(axis) is not int or axis not in (0, 1, 2) or not math.isfinite(height) or not 0 <= height <= 20000:
            raise ValueError('Invalid capsule axis or height')
        radius *= max(scales[j] for j in range(3) if j != axis)
        half = max(0., height*scales[axis]*.5-radius)
        direction = [transform[i][axis]/scales[axis] for i in range(3)]
        return dict(shape='capsule', start=[c-d*half for c, d in zip(center, direction)],
                    end=[c+d*half for c, d in zip(center, direction)], radius=radius)

    try:
        if type(layer) is not int or not 0 <= layer < 32:
            raise ValueError('No verified installation layer')
        kind, root = get(root_id)
        if kind != 1:
            raise ValueError('Root is not a GameObject')
        roots = [oid for oid in components(root) if object_type(oid) in (4, 224)]
        if len(roots) != 1:
            raise ValueError('Root needs one Transform')
        root_transform = get(roots[0])[1]
        if any(abs(v-1.) > 1e-6 for v in vector(root_transform['m_LocalScale'])):
            raise ValueError('Non-unit prefab root scale needs an instance adapter')
        seen = set()
        stack = [(roots[0], IDENTITY, True, 0)]
        while stack:
            oid, parent, active, depth = stack.pop()
            if oid in seen or len(seen) >= MAX_NODES or depth > MAX_DEPTH:
                raise ValueError('Shared/cyclic or excessive hierarchy')
            seen.add(oid)
            kind, transform = get(oid)
            if kind not in (4, 224):
                raise ValueError('Child is not a Transform')
            go_id = reference(transform['m_GameObject'])
            kind, go = get(go_id)
            if kind != 1:
                raise ValueError('Transform has no GameObject')
            # Placed prefabs activate their stored inactive root. Child states
            # are preserved, and disabled colliders never become active volumes.
            state = flag(go['m_IsActive'])
            active = active and (depth == 0 or state)
            world = multiply(parent, IDENTITY if depth == 0 else matrix(transform))
            result['nodesVisited'] += 1
            if go['m_Layer'] == layer:
                for cid in components(go):
                    ckind = object_type(cid)
                    if ckind not in (64, 65, 135, 136, 154):
                        continue
                    _, fields = get(cid)
                    try:
                        if reference(fields['m_GameObject']) != go_id:
                            raise ValueError('Collider ownership mismatch')
                        if ckind == 64 and mesh is not None:
                            shape = mesh(fields, world)
                        elif ckind in (65,135,136):
                            shape = geometry(ckind, fields, world)
                        else:
                            raise ValueError('Unsupported no-build collider; no radius approximation used')
                        if len(result['volumes']) >= 1024:
                            raise ValueError('Volume allowance exceeded')
                        result['volumes'].append(dict(objectId=cid, active=active and flag(fields['m_Enabled']), geometry=shape))
                    except (KeyError, ValueError, TypeError, OverflowError) as error:
                        issue(cid, error)
            children = transform['m_Children']
            if not isinstance(children, list) or len(children) > MAX_NODES:
                raise ValueError('Child allowance exceeded')
            for child in reversed(children):
                stack.append((reference(child), world, active, depth+1))
    except (KeyError, ValueError, TypeError, OverflowError) as error:
        issue(root_id, error)
    return result
