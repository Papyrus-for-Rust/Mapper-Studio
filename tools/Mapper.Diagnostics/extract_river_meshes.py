"""Capture the selected installation's exact GenerateRiverMeshes template.

Developer source-bound capture only. No Unity player or game code is executed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
from UnityPy.classes.PPtr import PPtr
from UnityPy.helpers.MeshHelper import MeshHandler
from extract_verified_prefabs import snapshot, fingerprint
from extract_rail_source import bounded, reference
from lazy_unity_bundle import LazyBundle, load
from UnityPy.streams import EndianBinaryReader
from rail_mesh_buffers import geometry
from river_mesh_stages import RESOURCES, stage_data, validate_sequence

BUNDLES = ('Bundles/maps/maps.bundle', 'Bundles/shared/content.bundle',
           'Bundles/shared/monuments.bundle', 'Bundles/shared/assetscenes.bundle')


class ResourceIndex(LazyBundle):
    """Inspect validated UnityFS directory metadata without opening its assets."""
    def read_files(self, blocks, nodes):
        names = [n.path for n in nodes]
        if (len({n.lower() for n in names}) != len(names)
                or any(n.offset < 0 or n.size < 0 or n.offset + n.size > blocks.length for n in nodes)):
            raise ValueError('Invalid or ambiguous river resource index')
        self.node_names = names


def resource_bundle(root, name):
    matches = []
    for relative in BUNDLES:
        with (root / relative).open('rb') as stream:
            index = ResourceIndex(EndianBinaryReader(stream), None, name=relative)
            if name in index.node_names:
                matches.append(relative)
    if len(matches) != 1:
        raise ValueError('Unresolved or ambiguous river resource')
    return matches[0]


def mesh_stages(root, *, layout='legacy', generation_stages=None):
    env, stream = load(root / BUNDLES[0])
    try:
        scenes = [a for a in env.assets if a.name == 'BuildPlayer-Procedural Map']
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural map scene')
        scene = scenes[0]
        def identity(obj):
            data = obj.read().m_Script.read_typetree()
            return data['m_AssemblyName'], data['m_Namespace'], data['m_ClassName']
        setups = [o for o in scene.objects.values() if int(o.type) == 114 and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        if len(setups) != 1:
            raise ValueError('Ambiguous river WorldSetup')
        def ptr(ref):
            if ref['m_FileID'] != 0:
                raise ValueError('External procedural hierarchy')
            return PPtr(**ref, assetsfile=scene).deref()
        seen, stages = set(), []
        ordinal = 0
        def walk(go, active=True, depth=0):
            nonlocal ordinal
            if depth > 128 or go.path_id in seen or len(seen) >= 65536:
                raise ValueError('Invalid river hierarchy')
            seen.add(go.path_id)
            data = go.read_typetree()
            if data['m_IsActive'] not in (0, 1, False, True):
                raise ValueError('Invalid hierarchy activation')
            active = active and bool(data['m_IsActive'])
            records = [ptr(c['component']) for c in data['m_Component']]
            for obj in records:
                if int(obj.type) != 114:
                    continue
                order = ordinal
                ordinal += 1
                ident = identity(obj)
                if ident[:2] != ('Assembly-CSharp', '') or not ident[2].startswith('GenerateRiver'):
                    continue
                f = obj.read_typetree()
                program = stage_data(ident, f, obj.path_id, order, active, layout=layout)
                if generation_stages is not None:
                    generation_stages.append(program)
                if ident[2] != 'GenerateRiverMeshes':
                    continue
                if not isinstance(f['RiverMeshes'], list) or len(f['RiverMeshes']) > 16:
                    raise ValueError('Excessive serialized river mesh array')
                row = {k: v for k, v in program.items() if k not in ('kind', 'order')}
                row['assets'] = []
                for field in RESOURCES:
                    for i, value in enumerate(f[field] if field == 'RiverMeshes' else [f[field]]):
                        ref = reference(value, scene)
                        if ref is not None:
                            row['assets'].append(dict(field=field, index=i, **ref))
                stages.append(row)
            transforms = [o.read_typetree() for o in records if int(o.type) == 4]
            if len(transforms) != 1:
                raise ValueError('Ambiguous river transform')
            for child in transforms[0]['m_Children']:
                walk(ptr(ptr(child).read_typetree()['m_GameObject']), active, depth + 1)
        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if len(stages) != 1:
            raise ValueError('Unsupported river mesh stage count')
        return stages[0]
    finally:
        stream.close()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rust-root', type=Path, required=True)
    p.add_argument('--profile', type=Path, required=True)
    p.add_argument('--cache-root', type=Path, required=True)
    a = p.parse_args()
    profile_raw = bounded(a.profile, 65536)
    profile = json.loads(profile_raw)
    sources = snapshot(a.rust_root)
    digest = lambda value: hashlib.sha256(value).hexdigest()
    current = profile.get('schema') == 2
    expected_keys = {'format', 'schema', 'id', 'native_sha256', 'scene_sha256', 'reference_assembly_sha256', 'code_sources', 'map_scene', 'policy'}
    if current:
        expected_keys.add('stage_layout')
    if (set(profile) != expected_keys or profile['format'] != 'papyrus-river-water-mesh-profile'
            or type(profile['schema']) is not int or profile['schema'] not in (1, 2)
            or current and profile['stage_layout'] != 'mode-description'
            or profile['native_sha256'] != sources[0]['sha256'] or profile['scene_sha256'] != sources[2]['sha256']):
        raise ValueError('River profile differs from selected installation')
    code = profile['code_sources']
    if not isinstance(code, list) or not 1 <= len(code) <= 2:
        raise ValueError('Missing river gameplay receipts')
    def verify_code():
        allowed = {'GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat', 'RustClient_Data/Managed/Assembly-CSharp.dll', 'RustDedicated_Data/Managed/Assembly-CSharp.dll'}
        if len({s['relative_path'] for s in code}) != len(code):
            raise ValueError('Duplicate river gameplay receipts')
        for source in code:
            if source['relative_path'] not in allowed or fingerprint(a.rust_root, source['relative_path'], 1024**3) != source:
                raise ValueError('River gameplay source changed')
    verify_code()
    scene_receipt = fingerprint(a.rust_root, BUNDLES[0], 512*1024**2)
    if profile['map_scene'] != scene_receipt:
        raise ValueError('River scene changed')
    programs = []
    stage = mesh_stages(a.rust_root, layout='mode-description' if current else 'legacy',
                        generation_stages=programs if current else None)
    if current:
        validate_sequence(programs)
    if (stage['mode'] & 1 == 0 or not stage['hierarchy_active']
            or not current and (stage['mode'] != -1 or stage['disabled'] or not stage['component_enabled'])):
        raise ValueError('Unsupported river mesh stage activation')
    refs = [r for r in stage['assets'] if r['field'] == 'RiverMesh']
    if len(refs) != 1:
        raise ValueError('Missing authored RiverMesh; runtime replaces RiverMeshes with this field')
    scripts = [Path(__file__).with_name(n) for n in ('extract_river_meshes.py', 'extract_rail_source.py', 'extract_verified_prefabs.py', 'rail_mesh_buffers.py', 'lazy_unity_bundle.py', 'river_mesh_stages.py', 'monument_stages.py', 'mapper_paths.py')]
    implementation = lambda: digest(b''.join(s.name.encode() + b'\0' + s.read_bytes() for s in scripts))
    before = implementation()
    environments, streams, extra = {}, [], {}
    def environment(relative):
        if relative not in environments:
            if relative not in {s['relative_path'] for s in sources + [scene_receipt]}:
                extra[relative] = fingerprint(a.rust_root, relative, 8*1024**3)
            env, stream = load(a.rust_root / relative)
            environments[relative] = env
            streams.append(stream)
        return environments[relative]
    try:
        scene = next(s for s in environment(BUNDLES[0]).assets if s.name == 'BuildPlayer-Procedural Map')
        ref = refs[0]
        if ref['file_id'] == 0:
            asset = scene
        else:
            external = ref['external_path'].replace('\\', '/')
            if not external.startswith('archive:/'):
                raise ValueError('Unsupported river resource reference')
            name = external.rsplit('/', 1)[-1]
            relative = resource_bundle(a.rust_root, name)
            matches = [s for s in environment(relative).assets if s.name == name]
            if len(matches) != 1:
                raise ValueError('Unresolved or ambiguous river resource')
            asset = matches[0]
        obj = asset.objects[ref['path_id']]
        if int(obj.type) != 43 or obj.byte_size > 16*1024**2:
            raise ValueError('Unsupported river mesh object')
        mesh = obj.read()
        if any(int(s.topology) != 0 for s in mesh.m_SubMeshes):
            raise ValueError('River template requires triangle submeshes')
        handler = MeshHandler(mesh)
        handler.process()
        vec = lambda v: [v.x, v.y, v.z]
        buffer = geometry(handler.m_Vertices or [], handler.m_Normals or [], handler.m_Tangents or [], handler.m_UV0 or [],
                          [t for sub in handler.get_triangles() for t in sub], vec(mesh.m_LocalAABB.m_Center), vec(mesh.m_LocalAABB.m_Extent))
        if snapshot(a.rust_root) != sources or fingerprint(a.rust_root, BUNDLES[0], 512*1024**2) != scene_receipt or bounded(a.profile, 65536) != profile_raw or implementation() != before or any(fingerprint(a.rust_root, name, 8*1024**3) != receipt for name, receipt in extra.items()):
            raise ValueError('River mesh capture changed during extraction')
        verify_code()
        doc = dict(format='papyrus-river-mesh-source', schema=1, adapter='papyrus-river-mesh-buffers-1', sources=sources+[scene_receipt],
                   code_sources=code, extra_sources=list(extra.values()), extractor_sha256=before, profile=profile,
                   mesh_stage=stage, mesh=dict(reference=ref, name=mesh.m_Name, buffer=buffer))
        if current:
            doc.update(schema=2, adapter='papyrus-river-mesh-buffers-2', generation_stages=programs)
        encoded = json.dumps(doc, separators=(',', ':'), allow_nan=False).encode()
        if len(encoded) > 16*1024**2:
            raise ValueError('River mesh snapshot exceeds 16 MiB')
        a.cache_root.mkdir(parents=True, exist_ok=True)
        destination = a.cache_root / digest(encoded)
        if destination.exists():
            raise ValueError('River mesh snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.river-mesh-', dir=a.cache_root) as tmp:
            private = Path(tmp)
            with (private / 'meshes.json').open('xb') as stream:
                stream.write(encoded)
                stream.flush()
                os.fsync(stream.fileno())
            publish_directory(private, destination)
        print(destination / 'meshes.json', flush=True)
    finally:
        for stream in streams:
            stream.close()


if __name__ == '__main__':
    main()
