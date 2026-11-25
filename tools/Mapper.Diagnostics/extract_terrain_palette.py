"""Capture scene-referenced terrain colors from an owned installation, atomically.

Only numeric colors and provenance are cached. No textures, shaders, models or
game code are exported. Unknown/ambiguous layouts fail without publication.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
import time

from extract_verified_prefabs import fingerprint
from mapper_paths import data_root
from terrain_palette import bind, colors, map_colors, shader_fingerprint

ADAPTER = 'papyrus-terrain-palette-1'
SCENE = 'BuildPlayer-Procedural Map'


def sources(root):
    natives = [p for p in ('RustClient_Data', 'RustDedicated_Data') if (root / p / 'Plugins/x86_64/RustNative.dll').is_file()]
    if len(natives) != 1:
        raise ValueError('Select exactly one supported Rust client/server installation')
    return [fingerprint(root, natives[0] + '/Plugins/x86_64/RustNative.dll', 64 * 1024**2),
            fingerprint(root, 'Bundles/maps/maps.bundle', 512 * 1024**2),
            fingerprint(root, 'Bundles/shared/content.bundle', 8 * 1024**3)]


def script_hash():
    names = ('extract_terrain_palette.py', 'terrain_palette.py', 'lazy_unity_bundle.py',
             'resolve_prefab_textures.py', 'extract_verified_prefabs.py')
    return hashlib.sha256(b''.join(name.encode() + b'\0' + Path(__file__).with_name(name).read_bytes() for name in names)).hexdigest()


def inspect(root):
    from lazy_unity_bundle import load
    from resolve_prefab_textures import container_index
    maps, map_stream = load(root / 'Bundles/maps/maps.bundle')
    content_stream = None
    try:
        scenes = [a for a in maps.assets if a.name == SCENE]
        if len(scenes) != 1:
            raise ValueError('Missing exact procedural scene')
        scene = scenes[0]
        if len(scene.objects) > 262144:
            raise ValueError('Procedural scene exceeds object budget')
        def identity(obj):
            script = obj.read().m_Script.read_typetree()
            return script['m_AssemblyName'], script['m_Namespace'], script['m_ClassName']
        setups = [o.path_id for o in scene.objects.values() if int(o.type) == 114 and identity(o) == ('Assembly-CSharp', '', 'WorldSetup')]
        def read(object_id):
            obj = scene.objects[object_id]
            return int(obj.type), obj.read_typetree()
        def external(file_id):
            return scene.externals[file_id - 1].path.replace('\\', '/').rsplit('/', 1)[-1]
        binding = bind(scene.name, setups, read, lambda oid: identity(scene.objects[oid]), external)
        content, content_stream = load(root / 'Bundles/shared/content.bundle')
        files = [a for env in (maps, content) for a in env.assets if a.name == binding['config_file']]
        if len(files) != 1:
            raise ValueError('Missing or ambiguous referenced config file')
        obj = files[0].objects[binding['config_object']]
        if int(obj.type) != 114:
            raise ValueError('Referenced terrain config is not a component asset')
        palette = colors(obj.read_typetree(), identity(obj))
        material_entries = container_index(content).get('assets/prefabs/tools/map/papermap.mat', [])
        if len(material_entries) != 1:
            raise ValueError('Missing or ambiguous PaperMap material')
        material = material_entries[0].deref()
        if int(material.type) != 21:
            raise ValueError('PaperMap is not a material')
        fields = material.read_typetree()
        shader_ref = fields['m_Shader']
        if shader_ref['m_FileID'] != 0:
            raise ValueError('External map shader needs another adapter')
        shader = material.assets_file.objects[shader_ref['m_PathID']]
        if int(shader.type) != 48:
            raise ValueError('Map material shader reference is not a shader')
        shader_fields = shader.read_typetree()
        palette['cartography'] = map_colors(fields, shader_fields['m_ParsedForm']['m_Name'],
                                           material.assets_file.name, material.path_id, shader.path_id,
                                           shader_fingerprint(shader_fields['compressedBlob']))
        aliases = set()
        for env in (maps, content):
            for path, entries in container_index(env).items():
                for entry in entries:
                    # Unrelated container entries can be null or point into a
                    # bundle that is not needed for this config. Resolve only
                    # candidates for this already verified object identity.
                    if hasattr(entry, 'm_PathID') and entry.m_PathID != binding['config_object']:
                        continue
                    candidate = entry.deref() if hasattr(entry, 'deref') else entry
                    if candidate.assets_file.name == binding['config_file'] and candidate.path_id == binding['config_object']:
                        aliases.add(path)
        if len(aliases) > 16 or any(len(p) > 1024 for p in aliases):
            raise ValueError('Excessive config path aliases')
        return dict(scene=SCENE, binding=binding, config_paths=sorted(aliases),
                    numeric_space='unity-serialized-color', **palette)
    finally:
        if content_stream is not None:
            content_stream.close()
        map_stream.close()


def publish(cache_root, raw):
    digest = hashlib.sha256(raw).hexdigest()
    cache_root.mkdir(parents=True, exist_ok=True)
    destination = cache_root / digest
    if destination.exists():
        if (destination / 'palette.json').read_bytes() != raw:
            raise ValueError('Existing terrain palette cache differs')
    else:
        with tempfile.TemporaryDirectory(prefix='.palette-', dir=cache_root) as tmp:
            stage = Path(tmp) / digest
            stage.mkdir()
            (stage / 'palette.json').write_bytes(raw)
            try:
                publish_directory(stage, destination)
            except OSError:
                if not destination.exists():
                    raise
                if (destination / 'palette.json').read_bytes() != raw:
                    raise ValueError('Existing terrain palette cache differs')
    return destination / 'palette.json'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, default=data_root() / 'papyrus-terrain-palette-cache')
    args = parser.parse_args()
    before, adapter_hash = sources(args.rust_root), script_hash()
    palette = inspect(args.rust_root)
    if sources(args.rust_root) != before or script_hash() != adapter_hash:
        raise ValueError('Terrain color sources or extractor changed during capture')
    document = dict(format='papyrus-terrain-palette', schema=1, adapter=ADAPTER,
                    extractor_sha256=adapter_hash, sources=before, **palette)
    raw = json.dumps(document, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()
    if len(raw) > 32 * 1024:
        raise ValueError('Terrain palette exceeds metadata budget')
    print(publish(args.cache_root, raw))


if __name__ == '__main__':
    main()
