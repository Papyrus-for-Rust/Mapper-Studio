"""Capture source-bound surface rail stage order/settings; does not generate a rail network."""
import argparse,hashlib,json,os,tempfile
from pathlib import Path
from mapper_paths import publish_directory
from UnityPy.classes.PPtr import PPtr
from lazy_unity_bundle import load
from extract_verified_prefabs import snapshot,fingerprint
from mapper_paths import data_root
from road_stages import activation, source_layout as infrastructure_layout
from gameplay_sources import verify_code
KINDS=('GenerateRailRing','GenerateRailLayout','GenerateRailBranching','GenerateRailSiding','GenerateRailTerrain','GenerateRailTopology','GenerateRailTexture','GenerateRailMeshes','CollateTrainTracks')
LIMIT=4*1024**2

def source_layout(source):
    if not isinstance(source, dict) or not isinstance(source.get('profile'), dict):
        raise ValueError('Unsupported rail source')
    layout = source['profile'].get('stage_layout')
    if (source.get('format') != 'papyrus-rail-source'
            or type(source.get('schema')) is not int
            or (source['schema'], source.get('adapter'), layout) not in (
                (1, 'papyrus-surface-rail-stages-1', None),
                (2, 'papyrus-surface-rail-stages-2', 'mode-description'))):
        raise ValueError('Rail stage layout differs from its source revision')
    return 'legacy' if layout is None else layout

def bounded(path,limit=LIMIT):
    with path.open('rb') as stream:raw=stream.read(limit+1)
    if len(raw)>limit:raise ValueError('Rail metadata input exceeds 4 MiB')
    return raw

def stage_data(identity,fields,identifier,order,active,*,layout='legacy'):
    if identity[:2]!=('Assembly-CSharp','') or identity[2] not in KINDS:raise ValueError('Unsupported rail component identity')
    kind=identity[2]
    extra=({'MinWorldSize'} if kind=='GenerateRailRing' else {'RailMesh','RailMeshes','RailMaterial','RailPhysicMaterial'} if kind=='GenerateRailMeshes' else set())
    flags=activation(fields,extra,layout)
    minimum=fields.get('MinWorldSize')
    if minimum is not None and (type(minimum) is not int or not 0<=minimum<=6000):raise ValueError('Unsupported ring minimum size')
    return dict(kind=kind,object_id=identifier,order=order,**flags,hierarchy_active=active,minimum_world_size=minimum,description=fields['Description'],assets=[])

