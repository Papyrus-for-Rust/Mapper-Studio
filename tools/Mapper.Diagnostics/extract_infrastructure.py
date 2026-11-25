"""Publish source-bound numeric infrastructure connectors without changing existing asset caches."""
import argparse,hashlib,json,os,tempfile,time
from pathlib import Path
from extract_verified_prefabs import snapshot,fingerprint
from road_stages import layout_stages
from lazy_unity_bundle import load
from prefab_paths import inspect, component_readers
from mapper_paths import data_root, publish_directory

LIMIT=16*1024**2

def publish(cache_root, encoded):
    digest = hashlib.sha256(encoded).hexdigest()
    cache_root.mkdir(parents=True, exist_ok=True)
    destination = cache_root / digest
    if destination.exists():
        raise ValueError('Snapshot already exists; refusing overwrite')
    with tempfile.TemporaryDirectory(prefix='.infrastructure-', dir=cache_root) as temporary:
        stage = Path(temporary)
        with (stage / 'infrastructure.json').open('xb') as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        publish_directory(stage, destination)
    return destination / 'infrastructure.json'

def bounded(path):
    with path.open('rb') as stream:raw=stream.read(LIMIT+1)
    if len(raw)>LIMIT:raise ValueError("Infrastructure input exceeds 16 MiB")
    return raw

