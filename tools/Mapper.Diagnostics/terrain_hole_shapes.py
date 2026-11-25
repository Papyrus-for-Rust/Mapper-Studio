"""Capture referenced terrain-hole meshes and their authored instance transforms."""
import math

from decor_collider_meshes import MeshSources, geometry
from extract_verified_prefabs import fingerprint
from monument_stages import flag, integer
from prefab_geometry import IDENTITY, matrix, multiply


def instance_data(fields):
    if {name for name in fields if not name.startswith('m_')} != {'radius', 'asset'}:
        raise ValueError('Unknown TerrainHoleShape fields')
    radius = fields['radius']
    if type(radius) not in (int, float) or not math.isfinite(radius) or not 0 < radius <= 10000:
        raise ValueError('Invalid terrain-hole radius')
    return dict(radius=radius, enabled=flag(fields['m_Enabled']))


def asset_data(fields):
    if {name for name in fields if not name.startswith('m_')} != {'ID', 'startInstanceCount', 'meshAsset'}:
        raise ValueError('Unknown HoleShapeAsset fields')
    name = fields.get('m_Name')
    if not isinstance(name, str) or len(name.encode('utf-8')) > 1024 or '\0' in name:
        raise ValueError('Invalid terrain-hole asset name')
    return dict(name=name, id=integer(fields['ID'], -2**31, 2**31 - 1),
                start_instance_count=integer(fields['startInstanceCount'], 0, 1048576))


class HoleShapeSources(MeshSources):
    """Resolve only referenced serialized nodes, reusing the mesh receipt guard."""
    def __init__(self, root, environment, backfaces):
        super().__init__(root, environment, backfaces)
        self.missing_nodes = set()
        self.hole_assets = {}
        self.nodes = {asset.name: asset for asset in environment.assets}
        if len(self.nodes) != len(environment.assets):
            raise ValueError('Ambiguous terrain-hole source nodes')

    def resolve(self, asset, reference):
        from lazy_unity_bundle import load

        if (not isinstance(reference, dict) or set(reference) != {'m_FileID', 'm_PathID'}
                or type(reference['m_FileID']) is not int
                or not 0 <= reference['m_FileID'] <= len(asset.externals)
                or type(reference['m_PathID']) is not int
                or not -2**63 <= reference['m_PathID'] < 2**63
                or reference['m_PathID'] == 0):
            raise ValueError('Invalid terrain-hole object reference')
        file_id, path_id = reference['m_FileID'], reference['m_PathID']
        if file_id == 0:
            if path_id not in asset.objects:
                raise ValueError('Missing local terrain-hole object')
            return asset, path_id
        external = asset.externals[file_id - 1].path.replace('\\', '/')
        if not external.startswith('archive:/'):
            raise ValueError('Unsupported terrain-hole external reference')
        node = external.rsplit('/', 1)[-1]
        if not node or len(node) > 512 or '\0' in node or node in ('.', '..'):
            raise ValueError('Invalid terrain-hole serialized node')
        if node not in self.nodes:
            for relative in ('Bundles/shared/assetscenes.bundle', 'Bundles/shared/content.bundle',
                             'Bundles/shared/monuments.bundle'):
                key = (relative, node)
                if key in self.missing_nodes or not (self.root / relative).is_file():
                    continue
                if relative != 'Bundles/shared/assetscenes.bundle' and relative not in self.sources:
                    self.sources[relative] = fingerprint(self.root, relative, 8 * 1024**3)
                try:
                    env, stream = load(self.root / relative, nodes={node})
                except ValueError as error:
                    if not str(error).startswith('Requested UnityFS nodes are missing:'):
                        raise
                    self.missing_nodes.add(key)
                    continue
                self.streams.append(stream)
                self.environments[key] = env
                if len(self.nodes) + len(env.assets) > 65536:
                    raise ValueError('Excessive terrain-hole serialized nodes')
                for item in env.assets:
                    self.nodes.setdefault(item.name, item)
                break
        if node not in self.nodes:
            raise ValueError('Missing terrain-hole serialized node')
        selected = self.nodes[node]
        if path_id not in selected.objects:
            raise ValueError('Missing referenced terrain-hole object')
        return selected, path_id

    def hole_asset(self, asset, reference):
        from UnityPy.helpers.MeshHelper import MeshHandler

        selected, path_id = self.resolve(asset, reference)
        key = (selected.name, path_id)
        if key in self.hole_assets:
            return self.hole_assets[key]
        reader = selected.objects[path_id]
        if int(reader.type) != 114 or reader.byte_size > 1024**2:
            raise ValueError('Terrain-hole reference requires an exact HoleShapeAsset')
        fields = reader.read_typetree()
        script_file, script_id = self.resolve(selected, fields['m_Script'])
        script = script_file.objects[script_id]
        if int(script.type) != 115 or script.byte_size > 1024**2:
            raise ValueError('Invalid terrain-hole asset script')
        script_fields = script.read_typetree()
        identity = tuple(script_fields.get(name) for name in ('m_AssemblyName', 'm_Namespace', 'm_ClassName'))
        if identity != ('Assembly-CSharp', '', 'HoleShapeAsset'):
            raise ValueError('Terrain-hole reference requires an exact HoleShapeAsset')
        result = asset_data(fields)
        mesh_file, mesh_id = self.resolve(selected, fields['meshAsset'])
        mesh_reader = mesh_file.objects[mesh_id]
        if int(mesh_reader.type) != 43 or mesh_reader.byte_size > 16 * 1024**2:
            raise ValueError('Invalid terrain-hole mesh object')
        mesh = mesh_reader.read()
        if any(int(sub.topology) != 0 for sub in mesh.m_SubMeshes):
            raise ValueError('Terrain-hole mesh requires triangle topology')
        handler = MeshHandler(mesh)
        handler.process()
        result.update(asset_file=selected.name, object_id=path_id,
                      mesh_file=mesh_file.name, mesh_object_id=mesh_id,
                      mesh=geometry(handler.m_Vertices or [],
                                    [face for sub in handler.get_triangles() for face in sub]))
        if len(self.hole_assets) >= 256:
            raise ValueError('Excessive terrain-hole asset roster')
        self.hole_assets[key] = result
        return result


