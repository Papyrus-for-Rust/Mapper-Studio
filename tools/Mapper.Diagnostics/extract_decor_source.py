"""Extract procedural static-decoration stages and numeric placement attributes.

Runtime populations and client-only patch/foliage spawners are deliberately separate.
"""
import argparse
import hashlib
import json
import math
import os
import tempfile
from pathlib import Path
from mapper_paths import publish_directory
from UnityPy.classes.PPtr import PPtr
from extract_spawn_source import filter_data
from extract_verified_prefabs import snapshot, fingerprint
from lazy_unity_bundle import load
from prefab_geometry import IDENTITY, matrix, multiply, point, vector
from decor_collider_meshes import MeshSources
from road_stages import activation
from monument_placement_attributes import attribute_fields

LIMIT = 16 * 1024**2
KINDS = {"PlaceDecorUniform": "uniform", "PlaceDecorWhiteNoise": "white_noise", "PlaceDecorValueNoise": "value_noise"}

def stage_data(identity, fields, order, object_id, *, layout='legacy'):
    if identity[:2] != ("Assembly-CSharp", "") or identity[2] not in KINDS:
        raise ValueError("Unknown static decoration stage identity")
    folder = fields["ResourceFolder"].lower()
    if not folder.startswith("decor/") or any(p in ("", ".", "..") for p in folder.split("/")) or "\\" in folder or len(folder) > 256:
        raise ValueError("Invalid static decoration resource folder")
    kind = KINDS[identity[2]]
    if layout == 'mode-description':
        extra = {'ResourceFolder', 'Filter', 'FilterMode', 'AnchorMode'}
        extra |= {'ObjectDistance', 'ObjectDithering'} if kind == 'uniform' else {'ObjectDensity'}
        if kind == 'value_noise': extra.add('Cluster')
        flags = activation(fields, extra, layout)
        enabled = flags['component_enabled']
    elif layout == 'legacy':
        enabled = bool(fields['m_Enabled']) and not fields['Disabled']
    else:
        raise ValueError('Unsupported static decoration stage layout')
    return dict(id=str(object_id), order=order, name=fields["Description"], kind=kind, folder=folder,
                enabled=enabled, mode=fields["Mode"],
                filter=filter_data(fields["Filter"]), filter_mode=fields["FilterMode"], anchor_mode=fields["AnchorMode"],
                distance=fields.get("ObjectDistance", 0.), dithering=fields.get("ObjectDithering", 0.),
                density=fields.get("ObjectDensity", 0.), cluster=fields.get("Cluster"), prefabs=[])

def numeric(value):
    if type(value) in (int, bool): return value
    if type(value) is float:
        if not math.isfinite(value): raise ValueError("Nonfinite placement field")
        return value
    if isinstance(value, list): return [numeric(v) for v in value]
    if isinstance(value, dict):
        if "m_PathID" in value: raise ValueError("Unresolved asset reference")
        return {k:numeric(v) for k,v in value.items()}
    raise ValueError("Unsupported placement field")

from mapper_paths import data_root