def generation_stages(root, profile, stage_layout):
    """Read one procedural scene and retain each published stage's ordering."""
    has_layout = profile.get('trails') is not None
    has_ring = profile.get('ring') is not None
    has_topology = profile.get('post_topology') is not None
    if not (has_layout or has_ring or has_topology):
        return [], None, None
    inventory = layout_stages(root, include_ring=has_ring or has_topology,
                              include_topology=has_topology, stage_layout=stage_layout)
    stages = []
    if has_layout:
        for row in inventory:
            if row.get('kind', 'layout') != 'layout':
                continue
            stage = {key: value for key, value in row.items() if key != 'kind'}
            stage['order'] = len(stages)
            stages.append(stage)
        if ([s['infrastructure_type'] for s in stages] != profile['layout_order']
                or any(s['mode'] != profile['layout_mode'] or s['disabled'] is True
                       or not s['component_enabled'] or not s['hierarchy_active'] for s in stages)):
            raise ValueError('Unsupported ordered road layout stages')
    ring_stage = None
    if has_ring:
        ring_inventory = [row for row in inventory if row.get('kind') != 'topology']
        if ([s['kind'] for s in ring_inventory] != ['ring', 'layout', 'layout']
                or [s['infrastructure_type'] for s in ring_inventory[1:]] != profile['layout_order']):
            raise ValueError('Unsupported ring before access/trail scene order')
        ring_stage = {key: value for key, value in ring_inventory[0].items() if key != 'kind'}
        if (ring_stage['minimum_world_size'] != profile['ring']['minimum_world_size']
                or ring_stage['mode'] != -1 or ring_stage['disabled']
                or not ring_stage['component_enabled'] or not ring_stage['hierarchy_active']):
            raise ValueError('Road ring policy/scene mismatch')
    topology_stage = None
    if has_topology:
        if [s['kind'] for s in inventory] != ['ring', 'layout', 'layout', 'topology']:
            raise ValueError('Unsupported ordered road topology stage')
        topology_stage = {key: value for key, value in inventory[-1].items() if key != 'kind'}
        if (topology_stage['mode'] != -1 or topology_stage['disabled']
                or not topology_stage['component_enabled'] or not topology_stage['hierarchy_active']):
            raise ValueError('Road topology stage is inactive or unsupported')
    return stages, ring_stage, topology_stage


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust-root',type=Path,required=True)
    parser.add_argument('--catalog',type=Path,required=True)
    parser.add_argument('--profile',type=Path,required=True)
    parser.add_argument('--cache-root',type=Path,default=data_root()/'papyrus-infrastructure-cache')
    args=parser.parse_args()
    raw=bounded(args.catalog);catalog=json.loads(raw)
    source=snapshot(args.rust_root)
    if catalog.get('format')!='papyrus-prefab-catalog' or source!=catalog.get('sources'):raise ValueError('Catalog/source mismatch')
    profile_raw=bounded(args.profile);profile=json.loads(profile_raw)
    if profile.get('native_sha256')!=source[0]['sha256'] or profile.get('scene_sha256')!=source[2]['sha256']:
        raise ValueError('Infrastructure profile belongs to another source build')
    stage_layout=profile.get('stage_layout')
    if stage_layout is None:
        stage_layout='legacy'
    elif stage_layout!='mode-description':
        raise ValueError('Unsupported road stage activation layout')
    code_sources=[];map_scene=None
    if profile.get('trails') is not None:
        for expected in profile['code_sources']:
            if expected['relative_path'] not in ('GameAssembly.dll','RustClient_Data/il2cpp_data/Metadata/global-metadata.dat','RustClient_Data/Managed/Assembly-CSharp.dll','RustDedicated_Data/Managed/Assembly-CSharp.dll'):
                raise ValueError('Unsupported road gameplay receipt path')
            actual=fingerprint(args.rust_root,expected['relative_path'],1024**3)
            if actual!=expected:raise ValueError('Road gameplay code differs from profile')
            code_sources.append(actual)
        if not 1<=len(code_sources)<=2:raise ValueError('Missing bounded road gameplay receipts')
        map_scene=fingerprint(args.rust_root,'Bundles/maps/maps.bundle',512*1024**2)
        if map_scene!=profile['map_scene']:raise ValueError('Road map scene differs from profile')
    stages, ring_stage, topology_stage = generation_stages(args.rust_root, profile, stage_layout)
    script_files=[Path(__file__).with_name(n) for n in ('extract_infrastructure.py','road_stages.py','prefab_paths.py','prefab_geometry.py','lazy_unity_bundle.py','extract_verified_prefabs.py')]
    def script_hash():return hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in script_files)).hexdigest()
    before=script_hash()
    targets={p:v for group in ('modifiers','special') for p,v in catalog['metadata'][group].items()}
    if not 1<=len(targets)<=4096:raise ValueError('Invalid prefab roster size')
    selected = {f"BuildPlayer-{value['assetScene']}" for value in targets.values()
                if isinstance(value.get('assetScene'), str) and value['assetScene']}
    if not selected:
        raise ValueError('Catalog has no exact infrastructure scene references')
    env,stream=load(args.rust_root/'Bundles/shared/assetscenes.bundle', nodes=selected, follow_dependencies=True)
    prefabs={}
    for asset in env.assets:
        names={p for p,v in targets.items() if asset.name=='BuildPlayer-'+v.get('assetScene','')}
        if not names:continue
        read, script_type = component_readers(asset.objects)
        for obj in asset.objects.values():
            if int(obj.type)!=1:continue
            name=obj.read().m_Name.lower()
            if name not in names:continue
            if name in prefabs:raise ValueError('Duplicate prefab root')
            prefabs[name]=inspect(obj.path_id,read,script_type)
            print(name, len(prefabs[name]['connectors']),flush=True)
    for p in targets.keys()-prefabs.keys():prefabs[p]={'version':1,'space':'prefab-root-local-unity','complete':False,'connectors':[],'issues':[{'objectId':0,'reason':'Missing prefab root'}],'nodesVisited':0}
    if snapshot(args.rust_root)!=source or script_hash()!=before or bounded(args.profile)!=profile_raw or bounded(args.catalog)!=raw:raise ValueError('Sources changed during extraction')
    if code_sources and (any(fingerprint(args.rust_root,s['relative_path'],1024**3)!=s for s in code_sources) or fingerprint(args.rust_root,'Bundles/maps/maps.bundle',512*1024**2)!=map_scene):
        raise ValueError('Road scene or gameplay code changed during extraction')
    document={'format':'papyrus-infrastructure-source','schema':1,'adapter':'papyrus-path-connectors-1','catalog_sha256':hashlib.sha256(raw).hexdigest(),
        'sources':source,'extractor_sha256':before,'profile':profile,'prefabs':prefabs}
    if stage_layout=='mode-description':
        document.update(schema=2,adapter='papyrus-path-connectors-2')
    if stages:document['layout_stages']=stages
    if ring_stage:document['ring_stage']=ring_stage
    if topology_stage:document['topology_stage']=topology_stage
    encoded=json.dumps(document,indent=2,allow_nan=False).encode()
    if len(encoded)>LIMIT:raise ValueError('Connector document exceeds 16 MiB')
    print(publish(args.cache_root, encoded))
if __name__=='__main__':main()
