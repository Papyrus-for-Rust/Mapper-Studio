"""Extract exact selected-installation rail template LODs into an immutable cache.

Generated game geometry stays local. This does not deform paths, cook PhysX
colliders, reproduce terrain contributors or assemble runtime railway objects.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
from UnityPy.helpers.MeshHelper import MeshHandler
from extract_verified_prefabs import snapshot, fingerprint
from extract_rail_source import bounded, reference, source_layout
from mesh_source_nodes import load_mesh_node
from mapper_paths import data_root
from rail_mesh_buffers import geometry

LIMIT = 32 * 1024**2
BUNDLES = ('Bundles/maps/maps.bundle', 'Bundles/shared/content.bundle',
           'Bundles/shared/monuments.bundle', 'Bundles/shared/assetscenes.bundle')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rust-root', type=Path, required=True)
    p.add_argument('--rail-source', type=Path, required=True)
    p.add_argument('--cache-root', type=Path, default=data_root()/'papyrus-rail-mesh-cache')
    a = p.parse_args()
    raw = bounded(a.rail_source)
    rail = json.loads(raw)
    digest = lambda data: hashlib.sha256(data).hexdigest()
    sources = snapshot(a.rust_root) + [fingerprint(a.rust_root, BUNDLES[0], 512*1024**2)]
    source_layout(rail)
    if rail['sources'] != sources:
        raise ValueError('Rail source differs from selected installation')
    code = rail['code_sources']
    allowed = {'GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
               'RustClient_Data/Managed/Assembly-CSharp.dll', 'RustDedicated_Data/Managed/Assembly-CSharp.dll'}
    if not 1 <= len(code) <= 2 or len({s['relative_path'] for s in code}) != len(code):
        raise ValueError('Missing or duplicate rail gameplay receipts')
    def verify_code():
        for expected in code:
            if expected['relative_path'] not in allowed or fingerprint(a.rust_root, expected['relative_path'], 1024**3) != expected:
                raise ValueError('Rail gameplay source changed')
    verify_code()
    stages = [s for s in rail['stages'] if s['kind'] == 'GenerateRailMeshes']
    if len(stages) != 1:
        raise ValueError('Missing unambiguous rail mesh stage')
    stage = stages[0]
    scripts = [Path(__file__).with_name(name) for name in (
        'extract_rail_meshes.py', 'rail_mesh_buffers.py', 'extract_rail_source.py',
        'extract_verified_prefabs.py', 'lazy_unity_bundle.py', 'road_stages.py',
        'gameplay_sources.py', 'mesh_source_nodes.py')]
    implementation = lambda: digest(b''.join(s.name.encode()+b'\0'+s.read_bytes() for s in scripts))
    before = implementation()
    assets, streams, extra = {}, [], {}
    def mesh_asset(relative, name):
        key = (relative, name)
        if key not in assets:
            if relative != BUNDLES[0] and relative not in extra:
                extra[relative] = fingerprint(a.rust_root, relative, 8*1024**3)
            env, stream = load_mesh_node(a.rust_root / relative, name)
            if stream is None:
                assets[key] = None
            else:
                streams.append(stream)
                matches = [asset for asset in env.assets if asset.name == name]
                if len(matches) > 1:
                    raise ValueError('Ambiguous rail resource file')
                assets[key] = matches[0] if matches else None
        return assets[key]
    try:
        scene = mesh_asset(BUNDLES[0], 'BuildPlayer-Procedural Map')
        if scene is None:
            raise ValueError('Missing rail scene')
        component = scene.objects[stage['object_id']]
        if int(component.type) != 114:
            raise ValueError('Rail stage object is not a component')
        script = component.read().m_Script.read_typetree()
        if (script['m_AssemblyName'], script['m_Namespace'], script['m_ClassName']) != ('Assembly-CSharp', '', 'GenerateRailMeshes'):
            raise ValueError('Rail stage script identity changed')
        fields = component.read_typetree(); actual = []
        for field in ('RailMesh', 'RailMeshes', 'RailMaterial', 'RailPhysicMaterial'):
            values = fields[field] if field == 'RailMeshes' else [fields[field]]
            if not isinstance(values, list) or len(values) > 16:
                raise ValueError('Rail resource list exceeds bounds')
            for index, value in enumerate(values):
                ref = reference(value, scene)
                if ref is not None: actual.append(dict(field=field, index=index, **ref))
        if actual != stage['assets']:
            raise ValueError('Rail mesh references differ from source scene')
        selected = [ref for ref in actual if ref['field'] == 'RailMeshes']
        if fields['RailMeshes'] and len(selected) != len(fields['RailMeshes']):
            raise ValueError('Null entry within rail LOD array')
        if not selected: selected = [ref for ref in actual if ref['field'] == 'RailMesh']
        if not selected or [ref['index'] for ref in selected] != list(range(len(selected))):
            raise ValueError('Missing rail LOD or null entry within rail LOD array')
        meshes = []
        for ref in selected:
            if ref['file_id'] == 0:
                asset = scene
            else:
                path = ref['external_path'].replace('\\', '/')
                if not path.startswith('archive:/'):
                    raise ValueError('Unsupported rail mesh external path')
                name = path.rsplit('/', 1)[-1]; asset = None
                for relative in BUNDLES:
                    asset = mesh_asset(relative, name)
                    if asset is not None:
                        break
                if asset is None: raise ValueError('Rail mesh resource file unresolved')
            obj = asset.objects[ref['path_id']]
            if int(obj.type) != 43 or obj.byte_size > 16*1024**2:
                raise ValueError('Rail mesh object exceeds bounds')
            mesh = obj.read()
            if any(int(s.topology) != 0 for s in mesh.m_SubMeshes):
                raise ValueError('Rail template requires triangle submeshes')
            h = MeshHandler(mesh); h.process()
            vec = lambda v: [v.x, v.y, v.z]
            buffers = geometry(h.m_Vertices or [], h.m_Normals or [], h.m_Tangents or [],
                               h.m_UV0 or [], [t for sub in h.get_triangles() for t in sub],
                               vec(mesh.m_LocalAABB.m_Center), vec(mesh.m_LocalAABB.m_Extent))
            meshes.append(dict(reference=ref, name=mesh.m_Name, buffer=buffers))
        if sum(len(m['buffer']['vertices']) for m in meshes) > 262144 or sum(len(m['buffer']['triangles']) for m in meshes) > 524288:
            raise ValueError('Rail template aggregate exceeds bounds')
        if (snapshot(a.rust_root) + [fingerprint(a.rust_root, BUNDLES[0], 512*1024**2)] != sources
                or any(fingerprint(a.rust_root, path, 8*1024**3) != expected for path, expected in extra.items())
                or bounded(a.rail_source) != raw or implementation() != before):
            raise ValueError('Rail mesh source changed during extraction')
        verify_code()
        doc = dict(format='papyrus-rail-mesh-source', schema=1, adapter='papyrus-rail-mesh-buffers-1',
                   rail_source_sha256=digest(raw), sources=sources, code_sources=code,
                   extra_sources=list(extra.values()), extractor_sha256=before, meshes=meshes)
        encoded = json.dumps(doc, separators=(',', ':'), allow_nan=False).encode()
        if len(encoded) > LIMIT: raise ValueError('Rail mesh snapshot exceeds 32 MiB')
        a.cache_root.mkdir(parents=True, exist_ok=True); destination = a.cache_root/digest(encoded)
        if destination.exists(): raise ValueError('Rail mesh snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.rail-mesh-', dir=a.cache_root) as tmp:
            stage_dir = Path(tmp)
            with (stage_dir/'meshes.json').open('xb') as f:
                f.write(encoded); f.flush(); os.fsync(f.fileno())
            publish_directory(stage_dir, destination)
        print(destination/'meshes.json', flush=True)
    finally:
        for stream in streams: stream.close()


if __name__ == '__main__':
    main()
