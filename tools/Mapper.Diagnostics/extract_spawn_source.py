"""Extract source-bound terrain spawn filters; never instantiate game populations."""
import argparse,hashlib,json,os,tempfile,math
from pathlib import Path
from mapper_paths import publish_directory
from UnityPy.classes.PPtr import PPtr
from lazy_unity_bundle import load
from extract_verified_prefabs import snapshot,fingerprint
from mapper_paths import data_root
LIMIT=4*1024**2

def read_json(path):
    with path.open('rb') as f:raw=f.read(LIMIT+1)
    if len(raw)>LIMIT:raise ValueError('Spawn metadata input exceeds 4 MiB')
    return json.loads(raw)

def filter_data(fields):
    names={'SplatType','BiomeType','TopologyAny','TopologyAll','TopologyNot'}
    if not isinstance(fields,dict) or set(fields)!=names or any(type(fields[n]) is not int or not -2**31<=fields[n]<2**31 for n in names):raise ValueError('Unknown spawn filter layout')
    if fields['TopologyAny']==0 or fields['SplatType']==0 or fields['BiomeType']==0:raise ValueError('Empty spawn filter mask')
    return fields

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root',type=Path,required=True);parser.add_argument('--profile',type=Path,required=True)
    parser.add_argument('--cache-root',type=Path,default=data_root()/'papyrus-spawn-cache')
    args=parser.parse_args();profile=read_json(args.profile)
    source=snapshot(args.rust_root)
    source.append(fingerprint(args.rust_root,'Bundles/shared/content.bundle',8*1024**3))
    if profile['native_sha256']!=source[0]['sha256'] or profile['scene_sha256']!=source[2]['sha256']:raise ValueError('Topology profile differs from the selected build')
    scripts=[Path(__file__).with_name(n) for n in ('extract_spawn_source.py','extract_verified_prefabs.py','lazy_unity_bundle.py','prefab_geometry.py')]
    def script_hash():return hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
    before=script_hash()
    manifest=read_json(args.rust_root/'Bundles/AssetSceneManifest.json')
    target=profile['handler_prefab']
    assigned=[s['Name'] for s in manifest['Scenes'] if any(p.lower()==target for p in s['IncludedAssets'])]
    if len(assigned)!=1:raise ValueError('Spawn handler needs one exact AssetScene assignment')
    scenes,scene_stream=load(args.rust_root/'Bundles/shared/assetscenes.bundle')
    content,content_stream=load(args.rust_root/'Bundles/shared/content.bundle')
    for a in content.assets:scenes.register_cab(a.name,a)
    for a in scenes.assets:content.register_cab(a.name,a)
    asset=next(a for a in scenes.assets if a.name=='BuildPlayer-'+assigned[0])
    roots=[o for o in asset.objects.values() if int(o.type)==1 and o.read().m_Name.lower()==target]
    if len(roots)!=1:raise ValueError('Missing or ambiguous source spawn prefab')
    go=roots[0].read_typetree();handlers=[]
    def identity(obj):
        s=obj.read().m_Script.read_typetree();return (s['m_AssemblyName'],s['m_Namespace'],s['m_ClassName'])
    for component in go['m_Component']:
        obj=PPtr(**component['component'],assetsfile=asset).deref()
        if int(obj.type)==114 and identity(obj)==('Assembly-CSharp','','SpawnHandler'):handlers.append(obj)
    if len(handlers)!=1:raise ValueError('Expected one exact SpawnHandler script')
    handler=handlers[0].read_typetree()
    filters=[{'name':'CharacterSpawn','kind':'character','filter':filter_data(handler['CharacterSpawn']), 'cutoff':handler['CharacterSpawnCutoff'],'radius':0.}]
    populations=[];issues=[];seen=set()
    refs=handler['SpawnPopulations']+handler.get('ConvarSpawnPopulations',[])
    if len(refs)>256:raise ValueError('Excessive source population references')
    for ref in refs:
        if ref.get('m_PathID')==0:
            issues.append('Null population reference');continue
        try:
            obj=PPtr(**ref,assetsfile=asset).deref();key=(obj.assets_file.name,obj.path_id)
            if key in seen:continue
            seen.add(key);f=obj.read_typetree();kind=identity(obj)[2];name=f.get('m_Name','')
            print(name,kind,flush=True)
            record={'name':name,'type':kind,'object_id':obj.path_id}
            if kind in ('DensitySpawnPopulation','ConvarControlledSpawnPopulation','ConvarControlledSpawnPopulationRail'):
                filters.append({'name':name,'kind':'population','filter':filter_data(f['Filter']),'cutoff':f['FilterCutoff'],'radius':f['FilterRadius']})
                record['terrain_filter_extracted']=True
            else:record['terrain_filter_extracted']=False
            populations.append(record)
        except (ValueError,KeyError,TypeError,FileNotFoundError) as error:issues.append(str(error)[:256])
    if len(filters)>128:raise ValueError('Excessive terrain spawn filters')
    if source!=snapshot(args.rust_root)+[fingerprint(args.rust_root,'Bundles/shared/content.bundle',8*1024**3)] or before!=script_hash():raise ValueError('Source changed during extraction')
    document={'format':'papyrus-spawn-source','schema':1,'adapter':'papyrus-spawn-filters-1','sources':source,'extractor_sha256':before,'profile':profile,
        'filters':filters,'populations':populations,'issues':issues,'runtime_checks':{'placement_check_height':handler['PlacementCheckHeight'],'radius_check_distance':handler['RadiusCheckDistance'],'monument_clearance':profile['monument_clearance'],'physics_validated':False}}
    encoded=json.dumps(document,indent=2,allow_nan=False).encode()
    if len(encoded)>LIMIT:raise ValueError('Spawn source exceeds 4 MiB')
    args.cache_root.mkdir(parents=True,exist_ok=True);destination=args.cache_root/hashlib.sha256(encoded).hexdigest()
    if destination.exists():raise ValueError('Spawn snapshot already exists; refusing overwrite')
    with tempfile.TemporaryDirectory(prefix='.spawn-',dir=args.cache_root) as temp:
        stage=Path(temp)
        with (stage/'spawn.json').open('xb') as f:f.write(encoded);f.flush();os.fsync(f.fileno())
        publish_directory(stage, destination)
    print(destination/'spawn.json')
if __name__=='__main__':main()
