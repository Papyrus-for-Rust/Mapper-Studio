"""Reuse decoded RGBA only for identical converter inputs, never bundle names."""
import hashlib
import importlib.metadata
import json
from functools import lru_cache
from pathlib import Path
import zlib

@lru_cache(maxsize=1)
def decoder_identity():
    import UnityPy
    import PIL
    from UnityPy.export import Texture2DConverter
    return [UnityPy.__version__, PIL.__version__, importlib.metadata.version('texture2ddecoder'),
            hashlib.sha256(Path(Texture2DConverter.__file__).read_bytes()).hexdigest()]

def payload(texture):
    reader = texture.object_reader
    stream = getattr(texture, 'm_StreamData', None)
    if (stream and stream.size > 128*1024**2) or len(texture.image_data or b'') > 128*1024**2:
        raise ValueError('Texture payload exceeds reuse budget')
    data = texture.get_image_data()
    if not data or len(data) > 128*1024**2:
        raise ValueError('Invalid texture payload size')
    version = tuple(getattr(reader, 'version', (0,0,0,0)))
    platform = int(getattr(reader, 'platform', 0))
    blob = getattr(texture, 'm_PlatformBlob', None)
    arguments = [1,decoder_identity(),texture.m_Width,texture.m_Height,int(texture.m_TextureFormat),version,platform,list(blob) if blob is not None else None,True]
    digest = hashlib.sha256(json.dumps(arguments, separators=(',',':')).encode())
    digest.update(data)
    return digest.hexdigest(), data, version, platform, blob

class Pool:
    def __init__(self, roots):
        self.entries = {}
        self.hits = self.misses = 0
        indexed = 0
        for root in roots:
            root = Path(root)
            if not root.is_dir(): continue
            for directory in sorted(root.iterdir())[:256]:
                document = directory/'bindings.json'
                try:
                    with document.open('rb') as stream: raw = stream.read(4*1024**2+1)
                    if len(raw)>4*1024**2: continue
                    record=json.loads(raw)
                    if (record.get('format')!='papyrus-prefab-textures' or record.get('schema')!=1
                        or record.get('adapter')!='papyrus-guid-rgba-zlib-2'
                        or record.get('pixel_format')!='RGBA8-top-left'): continue
                    textures=record.get('textures',{})
                    if not isinstance(textures,dict) or len(textures)>4096: continue
                    for guid,item in textures.items():
                        signature=item.get('payload_sha256')
                        if (not isinstance(signature,str) or len(signature)!=64
                            or any(c not in '0123456789abcdef' for c in signature)
                            or not isinstance(guid,str) or len(guid)!=32
                            or any(c not in '0123456789abcdef' for c in guid)
                            or item.get('rgba_path')!=f'textures/{guid}.rgba.zlib'): continue
                        if indexed>=32768: break
                        self.entries.setdefault(signature,[]).append((directory,item))
                        indexed += 1
                except (OSError,ValueError,TypeError,AttributeError): continue

    def get(self, signature, width, height):
        for root,item in self.entries.get(signature,[]):
            try:
                count=width*height*4
                storage=item.get('storage',{})
                if (item.get('width')!=width or item.get('height')!=height or item.get('bytes')!=count
                    or storage.get('format')!='zlib' or not 0<storage.get('bytes',0)<=count+65536): continue
                path=root/item['rgba_path']
                if not path.resolve().is_relative_to(root.resolve()): continue
                with path.open('rb') as stream: encoded=stream.read(storage['bytes']+1)
                if len(encoded)!=storage['bytes'] or hashlib.sha256(encoded).hexdigest()!=storage['sha256']:continue
                decoder=zlib.decompressobj();pixels=decoder.decompress(encoded,count+1)
                if (len(pixels)!=count or not decoder.eof or decoder.unused_data or decoder.unconsumed_tail
                    or hashlib.sha256(pixels).hexdigest()!=item['sha256']):continue
                self.hits+=1
                return encoded,item['sha256']
            except (OSError,ValueError,TypeError,KeyError,zlib.error):continue
        self.misses+=1
        return None
