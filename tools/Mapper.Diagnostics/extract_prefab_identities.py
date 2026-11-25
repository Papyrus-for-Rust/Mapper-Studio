"""Capture authoritative prefab IDs from the selected installation's GameManifest."""
import argparse,hashlib,json,tempfile
from pathlib import Path
from mapper_paths import publish_directory
from mapper_paths import data_root

def capture(fields):
    rows=fields.get('pooledStrings'); props=fields.get('prefabProperties')
    if not isinstance(rows,list) or not 1<=len(rows)<=262144 or not isinstance(props,list) or not 1<=len(props)<=131072:
        raise ValueError('Unsupported GameManifest identity tables')
    by_id={}; by_path={}
    for row in rows:
        if not isinstance(row,dict):raise ValueError('Invalid pooled-string record')
        name=row.get('str'); identity=row.get('hash')
        if not isinstance(name,str) or len(name)>4096 or '\0' in name or type(identity)!=int or not 0<=identity<=0xffffffff:
            raise ValueError('Invalid pooled string')
        # The server inserts only the first row for a given numeric ID.
        if identity in by_id: continue
        normalized=name.lower()
        if normalized in by_path and by_path[normalized]!=identity: raise ValueError('Ambiguous case-insensitive pooled string')
        by_id[identity]=name;by_path[normalized]=identity
    prefabs={}
    for row in props:
        if not isinstance(row,dict):raise ValueError('Invalid prefab property record')
        name=row.get('name'); identity=row.get('hash')
        if not isinstance(name,str) or not name.isascii() or not name.lower().startswith('assets/') or not name.lower().endswith('.prefab') or '\\' in name or any(p in ('','..','.') for p in name.split('/')):
            raise ValueError('Unsupported prefab property path')
        if type(identity)!=int or identity==0 or by_path.get(name.lower())!=identity: raise ValueError('Prefab property is not resolvable through the selected string pool')
        if name.lower() in prefabs and prefabs[name.lower()]!=identity: raise ValueError('Conflicting prefab identity')
        prefabs[name.lower()]=identity
    return dict(sorted(prefabs.items()))

def main():
    from lazy_unity_bundle import load
    from resolve_prefab_textures import container_index,unique_asset
    from extract_verified_prefabs import fingerprint
    scripts=[Path(__file__).with_name(n) for n in ('extract_prefab_identities.py','lazy_unity_bundle.py','resolve_prefab_textures.py','extract_verified_prefabs.py','mapper_paths.py')]
    digest=lambda:hashlib.sha256(b''.join(p.name.encode()+b'\0'+p.read_bytes() for p in scripts)).hexdigest()
    extractor=digest()
    a=argparse.ArgumentParser();a.add_argument('--rust-root',type=Path,required=True);a.add_argument('--cache-root',type=Path,default=data_root()/'papyrus-prefab-identity-cache');args=a.parse_args()
    relative='Bundles/shared/content.bundle'; receipt=fingerprint(args.rust_root,relative,8*1024**3)
    env,stream=load(args.rust_root/relative)
    try:
        obj=unique_asset(container_index(env),'assets/manifest.asset')
        if obj is None:raise ValueError('Missing exact GameManifest')
        prefabs=capture(obj.read_typetree())
    finally:stream.close()
    if receipt!=fingerprint(args.rust_root,relative,8*1024**3):raise ValueError('GameManifest changed during capture')
    if digest()!=extractor:raise ValueError('Identity extractor changed during capture')
    doc=dict(format='papyrus-prefab-identities',schema=1,adapter='papyrus-game-manifest-pool-1',source=receipt,extractor_sha256=extractor,prefabs=prefabs)
    raw=json.dumps(doc,separators=(',',':'),allow_nan=False).encode()
    if len(raw)>16*1024**2:raise ValueError('Identity snapshot exceeds 16 MiB')
    args.cache_root.mkdir(parents=True,exist_ok=True);dest=args.cache_root/hashlib.sha256(raw).hexdigest()
    if dest.exists():raise ValueError('Snapshot already exists; refusing overwrite')
    with tempfile.TemporaryDirectory(prefix='.identities-',dir=args.cache_root) as tmp:
        stage=Path(tmp);(stage/'identities.json').write_bytes(raw);publish_directory(stage, dest)
    print(dest/'identities.json')
if __name__=='__main__':main()
