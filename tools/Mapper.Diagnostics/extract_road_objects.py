"""Capture installation-bound road object rules and authored prefab attributes.

Does not place prefabs, fabricate mesh data, or change existing snapshots.
"""
import argparse,hashlib,json,os,tempfile,gc
from pathlib import Path
from mapper_paths import publish_directory
from UnityPy.classes.PPtr import PPtr
from road_object_stages import stages,GROUPS,guid_reference,placement_policy_data
from resolve_prefab_textures import container_index,unique_asset,guid_paths
from extract_verified_prefabs import fingerprint,snapshot
from extract_decor_source import numeric
from prefab_geometry import IDENTITY,matrix,multiply,point,vector
from lazy_unity_bundle import load
from mapper_paths import data_root
from road_stages import source_layout
from monument_placement_attributes import attribute_fields

LIMIT=16*1024**2
def bounded(path):
    with path.open('rb') as f:raw=f.read(LIMIT+1)
    if len(raw)>LIMIT:raise ValueError('Road object input exceeds 16 MiB')
    return raw

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rust-root',type=Path,required=True)
    p.add_argument('--infrastructure',type=Path,required=True)
    p.add_argument('--placement-profile',type=Path)
    p.add_argument('--cache-root',type=Path,default=data_root()/'papyrus-road-object-cache')
    a=p.parse_args();raw=bounded(a.infrastructure);parent=json.loads(raw)
    layout=source_layout(parent)
    profile=parent['profile']
    if not profile.get('code_sources') or not profile.get('map_scene'):raise ValueError('Road object capture requires bound gameplay and map scene receipts')
    receipts=parent['sources']+profile['code_sources']+[profile['map_scene']]
    if len(receipts)>8 or len({s['relative_path'] for s in receipts})!=len(receipts):raise ValueError('Invalid parent receipts')
    allowed={'RustNative.dll','RustClient_Data/Plugins/x86_64/RustNative.dll','RustDedicated_Data/Plugins/x86_64/RustNative.dll','Bundles/AssetSceneManifest.json','Bundles/shared/assetscenes.bundle','GameAssembly.dll','RustClient_Data/il2cpp_data/Metadata/global-metadata.dat','RustClient_Data/Managed/Assembly-CSharp.dll','RustDedicated_Data/Managed/Assembly-CSharp.dll','Bundles/maps/maps.bundle'}
    def verify():
        if any(s['relative_path'] not in allowed for s in receipts):raise ValueError('Unsupported source receipt path')
        if any(fingerprint(a.rust_root,s['relative_path'],8*1024**3)!=s for s in receipts):raise ValueError('Road object source differs from parent installation')
    placement_raw=None;placement_policy=None
    if a.placement_profile:
        placement_raw=bounded(a.placement_profile);placement=json.loads(placement_raw)
        if placement.get('format')!='papyrus-road-object-profile' or placement.get('schema')!=1 or placement.get('native_sha256')!=profile['native_sha256'] or placement.get('scene_sha256')!=profile['scene_sha256'] or placement.get('code_sources')!=profile['code_sources']:
            raise ValueError('Road object placement policy belongs to another source build')
        if placement.get('stage_layout')!=profile.get('stage_layout'):
            raise ValueError('Road object placement policy has a different scene layout')
        placement_policy=placement_policy_data(placement['policy'])
    verify()
    if snapshot(a.rust_root)!=parent['sources']:raise ValueError('Parent prefab source differs from installation')
    scripts=[Path(__file__).with_name(n) for n in ('extract_road_objects.py','road_object_stages.py','extract_decor_source.py','prefab_geometry.py','lazy_unity_bundle.py','extract_verified_prefabs.py','resolve_prefab_textures.py','road_stages.py','monument_placement_attributes.py')]
    def script_hash():return hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
    before=script_hash();rows,workflow=stages(a.rust_root,include_workflow=True,stage_layout=layout)
    manifest=json.loads(bounded(a.rust_root/'Bundles/AssetSceneManifest.json'))
    targets={};issues=[]
    for stage in rows:
        for group in GROUPS:
            for rule in stage[group.lower()]:
                if not rule['folder']:continue
                prefix='assets/bundled/prefabs/autospawn/'+rule['folder']+'/'
                matches=[(s['Name'],path.lower()) for s in manifest['Scenes'] for path in s['IncludedAssets'] if path.lower().startswith(prefix) and path.lower().endswith('.prefab')]
                rule['prefabs']=sorted(path for _,path in matches)
                if not matches:issues.append('Empty road object folder: '+rule['folder'])
                for scene,path in matches:
                    if path in targets and targets[path]!=scene:raise ValueError('Ambiguous road prefab assignment')
                    targets[path]=scene
    if not 1<=len(targets)<=4096:raise ValueError('Unsupported road object prefab roster')
    selected={'BuildPlayer-'+scene for scene in targets.values() if isinstance(scene,str) and scene}
    env,stream=load(a.rust_root/'Bundles/shared/assetscenes.bundle',
                    nodes=selected or None,follow_dependencies=bool(selected));prefabs={}
    try:
        for asset in env.assets:
            names={path for path,scene in targets.items() if asset.name=='BuildPlayer-'+scene}
            if not names:continue
            def ptr(ref):
                if ref['m_FileID']!=0:raise ValueError('External road prefab hierarchy')
                return PPtr(**ref,assetsfile=asset).deref()
            for obj in asset.objects.values():
                if int(obj.type)!=1 or obj.read().m_Name.lower() not in names:continue
                name=obj.read().m_Name.lower()
                if name in prefabs:raise ValueError('Duplicate road prefab root')
                attrs=[];components={};visited=set();base={};problems=[]
                def walk(go,parent=IDENTITY,active=True,depth=0):
                    if depth>128 or go.path_id in visited or len(visited)>=65536:raise ValueError('Invalid road prefab hierarchy')
                    visited.add(go.path_id);f=go.read_typetree();root=go.path_id==obj.path_id
                    active=active and (root or bool(f['m_IsActive']))
                    objects=[ptr(c['component']) for c in f['m_Component']]
                    ts=[o.read_typetree() for o in objects if int(o.type)==4]
                    if len(ts)!=1:raise ValueError('Ambiguous road prefab transform')
                    t=ts[0];transform=parent if root else multiply(parent,matrix(t))
                    if root:base.update(rotation=vector(t['m_LocalRotation'],'xyzw'),scale=vector(t['m_LocalScale']))
                    for record in objects:
                        if int(record.type)!=114 or not active:continue
                        script=record.read().m_Script.read_typetree()
                        if (script['m_AssemblyName'],script['m_Namespace'])!=('Assembly-CSharp',''):continue
                        kind=script['m_ClassName'];components[kind]=components.get(kind,0)+1
                        if not (kind.startswith(('Decor','Terrain')) or kind.lower().startswith('powerline') or 'Sequence' in kind or kind in ('EnvironmentVolumeCheck','PrefabParameters','AddToHeightMap','SpawnableBoundsBlocker','RandomDestroy','DestroyIfInMonumentNoBuildZone')):continue
                        values={};references=[]
                        fields=record.read_typetree()
                        if layout=='mode-description':fields=attribute_fields(fields)
                        for key,value in fields.items():
                            if key.startswith('m_'):continue
                            try:values[key]=numeric(value)
                            except ValueError:
                                guid=guid_reference(value)
                                if guid is not None:references.append(dict(field=key,guid=guid))
                                else:problems.append(kind+'.'+key+': unresolved field')
                        attrs.append(dict(kind=kind,root=root,position=point(transform,[0.,0.,0.]),local_to_root=[list(row) for row in transform],fields=values,references=references))
                    for child in t['m_Children']:walk(ptr(ptr(child).read_typetree()['m_GameObject']),transform,active,depth+1)
                walk(obj)
                if len(attrs)>4096 or len(problems)>1024:raise ValueError('Excessive road prefab attributes')
                prefabs[name]=dict(**base,attributes=attrs,components=components,issues=problems)
                print(name,len(attrs),flush=True)
    finally:stream.close()
    for path in targets.keys()-prefabs.keys():issues.append('Missing road prefab root: '+path)
    del env,stream
    gc.collect()
    required={ref['guid'] for p in prefabs.values() for attr in p['attributes'] for ref in attr['references']}
    resources={};extra_sources=[]
    if required:
        extra_sources=[fingerprint(a.rust_root,'Bundles/shared/content.bundle',8*1024**3)]
        content,content_stream=load(a.rust_root/'Bundles/shared/content.bundle')
        try:
            index=container_index(content);manifest=unique_asset(index,'assets/manifest.asset')
            if manifest is None:raise ValueError('Missing exact GameManifest for road object references')
            resources=guid_paths(manifest.read_typetree(),required)
        finally:content_stream.close()
        for prefab in prefabs.values():
            for attr in prefab['attributes']:
                for ref in attr['references']:
                    if ref['guid'] not in resources:prefab['issues'].append(attr['kind']+'.'+ref['field']+': GUID absent from GameManifest')
        if extra_sources!=[fingerprint(a.rust_root,'Bundles/shared/content.bundle',8*1024**3)]:raise ValueError('GameManifest source changed during extraction')
    verify()
    if bounded(a.infrastructure)!=raw or script_hash()!=before or (a.placement_profile and bounded(a.placement_profile)!=placement_raw):raise ValueError('Sources changed during road object extraction')
    doc=dict(format='papyrus-road-object-source',schema=2,adapter='papyrus-road-objects-1',infrastructure_sha256=hashlib.sha256(raw).hexdigest(),sources=receipts,extra_sources=extra_sources,resources=resources,extractor_sha256=before,stages=rows,workflow=workflow,prefabs=prefabs,issues=issues)
    if layout=='mode-description':
        doc.update(schema=3,adapter='papyrus-road-objects-2',stage_layout=layout)
    if placement_policy is not None:doc.update(path_policy=placement_policy,placement_profile_sha256=hashlib.sha256(placement_raw).hexdigest())
    encoded=json.dumps(doc,indent=2,allow_nan=False).encode()
    if len(encoded)>LIMIT:raise ValueError('Road object metadata exceeds 16 MiB')
    a.cache_root.mkdir(parents=True,exist_ok=True);destination=a.cache_root/hashlib.sha256(encoded).hexdigest()
    if destination.exists():raise ValueError('Road object snapshot exists; refusing overwrite')
    with tempfile.TemporaryDirectory(prefix='.road-objects-',dir=a.cache_root) as temporary:
        stage=Path(temporary)
        with (stage/'objects.json').open('xb') as f:f.write(encoded);f.flush();os.fsync(f.fileno())
        publish_directory(stage, destination)
    print(destination/'objects.json',flush=True)
if __name__=='__main__':main()
