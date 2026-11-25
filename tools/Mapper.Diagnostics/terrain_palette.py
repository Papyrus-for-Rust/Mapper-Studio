"""Bounded, data-only inspection of a scene's actual TerrainGenerator config."""
import math
import hashlib

BIOMES = ('Arid', 'Temperate', 'Tundra', 'Arctic', 'Jungle')
MAP_COLORS = ('_DirtColor', '_SnowColor', '_SandColor', '_RockColor', '_GrassColor',
              '_ForestColor', '_PebbleColor', '_GravelColor', '_Color', '_WaterColor', '_OffShoreColor')


def shader_fingerprint(blob):
    if not isinstance(blob, (bytes, bytearray, list)) or not 1 <= len(blob) <= 8 * 1024**2:
        raise ValueError('Unsupported map shader blob')
    if isinstance(blob, list) and any(type(v) is not int or not 0 <= v <= 255 for v in blob):
        raise ValueError('Invalid map shader byte')
    return hashlib.sha256(bytes(blob)).hexdigest()


def map_colors(material, shader_name, serialized_file, object_id, shader_id, shader_blob_sha256=None):
    """Data-only PaperMap properties, separate from TerrainConfig's world colors."""
    if material.get('m_Name') != 'PaperMap' or shader_name != 'Custom/Map':
        raise ValueError('Unsupported map material/shader')
    saved = material['m_SavedProperties']
    def unique(entries):
        if len(entries) != len(dict(entries)):
            raise ValueError('Duplicate map material property')
        return dict(entries)
    rgba = unique(saved['m_Colors'])
    floats = unique(saved['m_Floats'])
    colors = []
    for name in MAP_COLORS:
        value = rgba[name]
        row = [value[c] for c in 'rgba']
        if set(value) != set('rgba') or any(type(v) not in (int, float) or not math.isfinite(v) or not 0 <= v <= 1 for v in row):
            raise ValueError('Invalid map color')
        colors.append(row)
    direction = [rgba['_SunDirection'][c] for c in 'rgb']
    if any(not math.isfinite(v) or abs(v) > 100 for v in direction) or sum(v*v for v in direction) < 1e-8:
        raise ValueError('Invalid map sun direction')
    params = [floats[k] for k in ('_SunPower', '_Brightness', '_Contrast')]
    if any(type(v) not in (int, float) or not math.isfinite(v) or not 0 <= v <= 4 for v in params):
        raise ValueError('Invalid map shading properties')
    result = dict(material_path='assets/prefabs/tools/map/papermap.mat', serialized_file=serialized_file,
                material_object=object_id, shader_object=shader_id, shader_name=shader_name,
                colors=colors, sun_direction=direction, sun_power=params[0], brightness=params[1], contrast=params[2])
    if shader_blob_sha256 is not None:
        if not isinstance(shader_blob_sha256, str) or len(shader_blob_sha256) != 64 or any(c not in '0123456789abcdef' for c in shader_blob_sha256):
            raise ValueError('Invalid map shader fingerprint')
        result['shader_blob_sha256'] = shader_blob_sha256
    return result


def reference(value):
    if not isinstance(value, dict) or set(value) != {'m_FileID', 'm_PathID'}:
        raise ValueError('Unsupported terrain object reference')
    file_id, object_id = value['m_FileID'], value['m_PathID']
    if type(file_id) is not int or not 0 <= file_id <= 256 or type(object_id) is not int or not -(2**63) < object_id < 2**63 or not object_id:
        raise ValueError('Invalid terrain object reference')
    return file_id, object_id


def bind(scene_file, setup_ids, read, identity, external):
    """Resolve WorldSetup.terrain -> attached TerrainGenerator -> exact config ref.

    read returns (Unity class ID, fields) for a local scene object. external
    resolves an external file index to its serialized-file name, never an asset
    filename search. No game component is constructed or executed.
    """
    if len(setup_ids) != 1:
        raise ValueError('Missing or ambiguous WorldSetup')
    setup = setup_ids[0]
    kind, fields = read(setup)
    if kind != 114 or identity(setup) != ('Assembly-CSharp', '', 'WorldSetup'):
        raise ValueError('Unsupported WorldSetup identity')
    file_id, terrain = reference(fields['terrain'])
    if file_id:
        raise ValueError('External terrain hierarchy needs another adapter')
    kind, fields = read(terrain)
    if kind != 1 or fields.get('m_IsActive') not in (1, True):
        raise ValueError('Missing or inactive terrain GameObject')
    components = fields.get('m_Component')
    if not isinstance(components, list) or not 1 <= len(components) <= 1024:
        raise ValueError('Unsupported terrain component roster')
    generators, seen = [], set()
    for entry in components:
        file_id, object_id = reference(entry['component'])
        if file_id or object_id in seen:
            raise ValueError('External or duplicate terrain component')
        seen.add(object_id)
        kind, fields = read(object_id)
        if kind == 114 and identity(object_id) == ('Assembly-CSharp', '', 'TerrainGenerator'):
            if fields.get('m_Enabled') not in (1, True) or reference(fields['m_GameObject']) != (0, terrain):
                raise ValueError('Inactive or detached TerrainGenerator')
            generators.append((object_id, fields))
    if len(generators) != 1:
        raise ValueError('Missing or ambiguous TerrainGenerator')
    generator, fields = generators[0]
    file_id, config = reference(fields['config'])
    config_file = external(file_id) if file_id else scene_file
    if not isinstance(config_file, str) or not 1 <= len(config_file) <= 256 or '/' in config_file or '\\' in config_file:
        raise ValueError('Invalid config serialized-file identity')
    return dict(scene_file=scene_file, world_setup=setup, terrain=terrain,
                generator=generator, config_file=config_file, config_object=config)


def colors(fields, identity):
    if identity != ('Assembly-CSharp', '', 'TerrainConfig'):
        raise ValueError('Unsupported TerrainConfig identity')
    splats = fields.get('Splats')
    if not isinstance(splats, list) or len(splats) != 8:
        raise ValueError('Terrain palette requires eight verified material channels')
    jungle = ['JungleColor' in s for s in splats]
    if any(jungle) and not all(jungle):
        raise ValueError('Incomplete jungle color coverage')
    biomes = BIOMES if all(jungle) else BIOMES[:4]
    materials, seen = [], set()
    for splat in splats:
        name = splat.get('Name')
        if not isinstance(name, str) or not 1 <= len(name) <= 128 or name in seen:
            raise ValueError('Invalid or duplicate terrain material name')
        seen.add(name)
        row = []
        for biome in biomes:
            color = splat.get(biome + 'Color')
            if not isinstance(color, dict) or set(color) != {'r', 'g', 'b', 'a'}:
                raise ValueError('Missing or unsupported serialized biome color')
            rgba = [color[c] for c in 'rgba']
            if any(type(v) not in (int, float) or not math.isfinite(v) or not 0 <= v <= 1 for v in rgba):
                raise ValueError('Unsupported non-finite or HDR terrain color')
            row.append(rgba)
        materials.append(dict(name=name, biome_colors=row))
    return dict(biomes=list(biomes), materials=materials)
