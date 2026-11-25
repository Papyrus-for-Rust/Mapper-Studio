"""Exact referenced triangle buffers for local AddToHeightMap research/cache data.

No display LOD, mesh simplification, PhysX cooking or sampling is performed here.
"""
import math
from UnityPy.helpers.MeshHelper import MeshHandler
from extract_verified_prefabs import fingerprint
from lazy_unity_bundle import load


def geometry(vertices, triangles):
    if len(vertices) < 3 or not triangles or len(vertices) > 65536 or len(triangles) > 131072:
        raise ValueError('Contributor mesh exceeds geometry bounds')
    if (any(len(p) != 3 or any(type(v) not in (int, float) for v in p) for p in vertices)
            or any(len(t) != 3 or any(type(i) is not int for i in t) for t in triangles)):
        raise ValueError('Contributor mesh requires three coordinates and integer triangle indices')
    points = [[float(v) for v in p] for p in vertices]
    faces = [list(t) for t in triangles]
    if (any(len(p) != 3 or any(not math.isfinite(v) or abs(v) > 10000 for v in p) for p in points)
            or any(len(t) != 3 or min(t) < 0 or max(t) >= len(points) for t in faces)):
        raise ValueError('Invalid contributor mesh coordinates/indices')
    return dict(vertices=points, triangles=faces)


class MeshSources:
    def __init__(self, root, scene_environment, backfaces):
        if type(backfaces) is not bool:
            raise ValueError('Collider sampling profile must declare backface policy')
        self.root = root
        self.backfaces = backfaces
        self.environments = {'Bundles/shared/assetscenes.bundle': scene_environment}
        self.streams = []
        self.sources = {}
        self.meshes = {}

    def environment(self, relative):
        if relative not in self.environments:
            self.sources[relative] = fingerprint(self.root, relative, 8 * 1024**3)
            env, stream = load(self.root / relative)
            self.environments[relative] = env
            self.streams.append(stream)
        return self.environments[relative]

    def resolve(self, asset, ref):
        file_id, path_id = ref['m_FileID'], ref['m_PathID']
        if type(file_id) is not int or type(path_id) is not int or file_id < 0 or path_id == 0:
            raise ValueError('Invalid collider mesh reference')
        if file_id == 0:
            return asset, path_id
        if file_id > len(asset.externals):
            raise ValueError('Collider external reference outside table')
        path = asset.externals[file_id - 1].path.replace('\\', '/')
        if not path.startswith('archive:/'):
            raise ValueError('Unsupported collider external mesh source')
        name = path.rsplit('/', 1)[-1]
        for relative in ('Bundles/shared/assetscenes.bundle', 'Bundles/shared/content.bundle', 'Bundles/shared/monuments.bundle'):
            matches = [a for a in self.environment(relative).assets if a.name == name]
            if len(matches) == 1:
                return matches[0], path_id
        raise ValueError('Collider mesh external file unresolved')

    def collider(self, asset, objects):
        colliders = [o for o in objects if o.type.name.endswith('Collider')]
        if len(colliders) != 1 or colliders[0].type.name != 'MeshCollider':
            raise ValueError('Contributor requires an unambiguous MeshCollider adapter')
        fields = colliders[0].read_typetree()
        selected, path_id = self.resolve(asset, fields['m_Mesh'])
        key = (selected.name, path_id)
        if key not in self.meshes:
            reader = selected.objects[path_id]
            if int(reader.type) != 43 or reader.byte_size > 16 * 1024**2:
                raise ValueError('Invalid contributor mesh object')
            mesh = reader.read()
            if any(int(sub.topology) != 0 for sub in mesh.m_SubMeshes):
                raise ValueError('Contributor requires triangle topology')
            handler = MeshHandler(mesh)
            handler.process()
            self.meshes[key] = geometry(handler.m_Vertices or [], [t for sub in handler.get_triangles() for t in sub])
        return dict(kind='raw-triangle-mesh', enabled=bool(fields['m_Enabled']),
                    trigger=bool(fields['m_IsTrigger']), convex=bool(fields['m_Convex']),
                    cooking_options=fields['m_CookingOptions'], backfaces=self.backfaces,
                    **self.meshes[key])

    def verify(self):
        if any(fingerprint(self.root, path, 8 * 1024**3) != source for path, source in self.sources.items()):
            raise ValueError('Contributor mesh sources changed during extraction')

    def close(self):
        for stream in self.streams:
            stream.close()
