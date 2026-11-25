"""Extract local static decoration LODs; no game geometry is distributed with Mapper."""
import argparse
import hashlib
import json
import os
import tempfile
from pathlib import Path
from mapper_paths import publish_directory
from UnityPy.classes.PPtr import PPtr
from extract_verified_prefabs import snapshot, fingerprint
from decor_collider_meshes import MeshSources
from prefab_geometry import IDENTITY, matrix, multiply
from static_preview_models import extract
from mapper_paths import data_root
from gameplay_sources import verify_code


def read_limited(path, limit):
    with path.open('rb') as stream:
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise ValueError('Decoration model input exceeds byte budget')
    return raw


def verify_source(root, source):
    schema = source.get('schema')
    adapters = {
        2: 'papyrus-static-decor-2',
        3: 'papyrus-static-decor-3',
        4: 'papyrus-static-decor-collider-4',
        5: 'papyrus-static-decor-collider-5',
        6: 'papyrus-static-decor-collider-6',
    }
    if (source.get('format') != 'papyrus-decor-source'
            or type(schema) is not int or schema not in adapters
            or source.get('adapter') != adapters[schema]
            or (source.get('stage_layout') != 'mode-description' if schema == 6
                else 'stage_layout' in source)):
        raise ValueError('Unsupported decoration model source revision or layout')
    code = source.get('code_sources', [])
    if not isinstance(code, list) or bool(code) != (schema >= 5):
        raise ValueError('Decoration model source gameplay receipts differ from revision')
    verify_code(root, source, label='Decoration model')
    sources = snapshot(root)
    if source.get('sources') != sources + [fingerprint(root, 'Bundles/maps/maps.bundle', 512*1024**2)]:
        raise ValueError('Decoration source differs from selected installation')
    extra = source.get('extra_sources', [])
    if not isinstance(extra, list) or len(extra) > 2 or schema < 4 and extra:
        raise ValueError('Invalid decoration contributor source roster')
    seen = set()
    for receipt in extra:
        if not isinstance(receipt, dict) or set(receipt) != {'relative_path', 'sha256', 'bytes'}:
            raise ValueError('Invalid decoration contributor receipt')
        path = receipt['relative_path']
        if (not isinstance(path, str)
                or path not in ('Bundles/shared/content.bundle', 'Bundles/shared/monuments.bundle')
                or path in seen or fingerprint(root, path, 8*1024**3) != receipt):
            raise ValueError('Decoration contributor source differs from selected installation')
        seen.add(path)
    return sources


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--cache-root', type=Path, default=data_root()/'papyrus-decor-model-cache')
    args = parser.parse_args()
    raw = read_limited(args.source, 16*1024**2)
    source = json.loads(raw)
    sources = verify_source(args.rust_root, source)
    targets = set(source['prefabs'])
    if not 1 <= len(targets) <= 4096: raise ValueError('Invalid decoration roster')
    manifest = json.loads(read_limited(args.rust_root/'Bundles/AssetSceneManifest.json',32*1024**2))
    scenes = {}
    for scene in manifest['Scenes']:
        for path in scene['IncludedAssets']:
            path = path.lower()
            if path not in targets: continue
            if path in scenes: raise ValueError('Ambiguous decoration scene')
            scenes[path] = 'BuildPlayer-'+scene['Name']
    if set(scenes) != targets: raise ValueError('Decoration scene coverage mismatch')
    scripts = [Path(__file__).with_name(n) for n in ('extract_decor_models.py','static_preview_models.py','decor_collider_meshes.py','extract_verified_prefabs.py','lazy_unity_bundle.py','prefab_geometry.py','model_preview_geometry.py','gameplay_sources.py')]
    def script_hash(): return hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
    before = script_hash()
    from lazy_unity_bundle import load
    env, stream = load(args.rust_root/'Bundles/shared/assetscenes.bundle',
                       nodes=set(scenes.values()), follow_dependencies=True)
    mesh_sources = MeshSources(args.rust_root, env, False)
    models = {}
    try:
        for asset in env.assets:
            names = {p for p, scene in scenes.items() if scene == asset.name}
            if not names: continue
            def ptr(ref):
                if ref['m_FileID'] != 0: raise ValueError('External decoration hierarchy')
                return PPtr(**ref, assetsfile=asset).deref()
            for obj in asset.objects.values():
                if int(obj.type) != 1: continue
                name = obj.read().m_Name.lower()
                if name not in names: continue
                if name in models: raise ValueError('Duplicate decoration root')
                nodes=[]; seen=set(); issues=[]
                def walk(go, parent=IDENTITY, active=True, depth=0):
                    if depth>128 or len(seen)>=65536 or go.path_id in seen: raise ValueError('Invalid decoration hierarchy')
                    seen.add(go.path_id); fields=go.read_typetree(); root=go.path_id==obj.path_id
                    active=active and (root or bool(fields['m_IsActive']))
                    records=[ptr(c['component']) for c in fields['m_Component']]
                    transforms=[o.read_typetree() for o in records if int(o.type)==4]
                    if len(transforms)!=1: raise ValueError('Missing static decoration Transform')
                    transform=parent if root else multiply(parent,matrix(transforms[0]))
                    for record in records:
                        if int(record.type)!=114 or not active: continue
                        data=record.read_typetree()
                        script=record.read().m_Script.read_typetree()
                        if script['m_AssemblyName']!='Assembly-CSharp' or script['m_Namespace']!='':continue
                        kind=script['m_ClassName']
                        if not root and kind.startswith('Decor'):
                            issues.append('Nested '+kind+' uses authored preview transform; runtime assembly pending')
                        if kind=='AddToHeightMap' and data.get('DestroyGameObject'):
                            active=False
                    nodes.append((records,transform,active,depth))
                    for child in transforms[0]['m_Children']:
                        walk(ptr(ptr(child).read_typetree()['m_GameObject']),transform,active,depth+1)
                walk(obj)
                def resolve(asset, ref):
                    selected, identifier=mesh_sources.resolve(asset,ref)
                    return selected.objects[identifier]
                model=extract(nodes,resolve,asset,256)
                model['issues']=sorted(set(model['issues']+issues))[:256]
                model['complete']=bool(model['triangles']) and not model['issues']
                models[name]=model
                print(name,len(model['triangles']),len(model['issues']),flush=True)
        if set(models)!=targets: raise ValueError('Incomplete decoration model roster')
        if snapshot(args.rust_root)!=sources or read_limited(args.source,16*1024**2)!=raw or script_hash()!=before: raise ValueError('Decoration model inputs changed during extraction')
        verify_code(args.rust_root, source, label='Decoration model')
        mesh_sources.verify()
        if fingerprint(args.rust_root,'Bundles/maps/maps.bundle',512*1024**2)!=source['sources'][3]: raise ValueError('Map scene changed during model extraction')
        doc=dict(format='papyrus-decor-model-source',schema=1,adapter='papyrus-decor-static-lod-1',decoration_sha256=hashlib.sha256(raw).hexdigest(),sources=sources,extra_sources=list(mesh_sources.sources.values()),extractor_sha256=before,assembly_validated=False,models=models)
        encoded=json.dumps(doc,separators=(',',':'),allow_nan=False).encode()
        if len(encoded)>32*1024**2: raise ValueError('Decoration model cache exceeds 32 MiB')
        args.cache_root.mkdir(parents=True,exist_ok=True)
        destination=args.cache_root/hashlib.sha256(encoded).hexdigest()
        if destination.exists(): raise ValueError('Decoration model snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.decor-models-',dir=args.cache_root) as temporary:
            stage=Path(temporary)
            with (stage/'models.json').open('xb') as f: f.write(encoded); f.flush(); os.fsync(f.fileno())
            publish_directory(stage, destination)
        print(destination/'models.json',flush=True)
    finally:
        mesh_sources.close(); stream.close()

if __name__=='__main__': main()