def capture_holes(asset, root, sources):
    from extract_uniform_cliffs import identity, local_pointer

    result, seen = [], set()

    def walk(go, parent=IDENTITY, active=True, depth=0):
        if depth > 128 or go.path_id in seen or len(seen) >= 65536:
            raise ValueError('Invalid terrain-hole prefab hierarchy')
        seen.add(go.path_id)
        fields = go.read_typetree()
        is_root = go.path_id == root.path_id
        local_active = flag(fields['m_IsActive'])
        active = active and (is_root or local_active)
        objects = [local_pointer(asset, entry['component']) for entry in fields['m_Component']]
        transforms = [obj.read_typetree() for obj in objects if int(obj.type) == 4]
        if len(transforms) != 1:
            raise ValueError('Ambiguous terrain-hole transform')
        transform = transforms[0]
        local = parent if is_root else multiply(parent, matrix(transform))
        if active:
            for obj in objects:
                if int(obj.type) == 114 and identity(obj) == ('Assembly-CSharp', '', 'TerrainHoleShape'):
                    data = obj.read_typetree()
                    instance = instance_data(data)
                    result.append(dict(object_id=obj.path_id, local_to_root=[list(row) for row in local],
                                       asset=sources.hole_asset(asset, data['asset']), **instance))
                    if len(result) > 256:
                        raise ValueError('Excessive terrain-hole instance roster')
        for child in transform['m_Children']:
            child_transform = local_pointer(asset, child).read_typetree()
            walk(local_pointer(asset, child_transform['m_GameObject']), local, active, depth + 1)

    walk(root)
    return result
