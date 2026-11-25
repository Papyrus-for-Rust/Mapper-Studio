"""Extract installation-bound monument eligibility and optional static preview meshes.

No game assets are distributed. Unsupported placement attributes and mesh sources
are reported explicitly. Model preview is untextured and not collision validation.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
from mapper_paths import publish_directory
import tempfile
import os
from UnityPy.classes.PPtr import PPtr
from UnityPy.helpers.MeshHelper import MeshHandler
from extract_verified_prefabs import snapshot, fingerprint
from lazy_unity_bundle import load
from prefab_geometry import IDENTITY, matrix, multiply, point, vector
from mapper_paths import data_root
from model_preview_geometry import PreviewMesh
import monument_placement_attributes as placement_attributes
from gameplay_sources import verify_code

LIMIT = 32 * 1024**2


def profile_code(root, profile):
    schema = profile.get('schema')
    if type(schema) is not int or schema not in (1, 2):
        raise ValueError('Unsupported monument profile schema')
    code = profile.get('code_sources', [])
    if schema == 1:
        if code:
            raise ValueError('Legacy monument profile cannot carry gameplay receipts')
        return []
    if not isinstance(code, list) or not code:
        raise ValueError('Monument profile requires gameplay receipts')
    paths = {r.get('relative_path') for r in code
             if isinstance(r, dict) and isinstance(r.get('relative_path'), str)}
    layouts = ({'GameAssembly.dll', 'RustClient_Data/il2cpp_data/Metadata/global-metadata.dat'},
               {'RustClient_Data/Managed/Assembly-CSharp.dll'},
               {'RustDedicated_Data/Managed/Assembly-CSharp.dll'})
    if paths not in layouts or len(paths) != len(code):
        raise ValueError('Incomplete monument gameplay receipt layout')
    return verify_code(root, profile, label='Monument')


def tier_mask(tier, profile):
    if type(tier) is not int or (tier != -1 and (tier < 0 or tier & ~7)):
        raise ValueError("Unsupported monument tier convention")
    mask = 0
    for bit, topology in profile["tier_to_topology"].items():
        if int(bit) & tier:
            mask |= topology
    return mask


def rules_attribute(kind, fields, position, rules):
    if kind == "TerrainAnchor":
        names = ("Extents", "Offset", "Radius", "SlopeScale")
        rules['anchors'].append(dict(position=position, **{k.lower().replace('slopescale', 'slope_scale'): fields[k] for k in names}))
    elif kind == "TerrainFilter":
        rules['filters'].append(dict(position=position, filter=fields['Filter'], radius=fields['CheckTopologyRadius'],
                                     check_bounds=bool(fields['CheckTerrainBounds']), check_placement=bool(fields['CheckPlacementMap'])))
        if fields['CheckPlacementMap']:
            rules['pending'].append('Placement-map occupancy; projected monument/stamp reservations only')


def decor_requirement(kind, fields):
    """Accept only proven unit-scale/yaw-compatible transforms in this adapter."""
    if kind == 'DecorScale':
        if vector(fields['MinScale']) == vector(fields['MaxScale']) == [1., 1., 1.]:
            return None
        return 'DecorScale requires nonunit placement scale'
    if kind == 'DecorAlign':
        if (fields['NormalAlignment'] == fields['GradientAlignment'] == 0
                and vector(fields['SlopeOffset']) == [0., 0., 0.]
                and vector(fields['SlopeScale']) == [1., 1., 1.]):
            return None
        return 'DecorAlign requires terrain-relative rotation/scale'
    if kind == 'DecorRotate':
        lo, hi = vector(fields['MinRotation']), vector(fields['MaxRotation'])
        if any(lo[i] != 0 or hi[i] != 0 for i in (0, 2)):
            return 'DecorRotate requires pitch/roll placement'
    return None


def collect_decor(kind, fields, root, rules):
    # DecorComponentEx stops at the first non-root attribute. Child object decor
    # belongs to later prefab assembly, not the parent's proposed placement pose.
    if not root:
        rules['pending'].append('Nested '+kind+' requires prefab assembly')
        return
    required = decor_requirement(kind, fields)
    if required:
        rules['unsupported'].append(required)


def collect_rules(records, transform, root, rules, profile, *, components=None):
    """PrefabPreProcess.FindIComponents traverses inactive child transforms.

    Attribute setup and terrain filtering do not use GameObject activation or
    Behaviour.enabled. Preview meshes retain their separate visibility policy.
    """
    info_found = False
    if components is None:
        components = ((record, None, None) for record in records if int(record.type) == 114)
    for record, fields, identity in components:
        if fields is None:
            fields = record.read_typetree()
        if root and {'Tier', 'Bounds', 'MinWorldSize'}.issubset(fields):
            rules['tier_mask'] = tier_mask(fields['Tier'], profile)
            info_found = True
        if {'Extents', 'Offset', 'Radius', 'SlopeScale'}.issubset(fields):
            rules_attribute('TerrainAnchor', fields, point(transform, [0., 0., 0.]), rules)
        elif {'Filter', 'CheckPlacementMap', 'CheckTerrainBounds', 'CheckTopologyRadius'}.issubset(fields):
            rules_attribute('TerrainFilter', fields, point(transform, [0., 0., 0.]), rules)
        else:
            try:
                kind = identity[2] if identity is not None else record.read().m_Script.read_typetree()['m_ClassName']
            except (KeyError, AttributeError, ValueError):
                kind = ''
            if kind in ('DecorAlign', 'DecorScale', 'DecorRotate'):
                collect_decor(kind, fields, root, rules)
            elif kind.startswith(('TerrainCheck', 'WaterCheck', 'EnvironmentVolume')):
                rules['pending'].append(kind)
    return info_found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--models', action='store_true')
    parser.add_argument('--environment-meshes', action='store_true',
                        help='Capture exact environment collision buffers for world collision preparation')
    parser.add_argument('--prefab', action='append', default=[])
    parser.add_argument('--cache-root', type=Path, default=data_root()/'papyrus-monument-cache')
    args = parser.parse_args()
    raw = args.catalog.read_bytes()
    if len(raw) > 16*1024**2: raise ValueError('Catalog exceeds bound')
    catalog = json.loads(raw)
    sources = snapshot(args.rust_root)
    if catalog.get('format') != 'papyrus-prefab-catalog' or catalog['sources'] != sources:
        raise ValueError('Catalog differs from selected installation')
    profile_raw = args.profile.read_bytes()
    if len(profile_raw) > 65536: raise ValueError('Profile exceeds bound')
    profile = json.loads(profile_raw)
    if profile.get('format') != 'papyrus-monument-profile' or profile['native_sha256'] != sources[0]['sha256'] or profile['scene_sha256'] != sources[2]['sha256'] or not 1 <= profile['min_corners'] <= 4 or set(profile['tier_to_topology']) != {'1','2','4'}:
        raise ValueError('Missing exact monument tier adapter')
    code_sources = profile_code(args.rust_root, profile)
    scripts = [Path(__file__).with_name(n) for n in ('extract_monument_source.py','prefab_geometry.py','lazy_unity_bundle.py','model_preview_geometry.py','monument_placement_attributes.py','gameplay_sources.py','extract_verified_prefabs.py')]
    before = hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
    targets = {p:a for group in ('modifiers','special') for p,a in catalog['metadata'][group].items()
               if a.get('monumentBounds') is not None and (not args.prefab or p in args.prefab)}
    if not 1 <= len(targets) <= 512: raise ValueError('Monument roster exceeds bounds')
    environments = {}; streams = []; extra_sources = {}
    def environment(relative):
        if relative not in environments:
            if relative != sources[2]['relative_path']:
                extra_sources[relative] = fingerprint(args.rust_root, relative, 8*1024**3)
            selected = None
            if relative == sources[2]['relative_path']:
                scenes = {'BuildPlayer-'+a['assetScene'] for a in targets.values()}
                selected = scenes
            e,s = load(args.rust_root/relative, nodes=selected, follow_dependencies=selected is not None); environments[relative] = e; streams.append(s)
        return environments[relative]
    env = environment('Bundles/shared/assetscenes.bundle')
    def resolve(asset, ref):
        if ref['m_PathID'] == 0 or ref['m_FileID'] < 0: raise ValueError('Invalid mesh reference')
        if ref['m_FileID'] == 0: return asset.objects[ref['m_PathID']]
        if ref['m_FileID'] > len(asset.externals): raise ValueError('Invalid external table index')
        external = asset.externals[ref['m_FileID']-1].path.replace('\\','/')
        if not external.startswith('archive:/'): raise ValueError('Unsupported external mesh source: '+external)
        name = external.rsplit('/',1)[-1]
        for relative in ('Bundles/shared/assetscenes.bundle','Bundles/shared/monuments.bundle','Bundles/shared/content.bundle'):
            matches = [a for a in environment(relative).assets if a.name == name]
            if len(matches) == 1: return matches[0].objects[ref['m_PathID']]
        raise ValueError('External mesh file unresolved')
    prefabs = {}; total_triangles = 0; collision_triangles = 0; collision_mesh_cache = {}
    try:
        for asset in env.assets:
            names = {p for p,a in targets.items() if asset.name == 'BuildPlayer-'+a['assetScene']}
            if not names: continue
            def ptr(ref):
                if ref['m_FileID'] != 0: raise ValueError('External prefab hierarchy')
                if len(ref) != 2 or 'm_PathID' not in ref:
                    return PPtr(**ref, assetsfile=asset).deref()
                path_id = ref['m_PathID']
                if path_id == 0: raise ValueError("PPtr can't deref with m_PathID == 0!")
                return asset.objects[path_id]
            script_identities = {}
            def script_identity(record, fields):
                ref = fields['m_Script']; key = ref['m_FileID'], ref['m_PathID']
                if key not in script_identities:
                    value = PPtr(**ref, assetsfile=asset).read_typetree()
                    script_identities[key] = value['m_AssemblyName'], value['m_Namespace'], value['m_ClassName']
                return script_identities[key]
            for obj in asset.objects.values():
                if int(obj.type) != 1: continue
                name = obj.read().m_Name.lower()
                if name not in names: continue
                if name in prefabs: raise ValueError('Duplicate monument root')
                rules = dict(tier_mask=0, min_corners=profile['min_corners'], anchors=[], filters=[], unsupported=[], pending=[])
                attributes = placement_attributes.empty()
                if args.environment_meshes: attributes['environment']['collider_meshes'] = []
                model = dict(vertices=[], triangles=[], issues=[], complete=False, simplified=False)
                preview = PreviewMesh()
                nodes = []; visited = set(); info_found = False; collision_seen = set()
                def walk(go, parent=IDENTITY, active=True, depth=0, rotation_chain=(), incoming=None):
                    nonlocal info_found, collision_triangles
                    if depth > 128 or len(visited) >= 65536 or go.path_id in visited: raise ValueError('Invalid monument hierarchy')
                    visited.add(go.path_id); f = go.read_typetree(); root = go.path_id == obj.path_id
                    active = active and (root or bool(f['m_IsActive']))
                    records = [ptr(c['component']) for c in f['m_Component']]
                    component_fields = {}
                    def fields_for(record):
                        if record.path_id not in component_fields:
                            component_fields[record.path_id] = record.read_typetree()
                        return component_fields[record.path_id]
                    ts = [o for o in records if int(o.type) in (4,224)]
                    if len(ts) != 1: raise ValueError('Missing monument transform')
                    # A child link already decoded the transform to find its GameObject.
                    # Confirm the component owns that transform before reusing it.
                    reader = ts[0]
                    t = (incoming[1] if incoming is not None and incoming[0] is reader
                         else fields_for(reader))
                    transform = parent if root else multiply(parent,matrix(t))
                    rotations = rotation_chain if root else rotation_chain+(tuple(vector(t['m_LocalRotation'],'xyzw')),)
                    if root:
                        try: placement_attributes.root_transform(t, attributes)
                        except (ValueError,KeyError,TypeError) as error: attributes['environment']['issues'].append(str(error)[:160])
                    if root and (vector(t['m_LocalScale']) != [1.,1.,1.] or vector(t['m_LocalRotation'],'xyzw') != [0.,0.,0.,1.]):
                        rules['unsupported'].append('Nonidentity prefab root rotation/scale')
                    if args.models:
                        nodes.append((records, transform, active, depth))
                    decoded_components = []
                    for record in records:
                        if int(record.type) != 114: continue
                        fields = fields_for(record)
                        identity = None
                        try:
                            identity = script_identity(record, fields)
                            game_object = None
                            if identity[:2] == ('Assembly-CSharp','') and identity[2] in ('EnvironmentVolume','EnvironmentVolumeCheck'):
                                game_object = placement_attributes.environment_object(
                                    go.path_id, f['m_Layer'],
                                    [r.path_id for r in records if int(r.type)==114 and script_identity(r,fields_for(r))==('Assembly-CSharp','','EnvironmentVolume')],
                                    [fields_for(r)['m_Enabled'] for r in records if int(r.type) in (64,65,135,136,154)])
                            placement_attributes.capture(identity, fields,
                                point(transform,[0.,0.,0.]), root, record.path_id, active, attributes,
                                transform=transform, collider_types=[int(r.type) for r in records if int(r.type) in (64,65,135,136,154)],
                                collider_ids=[r.path_id for r in records if int(r.type) in (64,65,135,136,154)], rotation_chain=rotations,
                                game_object=game_object)
                            if args.environment_meshes and identity == ('Assembly-CSharp', '', 'EnvironmentVolume'):
                                for collider in records:
                                    if int(collider.type) != 64 or collider.path_id in collision_seen: continue
                                    collision_seen.add(collider.path_id)
                                    try:
                                        collider_fields = fields_for(collider)
                                        mesh_reader = resolve(asset, collider_fields['m_Mesh'])
                                        key = (mesh_reader.assets_file.name, mesh_reader.path_id)
                                        if key not in collision_mesh_cache:
                                            if int(mesh_reader.type) != 43 or mesh_reader.byte_size > 16*1024**2:
                                                raise ValueError('Environment collision mesh object exceeds bounds')
                                            mesh = mesh_reader.read()
                                            if any(int(sub.topology) != 0 for sub in mesh.m_SubMeshes):
                                                raise ValueError('Environment collider requires triangle topology')
                                            handler = MeshHandler(mesh); handler.process()
                                            vertices = handler.m_Vertices or []
                                            triangles = [t for sub in handler.get_triangles() for t in sub]
                                            # Bound the global decode before retaining mesh buffers.
                                            if len(vertices) > 65536 or not 1 <= len(triangles) <= 131072 or collision_triangles+len(triangles) > 262144:
                                                raise ValueError('Environment collision mesh budget exceeded')
                                            collision_triangles += len(triangles)
                                            collision_mesh_cache[key] = (vertices, triangles)
                                        vertices, triangles = collision_mesh_cache[key]
                                        attributes['environment']['collider_meshes'].append(
                                            placement_attributes.collider_mesh(collider.path_id, collider_fields, key[0], key[1], vertices, triangles))
                                    except (ValueError, KeyError, TypeError, AttributeError) as error:
                                        attributes['environment']['issues'].append('Collider '+str(collider.path_id)+': '+str(error)[:120])
                        except (ValueError, KeyError, TypeError, AttributeError) as error:
                            if identity is None or identity == ('Assembly-CSharp','','WaterBody'):
                                attributes['environment']['water_placement']['issues'].append('Incomplete water component inspection: '+str(error)[:110])
                            target = attributes['environment']['issues'] if identity is None or identity[:2]==('Assembly-CSharp','') and identity[2] in ('EnvironmentVolume','EnvironmentVolumeCheck') else attributes['issues']
                            target.append(str(error)[:160])
                        decoded_components.append((record, fields, identity))
                    found_info = collect_rules(records, transform, root, rules, profile,
                                               components=decoded_components)
                    info_found = info_found or found_info
                    # Reuse is local to one GameObject. Do not retain decoded
                    # component bodies across the recursive hierarchy walk.
                    component_fields.clear()
                    decoded_components.clear()
                    for child in t['m_Children']:
                        child_reader = ptr(child)
                        child_transform = child_reader.read_typetree()
                        walk(ptr(child_transform['m_GameObject']), transform, active, depth+1,
                             rotations, (child_reader, child_transform))
                try: walk(obj)
                except (ValueError,KeyError,TypeError) as error:
                    rules['unsupported'].append(str(error))
                    attributes['environment']['issues'].append('Incomplete environment hierarchy: '+str(error)[:110])
                    attributes['environment']['water_placement']['issues'].append('Incomplete water hierarchy: '+str(error)[:110])
                if not info_found: rules['unsupported'].append('Missing source MonumentInfo tier')
                if len(rules['anchors']) > 4096 or len(rules['filters']) > 4096: raise ValueError('Excessive monument attributes')
                if args.models:
                    if any(int(record.type)==137 for records,_,active,_ in nodes for record in records if active): model['issues'].append('Skinned model rendering requires another adapter')
                    masters=[]
                    for records,_,_,depth in nodes:
                        for record in records:
                            if int(record.type)!=114:continue
                            fields=record.read_typetree()
                            if {'ReplacementMesh','ChildComponents','MeshBounds'}.issubset(fields) and fields['ReplacementMesh'].get('m_FileID')==0 and fields['ReplacementMesh'].get('m_PathID'):
                                masters.append((depth,fields['ReplacementMesh']['m_PathID']))
                    master_ids={i for depth,i in masters if depth==min(d for d,_ in masters)} if masters else set()
                    lod_renderers = set(); all_lod_renderers = set()
                    for records,_,_,_ in nodes:
                        for record in records:
                            if int(record.type) == 205:
                                lods = record.read_typetree().get('m_LODs',[])
                                choices = [[r.get('renderer',r)['m_PathID'] for r in lod.get('renderers',[])] for lod in lods]
                                all_lod_renderers.update(i for choice in choices for i in choice)
                                lod_renderers.update(next((choice for choice in reversed(choices) if choice),[]))
                    for records,transform,active,_ in nodes:
                        renderers = [r for r in records if int(r.type) == 23 and (r.path_id in master_ids if master_ids else active and r.read_typetree().get('m_Enabled',False))]
                        if not renderers or not any(r.path_id not in all_lod_renderers or r.path_id in lod_renderers for r in renderers): continue
                        meshes = [r for r in records if int(r.type) == 33]
                        if len(meshes) != 1: model['issues'].append('Renderer has no supported MeshFilter'); continue
                        try:
                            mesh_ref=meshes[0].read_typetree()['m_Mesh']
                            if not master_ids:
                                for component in records:
                                    if int(component.type)!=114:continue
                                    states=component.read_typetree().get('States',[])
                                    if states and isinstance(states,list) and all(isinstance(x,dict) and 'distance' in x and 'mesh' in x for x in states):
                                        mesh_ref=next((x['mesh'] for x in reversed(states) if x['mesh'].get('m_PathID')),mesh_ref)
                            record = resolve(asset, mesh_ref)
                            if int(record.type) != 43 or record.byte_size > 16*1024**2: raise ValueError('Mesh object outside bounds')
                            mesh = record.read(); handler = MeshHandler(mesh); handler.process()
                            vertices = handler.m_Vertices or []; triangles = [t for sub in handler.get_triangles() for t in sub]
                            if not vertices or len(vertices) > 200000 or len(triangles) > 200000: raise ValueError('Mesh outside preview budget')
                            transformed = [point(transform,v) for v in vertices]
                            if any(not math.isfinite(v) or abs(v)>10000 for p in transformed for v in p): raise ValueError('Invalid model vertex')
                            if any(len(t)!=3 or min(t)<0 or max(t)>=len(vertices) for t in triangles): raise ValueError('Invalid model triangle')
                            preview.add(transformed, triangles)
                        except Exception as error: model['issues'].append(str(error)[:160])
                    model['vertices'],model['triangles']=preview.finish()
                    model['simplified']=preview.simplified
                    total_triangles+=len(model['triangles'])
                    model['issues'] = sorted(set(model['issues']))[:256]
                    model['complete'] = bool(model['triangles']) and not model['issues']
                rules['unsupported'] = sorted(set(rules['unsupported']))[:256]; rules['pending'] = sorted(set(rules['pending']))[:256]
                if any(len(attributes[k]) > 4096 for k in ('root_decor','terrain_checks','water_checks','path_connectors','placement_blockers')): raise ValueError('Excessive placement attributes')
                attributes['issues'] = sorted(set(attributes['issues']))[:256]
                if attributes['environment']['root_transform'] is None:
                    attributes['environment']['issues'].append('Missing environment prefab root transform')
                attributes['environment']['issues'] = sorted(set(attributes['environment']['issues']))[:256]
                attributes['environment']['water_placement']['issues'] = sorted(set(attributes['environment']['water_placement']['issues']))[:256]
                prefabs[name] = dict(rules=rules, model=model, placement_attributes=attributes)
                print(name, len(rules['anchors']), len(rules['filters']), len(model['triangles']),flush=True)
        if set(prefabs) != set(targets): raise ValueError('Incomplete monument root roster')
        if snapshot(args.rust_root) != sources or profile_code(args.rust_root, profile) != code_sources or any(fingerprint(args.rust_root,p,8*1024**3)!=s for p,s in extra_sources.items()): raise ValueError('Monument sources changed during extraction')
        after = hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
        if before != after or args.profile.read_bytes() != profile_raw: raise ValueError('Extractor/profile changed during extraction')
        doc = dict(format='papyrus-monument-source',schema=1,adapter='papyrus-monument-unit-yaw-1',catalog_sha256=hashlib.sha256(raw).hexdigest(),sources=sources,extra_sources=list(extra_sources.values()),extractor_sha256=before,profile_sha256=hashlib.sha256(profile_raw).hexdigest(),models_requested=args.models,prefabs=prefabs)
        if code_sources:
            doc.update(schema=2, code_sources=code_sources)
        encoded = json.dumps(doc,separators=(',',':'),allow_nan=False).encode()
        if len(encoded)>LIMIT: raise ValueError('Monument document exceeds 32 MiB')
        args.cache_root.mkdir(parents=True,exist_ok=True);destination=args.cache_root/hashlib.sha256(encoded).hexdigest()
        if destination.exists(): raise ValueError('Monument snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.monuments-',dir=args.cache_root) as temporary:
            stage=Path(temporary);(stage/'monuments.json').write_bytes(encoded);publish_directory(stage, destination)
        print(destination/'monuments.json',flush=True)
    finally:
        for stream in streams: stream.close()


if __name__ == '__main__': main()