def reference(fields,scene):
    file_id,path_id=fields['m_FileID'],fields['m_PathID']
    if type(file_id) is not int or type(path_id) is not int or file_id<0 or file_id>len(scene.externals):raise ValueError('Invalid rail asset reference')
    if path_id==0:
        if file_id:raise ValueError('Invalid null rail reference')
        return None
    return dict(file_id=file_id,path_id=path_id,external_path=scene.externals[file_id-1].path if file_id else '')

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root',type=Path,required=True);parser.add_argument('--catalog',type=Path,required=True)
    parser.add_argument('--infrastructure',type=Path,required=True);parser.add_argument('--profile',type=Path,required=True)
    parser.add_argument('--cache-root',type=Path,default=data_root()/'papyrus-rail-cache')
    args=parser.parse_args()
    with args.catalog.open('rb') as f:catalog_raw=f.read(16*1024**2+1)
    if len(catalog_raw)>16*1024**2:raise ValueError('Catalog exceeds bound')
    catalog=json.loads(catalog_raw);infra_raw=bounded(args.infrastructure,16*1024**2);infra=json.loads(infra_raw);profile_raw=bounded(args.profile);profile=json.loads(profile_raw)
    sources=snapshot(args.rust_root)
    if catalog.get('format')!='papyrus-prefab-catalog' or catalog.get('sources')!=sources or infra.get('format')!='papyrus-infrastructure-source' or infra['sources']!=sources or infra['catalog_sha256']!=hashlib.sha256(catalog_raw).hexdigest():raise ValueError('Rail connector/catalog source mismatch')
    if profile.get('format')!='papyrus-rail-profile' or profile['native_sha256']!=sources[0]['sha256'] or profile['scene_sha256']!=sources[2]['sha256']:raise ValueError('Rail profile differs from selected installation')
    layout=profile.get('stage_layout','legacy')
    if layout not in ('legacy','mode-description') or layout!=infrastructure_layout(infra):raise ValueError('Rail and infrastructure stage layouts differ')
    code_sources=verify_code(args.rust_root,profile,label='Rail')
    if not code_sources:raise ValueError('Missing bounded gameplay receipts')
    sources+=[fingerprint(args.rust_root,'Bundles/maps/maps.bundle',512*1024**2)]
    scripts=[Path(__file__).with_name(n) for n in ('extract_rail_source.py','lazy_unity_bundle.py','extract_verified_prefabs.py','road_stages.py','gameplay_sources.py')]
    def implementation():return hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
    before=implementation();env,stream=load(args.rust_root/'Bundles/maps/maps.bundle')
    try:
        scenes=[a for a in env.assets if a.name=='BuildPlayer-Procedural Map']
        if len(scenes)!=1:raise ValueError('Missing exact procedural map scene')
        scene=scenes[0]
        def identity(o):
            f=o.read().m_Script.read_typetree();return f['m_AssemblyName'],f['m_Namespace'],f['m_ClassName']
        setups=[o for o in scene.objects.values() if int(o.type)==114 and identity(o)==('Assembly-CSharp','','WorldSetup')]
        if len(setups)!=1:raise ValueError('Ambiguous WorldSetup')
        def ptr(ref):
            if ref['m_FileID']!=0:raise ValueError('External procedural hierarchy')
            return PPtr(**ref,assetsfile=scene).deref()
        stages=[];seen=set()
        def walk(go,active=True,depth=0):
            if depth>128 or go.path_id in seen or len(seen)>=65536:raise ValueError('Invalid rail hierarchy')
            seen.add(go.path_id);f=go.read_typetree();active=active and bool(f['m_IsActive'])
            records=[ptr(c['component']) for c in f['m_Component']]
            for record in records:
                if int(record.type)!=114:continue
                ident=identity(record)
                if not ident[2].startswith('GenerateRail') and ident[2]!='CollateTrainTracks':continue
                fields=record.read_typetree();row=stage_data(ident,fields,record.path_id,len(stages),active,layout=layout)
                if ident[2]=='GenerateRailMeshes':
                    for field in ('RailMesh','RailMeshes','RailMaterial','RailPhysicMaterial'):
                        values=fields[field] if field=='RailMeshes' else [fields[field]]
                        if not isinstance(values,list) or len(values)>16:raise ValueError('Rail resource roster exceeds bounds')
                        for index,value in enumerate(values):
                            ref=reference(value,scene)
                            if ref is not None:row['assets'].append(dict(field=field,index=index,**ref))
                stages.append(row)
            transform=next(o.read_typetree() for o in records if int(o.type)==4)
            for child in transform['m_Children']:walk(ptr(ptr(child).read_typetree()['m_GameObject']),active,depth+1)
        walk(ptr(setups[0].read_typetree()['m_GameObject']))
        if not stages or len(stages)>64 or len({s['kind'] for s in stages})!=len(stages):raise ValueError('Unexpected surface rail stage coverage')
        if snapshot(args.rust_root)+[fingerprint(args.rust_root,'Bundles/maps/maps.bundle',512*1024**2)]!=sources or bounded(args.infrastructure,16*1024**2)!=infra_raw or bounded(args.profile)!=profile_raw or implementation()!=before:raise ValueError('Rail extraction source changed')
        if verify_code(args.rust_root,profile,label='Rail')!=code_sources:raise ValueError('Gameplay code changed during extraction')
        # The roster alone is not a rail network, cooked collider or resource decoder.
        doc=dict(format='papyrus-rail-source',schema=1,adapter='papyrus-surface-rail-stages-1',sources=sources,code_sources=code_sources,catalog_sha256=hashlib.sha256(catalog_raw).hexdigest(),infrastructure_sha256=hashlib.sha256(infra_raw).hexdigest(),profile_sha256=hashlib.sha256(profile_raw).hexdigest(),extractor_sha256=before,profile=profile,stages=stages)
        if layout=='mode-description':doc.update(schema=2,adapter='papyrus-surface-rail-stages-2')
        raw=json.dumps(doc,separators=(',',':'),allow_nan=False).encode()
        if len(raw)>LIMIT:raise ValueError('Rail snapshot exceeds bound')
        args.cache_root.mkdir(parents=True,exist_ok=True);destination=args.cache_root/hashlib.sha256(raw).hexdigest()
        if destination.exists():raise ValueError('Rail snapshot exists; refusing overwrite')
        with tempfile.TemporaryDirectory(prefix='.rails-',dir=args.cache_root) as temporary:
            stage=Path(temporary)
            with (stage/'rails.json').open('xb') as f:f.write(raw);f.flush();os.fsync(f.fileno())
            publish_directory(stage, destination)
        print(destination/'rails.json',flush=True)
    finally:stream.close()
if __name__=='__main__':main()
