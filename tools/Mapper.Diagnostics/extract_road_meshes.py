"""Capture exact road mesh templates and resource handles from a selected install.

This does not enable road surfaces, cook PhysX colliders or publish game assets.
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
from extract_rail_source import bounded
from mesh_source_nodes import load_mesh_node
from mapper_paths import data_root
from rail_mesh_buffers import geometry
from road_mesh_stages import mesh_stages, validate_policy
from road_stages import source_layout

BUNDLES = ('Bundles/maps/maps.bundle', 'Bundles/shared/content.bundle',
           'Bundles/shared/monuments.bundle', 'Bundles/shared/assetscenes.bundle')
LIMIT = 32 * 1024**2


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rust-root', type=Path, required=True)
    p.add_argument('--infrastructure', type=Path, required=True)
    p.add_argument('--profile', type=Path, required=True)
    p.add_argument('--cache-root', type=Path, default=data_root()/'papyrus-road-mesh-cache')
    a = p.parse_args()
    parent_raw = bounded(a.infrastructure, 16*1024**2); parent = json.loads(parent_raw)
    profile_raw = bounded(a.profile); profile = json.loads(profile_raw)
    validate_policy(profile.get('policy'))
    digest = lambda data: hashlib.sha256(data).hexdigest()
    layout = source_layout(parent)
    sources = snapshot(a.rust_root)
    if (parent.get('sources') != sources or profile.get('format') != 'papyrus-road-surface-profile'
            or set(profile) != ({'format', 'schema', 'id', 'native_sha256', 'scene_sha256', 'code_sources', 'map_scene', 'policy'}
                                | ({'stage_layout'} if layout == 'mode-description' else set()))
            or profile.get('stage_layout') != parent['profile'].get('stage_layout')
            or type(profile.get('schema')) is not int or profile['schema'] != 1 or profile.get('native_sha256') != sources[0]['sha256']
            or profile.get('scene_sha256') != sources[2]['sha256']
            or profile.get('code_sources') != parent['profile'].get('code_sources')
            or profile.get('map_scene') != parent['profile'].get('map_scene')):
        raise ValueError('Road surface profile/parent differs from selected installation')
    code = profile['code_sources']
    if not 1 <= len(code) <= 2:
        raise ValueError('Missing road gameplay receipts')
    def verify_code():
        allowed = ('GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat',
                   'RustClient_Data/Managed/Assembly-CSharp.dll', 'RustDedicated_Data/Managed/Assembly-CSharp.dll')
        for expected in code:
            if expected['relative_path'] not in allowed or fingerprint(a.rust_root, expected['relative_path'], 1024**3) != expected:
                raise ValueError('Road gameplay code changed')
    verify_code()
    map_scene = fingerprint(a.rust_root, BUNDLES[0], 512*1024**2)
    if map_scene != profile['map_scene']:
        raise ValueError('Road map scene changed')
    stages = mesh_stages(a.rust_root, stage_layout=layout)
    if len(stages) != 1 or stages[0]['mode'] != -1 or stages[0]['disabled'] or not stages[0]['component_enabled'] or not stages[0]['hierarchy_active']:
        raise ValueError('Unsupported road mesh stage scheduling')
    stage = stages[0]
    selected = [ref for ref in stage['assets'] if ref['field'] == 'RoadMeshes']
    if stage['lod_array_count']:
        if len(selected) != stage['lod_array_count']:
            raise ValueError('Null entry within road LOD array')
    else:
        selected = [ref for ref in stage['assets'] if ref['field'] == 'RoadMesh']
    if not selected or [ref['index'] for ref in selected] != list(range(len(selected))):
        raise ValueError('Missing ordered road mesh LODs')
    scripts = [Path(__file__).with_name(name) for name in ('extract_road_meshes.py', 'road_mesh_stages.py',
               'rail_mesh_buffers.py', 'extract_rail_source.py', 'extract_verified_prefabs.py',
               'lazy_unity_bundle.py', 'road_stages.py', 'mesh_source_nodes.py')]
    implementation = lambda: digest(b''.join(path.name.encode()+b'\0'+path.read_bytes() for path in scripts))
    before = implementation()
    assets, streams, extra = {}, [], {}
    primary = {source['relative_path'] for source in sources + [map_scene]}
    def mesh_asset(relative, name):
        key = (relative, name)
        if key not in assets:
            if relative not in primary and relative not in extra:
                extra[relative] = fingerprint(a.rust_root, relative, 8*1024**3)
            env, stream = load_mesh_node(a.rust_root / relative, name)
            if stream is None:
                assets[key] = None
            else:
                streams.append(stream)
                matches = [asset for asset in env.assets if asset.name == name]
                if len(matches) > 1:
                    raise ValueError('Ambiguous road mesh resource')
                assets[key] = matches[0] if matches else None
        return assets[key]
    try:
        meshes = []
        for ref in selected:
            if ref['file_id'] == 0:
                asset = mesh_asset(BUNDLES[0], 'BuildPlayer-Procedural Map')
            else:
                path = ref['external_path'].replace('\\', '/')
                if not path.startswith('archive:/'):
                    raise ValueError('Unsupported road mesh external path')
                name = path.rsplit('/', 1)[-1]; asset = None
                for relative in BUNDLES:
                    asset = mesh_asset(relative, name)
                    if asset is not None:
                        break
            if asset is None:
                raise ValueError('Unresolved road mesh resource')
            obj = asset.objects[ref['path_id']]
            if int(obj.type) != 43 or obj.byte_size > 16*1024**2:
                raise ValueError('Road mesh object exceeds bounds')
            mesh = obj.read()
            if any(int(sub.topology) != 0 for sub in mesh.m_SubMeshes):
                raise ValueError('Road mesh requires triangle submeshes')
            handler = MeshHandler(mesh); handler.process()
            vec = lambda v: [v.x, v.y, v.z]
            buffer = geometry(handler.m_Vertices or [], handler.m_Normals or [], handler.m_Tangents or [],
                              handler.m_UV0 or [], [t for sub in handler.get_triangles() for t in sub],
                              vec(mesh.m_LocalAABB.m_Center), vec(mesh.m_LocalAABB.m_Extent))
            meshes.append(dict(reference=ref, name=mesh.m_Name, buffer=buffer))
        if sum(len(m['buffer']['vertices']) for m in meshes) > 262144 or sum(len(m['buffer']['triangles']) for m in meshes) > 524288:
            raise ValueError('Road mesh aggregate exceeds bounds')
        print(f'Captured {len(meshes)} road LODs; checking source receipts', flush=True)
        if (snapshot(a.rust_root) != sources or fingerprint(a.rust_root, BUNDLES[0], 512*1024**2) != map_scene
                or any(fingerprint(a.rust_root, name, 8*1024**3) != value for name, value in extra.items())
                or bounded(a.infrastructure, 16*1024**2) != parent_raw or bounded(a.profile) != profile_raw
                or implementation() != before):
            raise ValueError('Road mesh sources changed during extraction')
        verify_code()
        version = 1 if layout == 'legacy' else 2
        doc = dict(format='papyrus-road-mesh-source', schema=version, adapter=f'papyrus-road-mesh-buffers-{version}',
                   infrastructure_sha256=digest(parent_raw), sources=sources+[map_scene], code_sources=code,
                   extra_sources=list(extra.values()), extractor_sha256=before, profile=profile,
                   mesh_stage=stage, meshes=meshes)
        encoded = json.dumps(doc, separators=(',', ':'), allow_nan=False).encode()
        if len(encoded) > LIMIT: raise ValueError('Road mesh snapshot exceeds 32 MiB')
        a.cache_root.mkdir(parents=True, exist_ok=True); destination = a.cache_root/digest(encoded)
        if destination.exists(): raise ValueError('Road mesh snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.road-mesh-', dir=a.cache_root) as tmp:
            private = Path(tmp)
            with (private/'meshes.json').open('xb') as stream:
                stream.write(encoded); stream.flush(); os.fsync(stream.fileno())
            publish_directory(private, destination)
        print(destination/'meshes.json', flush=True)
    finally:
        for stream in streams: stream.close()


if __name__ == '__main__':
    main()