from gameplay_sources import verify_code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-root",type=Path,required=True)
    parser.add_argument("--topology-profile",type=Path)
    parser.add_argument("--colliders",action="store_true",help="Include exact referenced AddToHeightMap triangle buffers; requires a bound sampling profile")
    parser.add_argument("--scene",default="BuildPlayer-Procedural Map")
    parser.add_argument("--cache-root",type=Path,default=data_root()/"papyrus-decor-cache")
    args=parser.parse_args()
    if args.colliders and args.topology_profile is None:raise ValueError('Collider extraction requires a source-bound profile')
    if len(args.scene)>256:raise ValueError("Invalid procedural scene name")
    sources=snapshot(args.rust_root)+[fingerprint(args.rust_root,"Bundles/maps/maps.bundle",512*1024**2)]
    policy=None;profile_raw=None;code_sources=[];stage_layout='legacy'
    if args.topology_profile is not None:
        profile_raw=args.topology_profile.read_bytes()
        if len(profile_raw)>1024*1024:raise ValueError("Topology profile exceeds bound")
        profile=json.loads(profile_raw)
        if profile.get("format")!="papyrus-topology-profile" or profile.get("native_sha256")!=sources[0]['sha256'] or profile.get("scene_sha256")!=sources[2]['sha256']:raise ValueError("Topology profile differs from selected installation")
        code_sources=verify_code(args.rust_root,profile)
        stage_layout=profile.get('stage_layout','legacy')
        if stage_layout not in ('legacy','mode-description'):
            raise ValueError('Unsupported static decoration stage layout')
        if stage_layout=='mode-description' and (not code_sources or not args.colliders):
            raise ValueError('Current decoration layout requires gameplay receipts and collider capture')
        protected_names=("Monument","Building","Road","Roadside","Rail","Railside","Ocean","River","Lake")
        flags=profile['flags'];protected=0
        for name in protected_names:protected|=flags[name]
        policy=dict(profile_id=profile['id'],profile_sha256=hashlib.sha256(profile_raw).hexdigest(),native_sha256=sources[0]['sha256'],scene_sha256=sources[2]['sha256'],topology_flags=flags,protected_topology=protected)
    scripts=[Path(__file__).with_name(n) for n in ("extract_decor_source.py","extract_spawn_source.py","extract_verified_prefabs.py","lazy_unity_bundle.py","prefab_geometry.py","decor_collider_meshes.py","gameplay_sources.py","road_stages.py","monument_placement_attributes.py")]
    def script_hash():return hashlib.sha256(b"".join(p.name.encode()+b"\0"+p.read_bytes() for p in scripts)).hexdigest()
    before=script_hash()
    manifest=json.loads((args.rust_root/"Bundles/AssetSceneManifest.json").read_bytes())
    maps,map_stream=load(args.rust_root/"Bundles/maps/maps.bundle")
    assets=[a for a in maps.assets if a.name==args.scene]
    if len(assets)!=1:raise ValueError("Missing exact procedural scene")
    scene=assets[0]
    def identity(obj):
        s=obj.read().m_Script.read_typetree()
        return s["m_AssemblyName"],s["m_Namespace"],s["m_ClassName"]
    setups=[o for o in scene.objects.values() if int(o.type)==114 and identity(o)==("Assembly-CSharp","","WorldSetup")]
    if len(setups)!=1:raise ValueError("Expected one WorldSetup")
    root=PPtr(**setups[0].read_typetree()["m_GameObject"],assetsfile=scene).deref()
    stages=[];issues=[];seen=set()
    def stages_walk(go,active=True,depth=0):
        if depth>128 or go.path_id in seen or len(seen)>65536:raise ValueError("Invalid procedural hierarchy")
        seen.add(go.path_id);f=go.read_typetree();active=active and bool(f["m_IsActive"])
        children=[]
        for c in f["m_Component"]:
            o=PPtr(**c["component"],assetsfile=scene).deref()
            if int(o.type)==4:children=o.read_typetree()["m_Children"]
            elif int(o.type)==114:
                ident=identity(o)
                if ident[2].startswith("PlaceDecor"):
                    if ident[:2]!=("Assembly-CSharp","") or ident[2] not in KINDS:
                        issues.append("Unsupported static stage: "+ident[2]);continue
                    row=stage_data(ident,o.read_typetree(),len(stages),o.path_id,layout=stage_layout)
                    row["enabled"]=row["enabled"] and active;stages.append(row)
        for child in children:
            t=PPtr(**child,assetsfile=scene).deref().read_typetree()
            stages_walk(PPtr(**t["m_GameObject"],assetsfile=scene).deref(),active,depth+1)
    stages_walk(root)
    if not 1<=len(stages)<=128:raise ValueError("Unexpected static decoration stage count")
    targets={}
    for stage in stages:
        prefix="assets/bundled/prefabs/autospawn/"+stage["folder"]+"/"
        matches=[(s["Name"],p.lower()) for s in manifest["Scenes"] for p in s["IncludedAssets"] if p.lower().startswith(prefix) and p.lower().endswith(".prefab")]
        stage["prefabs"]=sorted(p for _,p in matches)
        if not matches:issues.append("Empty static prefab folder: "+stage["folder"])
        for asset_name,p in matches:
            if p in targets and targets[p]!=asset_name:raise ValueError("Ambiguous static prefab assignment")
            targets[p]=asset_name
    if len(targets)>4096:raise ValueError("Excessive static prefab roster")
    env,stream=load(args.rust_root/"Bundles/shared/assetscenes.bundle")
    meshes=MeshSources(args.rust_root,env,profile['collider_sampling']['backfaces']) if args.colliders else None
    prefabs={}
    for asset in env.assets:
        names={p for p,a in targets.items() if asset.name=="BuildPlayer-"+a}
        if not names:continue
        def ptr(ref):
            if ref.get("m_FileID")!=0:raise ValueError("External prefab hierarchy")
            return PPtr(**ref,assetsfile=asset).deref()
        for obj in asset.objects.values():
            if int(obj.type)!=1:continue
            name=obj.read().m_Name.lower()
            if name not in names:continue
            if name in prefabs:raise ValueError("Duplicate static prefab root")
            attrs=[];components={};visited=set();base={};problems=[]
            def walk(go,parent=IDENTITY,active=True,depth=0):
                if depth>128 or go.path_id in visited or len(visited)>=65536:raise ValueError("Invalid prefab hierarchy")
                visited.add(go.path_id);f=go.read_typetree();is_root=go.path_id==obj.path_id
                active=active and (is_root or bool(f["m_IsActive"]))
                objects=[ptr(c["component"]) for c in f["m_Component"]]
                ts=[o.read_typetree() for o in objects if int(o.type)==4]
                if len(ts)!=1:raise ValueError("Missing prefab Transform")
                t=ts[0];transform=parent if is_root else multiply(parent,matrix(t))
                if is_root:base.update(rotation=vector(t["m_LocalRotation"],"xyzw"),scale=vector(t["m_LocalScale"]))
                for o in objects:
                    if int(o.type)!=114 or not active:continue
                    ident=identity(o);kind=ident[2]
                    if ident[:2]!=("Assembly-CSharp",""):continue
                    components[kind]=components.get(kind,0)+1
                    if kind.startswith(("Decor","Terrain")) or kind in ("EnvironmentVolumeCheck","PrefabParameters","AddToHeightMap","SpawnableBoundsBlocker"):
                        fields=attribute_fields(o.read_typetree());values={}
                        for key,value in fields.items():
                            if key.startswith("m_"):continue
                            try:values[key]=numeric(value)
                            except ValueError:problems.append(kind+"."+key+": unresolved field")
                        record=dict(kind=kind,root=is_root,position=point(transform,[0.,0.,0.]),fields=values)
                        if policy is not None:record['local_to_root']=[list(row) for row in transform]
                        if meshes is not None and kind=='AddToHeightMap':
                            try:record['collider']=meshes.collider(asset,objects)
                            except (ValueError,KeyError,TypeError,AttributeError) as error:problems.append('AddToHeightMap geometry: '+str(error))
                        attrs.append(record)
                for child in t["m_Children"]:
                    walk(ptr(ptr(child).read_typetree()["m_GameObject"]),transform,active,depth+1)
            walk(obj)
            if len(attrs)>4096:raise ValueError("Excessive prefab placement attributes")
            prefabs[name]=dict(**base,attributes=attrs,components=components,issues=problems)
            print(name,len(attrs),flush=True)
    for name in targets.keys()-prefabs.keys():issues.append("Missing prefab root: "+name)
    if sources!=snapshot(args.rust_root)+[fingerprint(args.rust_root,"Bundles/maps/maps.bundle",512*1024**2)] or before!=script_hash() or (args.topology_profile is not None and args.topology_profile.read_bytes()!=profile_raw):raise ValueError("Source changed during decoration extraction")
    if code_sources and verify_code(args.rust_root,profile)!=code_sources:raise ValueError("Decoration gameplay sources changed")
    if meshes is not None:meshes.verify()
    doc=dict(format="papyrus-decor-source",schema=5 if meshes is not None and code_sources else 4 if meshes is not None else 3 if policy is not None else 2,adapter="papyrus-static-decor-collider-5" if meshes is not None and code_sources else "papyrus-static-decor-collider-4" if meshes is not None else "papyrus-static-decor-3" if policy is not None else "papyrus-static-decor-2",scene=args.scene,sources=sources,extractor_sha256=before,stages=stages,prefabs=prefabs,issues=issues)
    if stage_layout=='mode-description':
        doc.update(schema=6,adapter='papyrus-static-decor-collider-6',stage_layout=stage_layout)
    if code_sources:
        if meshes is None:raise ValueError('Gameplay-bound decoration requires complete collider extraction')
        doc['code_sources']=code_sources
    if meshes is not None:doc['extra_sources']=list(meshes.sources.values())
    if policy is not None:doc['terrain_policy']=policy
    encoded=(json.dumps(doc,separators=(',',':'),allow_nan=False) if meshes is not None else json.dumps(doc,indent=2,allow_nan=False)).encode()
    if len(encoded)>LIMIT:raise ValueError("Static decoration metadata exceeds 16 MiB")
    args.cache_root.mkdir(parents=True,exist_ok=True);destination=args.cache_root/hashlib.sha256(encoded).hexdigest()
    if destination.exists():raise ValueError("Decoration snapshot already exists; refusing overwrite")
    with tempfile.TemporaryDirectory(prefix=".decor-",dir=args.cache_root) as temporary:
        stage=Path(temporary)
        with (stage/"decor.json").open("xb") as f:f.write(encoded);f.flush();os.fsync(f.fileno())
        publish_directory(stage, destination)
    print(destination/"decor.json",flush=True)
    if meshes is not None:meshes.close()
if __name__=="__main__":main()
