"""Resolve a verified prefab catalog's GUIDs through the installed GameManifest.

Publishes an exact-path, hashed RGBA cache under LocalAppData. No fuzzy matching.
This is a developer extraction tool, not the interactive editor rendering path.
"""
from mapper_progress import report
import argparse
from concurrent.futures import Future, ThreadPoolExecutor
from contextlib import nullcontext
import gc
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import shutil
import tempfile
import zlib

from extract_verified_prefabs import _live_fingerprint, fingerprint, snapshot, supported_catalog
from lazy_unity_bundle import load
from texture_payload_cache import Pool, payload


def read_catalog(path):
    with path.open("rb") as stream:
        raw = stream.read(16 * 1024**2 + 1)
    if len(raw) > 16 * 1024**2: raise ValueError("Catalog exceeds 16 MiB")
    return raw


def normalized_asset_path(name):
    if (not isinstance(name, str) or len(name) > 1024
            or not name.lower().startswith("assets/") or "\\" in name or "\0" in name
            or any(part in ("", ".", "..") for part in name.split("/"))):
        raise ValueError("Invalid GameManifest asset identifier")
    return name.lower()


def guid_paths(fields, required):
    result = {}
    for group in ("prefabProperties", "guidPaths"):
        records = fields.get(group)
        if not isinstance(records, list) or len(records) > 262144:
            raise ValueError("Unsupported GameManifest GUID table")
        for item in records:
            guid = item.get("guid")
            if guid not in required:
                continue
            name = normalized_asset_path(item.get("name"))
            if guid in result and result[guid] != name:
                raise ValueError(f"Ambiguous GUID {guid}")
            result[guid] = name
    return result


def container_index(environment):
    result = {}
    for asset in environment.assets:
        for path, obj in asset.container.items():
            path = path.replace("\\", "/").lower()
            result.setdefault(path, []).append(obj)
    return result


def unique_asset(index, path):
    # Unity can publish case aliases of the same object. Only ambiguity for an
    # actually requested asset matters; never select between distinct objects.
    readers = [obj.deref() if hasattr(obj, "deref") else obj for obj in index.get(path, [])]
    objects = {(id(obj.assets_file), obj.path_id): obj for obj in readers}
    if len(objects) > 1:
        raise ValueError(f"Ambiguous requested bundle asset: {path}")
    return next(iter(objects.values()), None)


def encode_texture(source_data, width, height, texture_format, version, platform, blob):
    from UnityPy.export import Texture2DConverter
    pixels = Texture2DConverter.parse_image_data(
        source_data, width, height, texture_format, version, platform, blob, True,
    ).convert("RGBA")
    if pixels.size != (width, height):
        raise ValueError("Decoded texture dimensions differ from source")
    data = pixels.tobytes()
    return zlib.compress(data, level=1), hashlib.sha256(data).hexdigest()


def decode(index, paths, required, output, bundle, textures, total, pool=None, emitted=None,
           worker=None):
    if emitted is None:
        emitted = {}
    stored_total = sum(item["storage"]["bytes"] for item in textures.values())
    pending = None

    def write_texture(guid, record, result):
        nonlocal stored_total
        signature = record["payload_sha256"]
        previous = emitted.get(signature)
        relative = record["rgba_path"]
        if previous is None:
            encoded, pixel_hash = result.result() if isinstance(result, Future) else result
            storage = {"format":"zlib", "bytes":len(encoded),
                       "sha256":hashlib.sha256(encoded).hexdigest()}
        else:
            storage, pixel_hash = previous["storage"], previous["sha256"]
        stored_total += storage["bytes"]
        if stored_total > 1024**3:
            raise ValueError("Stored prefab textures exceed 1 GiB cache budget")
        if previous is None:
            (output / relative).write_bytes(encoded)
        else:
            # Keep independent payload files for repair and publication.
            shutil.copyfile(output / previous["rgba_path"], output / relative)
        record["sha256"] = pixel_hash
        record["storage"] = storage
        textures[guid] = record
        if previous is None:
            emitted[signature] = record

    for guid in sorted(required - textures.keys()):
        path = paths.get(guid)
        if path not in index:
            continue
        obj = unique_asset(index, path)
        if int(obj.type) != 28:
            raise ValueError(f"Terrain GUID is not a Texture2D: {path}")
        texture = obj.read()
        width, height = texture.m_Width, texture.m_Height
        if not 1 <= width <= 4096 or not 1 <= height <= 4096:
            raise ValueError(f"Texture outside decoding budget: {path}")
        count = width * height * 4
        if total + count > 4*1024**3:
            raise ValueError("Decoded prefab textures exceed 4 GiB cache budget")
        signature, source_data, version, platform, blob = payload(texture)
        already_queued = pending is not None and pending[1]["payload_sha256"] == signature
        result = None
        if signature not in emitted and not already_queued:
            result = pool.get(signature, width, height) if pool else None
            if result is None:
                inputs = (source_data, width, height, texture.m_TextureFormat, version, platform, blob)
                result = worker.submit(encode_texture, *inputs) if worker else encode_texture(*inputs)
        # Read Unity objects only on this thread. Keep a single queued payload
        # after each iteration and publish in the original GUID order.
        if pending is not None:
            write_texture(*pending)
        pending = (guid, {
            "asset_path": path, "bundle_relative_path": bundle,
            "rgba_path": f"textures/{guid}.rgba.zlib", "width": width, "height": height,
            "bytes": count, "sha256": None, "payload_sha256": signature,
        }, result)
        if worker is None:
            write_texture(*pending)
            pending = None
        total += count
    if pending is not None:
        write_texture(*pending)
    return total


def discover_extra_bundles(root, paths, missing):
    locations = {}
    candidates = ["Bundles/maps/maps.bundle"] + [f"Bundles/shared/textures.{i}.bundle" for i in range(5)]
    for relative in candidates:
        if not (root / relative).is_file(): continue
        environment, stream = load(root / relative)
        try:
            index = container_index(environment)
            for guid in missing:
                path = paths.get(guid)
                if path and unique_asset(index, path) is not None:
                    if guid in locations:
                        raise ValueError(f"Terrain asset exists in multiple candidate bundles: {path}")
                    locations[guid] = relative
        finally:
            stream.close()
        del index, environment, stream
        gc.collect()
    if set(locations) != set(missing):
        raise ValueError(f"Unresolved GUIDs: {sorted(set(missing)-locations.keys())}")
    return sorted(set(locations.values()))


def discover_texture_bundles(root, required):
    bundles = ["Bundles/shared/content.bundle", "Bundles/shared/monuments.bundle"]
    paths, found = {}, set()
    for relative in bundles:
        report("Locating monument textures", relative, len(found), len(required), force=True)
        environment, stream = load(root / relative)
        try:
            index = container_index(environment)
            if relative == bundles[0]:
                manifest = unique_asset(index, "assets/manifest.asset")
                if manifest is None:
                    raise ValueError("GameManifest is absent from content bundle")
                paths = guid_paths(manifest.read_typetree(), required)
                del manifest
            for guid in required - found:
                obj = unique_asset(index, paths.get(guid))
                if obj is not None:
                    if int(obj.type) != 28:
                        raise ValueError(f"Terrain GUID is not a Texture2D: {paths[guid]}")
                    found.add(guid)
                del obj
        finally:
            stream.close()
        del index, environment, stream
        gc.collect()
    if missing := required - found:
        bundles.extend(discover_extra_bundles(root, paths, missing))
    return bundles


from mapper_paths import data_root



def read_source_plan(path, catalog_hash, required=None):
    maximum = 8 * 1024**2
    with path.open('rb') as stream:
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise ValueError('Texture source plan exceeds its budget')
    plan = json.loads(raw)
    supported = {'Bundles/shared/content.bundle', 'Bundles/shared/monuments.bundle',
                 'Bundles/maps/maps.bundle',
                 *(f'Bundles/shared/textures.{i}.bundle' for i in range(5))}
    fields = {'format', 'schema', 'catalog_sha256', 'bundles'}
    if (not isinstance(plan, dict) or not fields <= plan.keys()
            or plan['format'] != 'mapper-texture-sources' or type(plan['schema']) is not int
            or plan['catalog_sha256'] != catalog_hash or not isinstance(plan['bundles'], list)
            or not 2 <= len(plan['bundles']) <= 8
            or plan['bundles'][:2] != ['Bundles/shared/content.bundle', 'Bundles/shared/monuments.bundle']
            or any(not isinstance(name, str) or name not in supported for name in plan['bundles'])
            or len(set(plan['bundles'])) != len(plan['bundles'])):
        raise ValueError('Invalid texture source plan')
    if plan['schema'] == 1 and set(plan) == fields and len(raw) <= 4096:
        return plan
    if plan['schema'] == 2 and set(plan) == fields | {'paths', 'content_has_textures'}:
        paths = plan['paths']
        if (required and isinstance(paths, dict) and paths.keys() == required
                and type(plan['content_has_textures']) is bool
                and all(normalized_asset_path(name) == name and len(name.encode("utf-8")) <= 1024
                        for name in paths.values())):
            return plan
    raise ValueError('Invalid texture source plan')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-root", type=Path, required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--cache-root", type=Path, default=data_root()/"papyrus-texture-cache")
    parser.add_argument("--reuse-cache",type=Path,action="append",default=[])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--discover-sources", type=Path)
    mode.add_argument("--source-plan", type=Path, help="Reuse the parent application's verified bundle discovery.")
    args = parser.parse_args()
    raw = read_catalog(args.catalog)
    catalog = json.loads(raw)
    if not supported_catalog(catalog):
        raise ValueError("Unsupported prefab catalog")
    catalog_hash = hashlib.sha256(raw).hexdigest()
    sources = snapshot(args.rust_root)
    if sources != catalog["sources"]:
        raise ValueError("Catalog does not match installed sources")
    bundles = ["Bundles/shared/content.bundle", "Bundles/shared/monuments.bundle"]
    primary_sources = [fingerprint(args.rust_root, p, 8 * 1024**3) for p in bundles]
    sources += primary_sources
    required = {guid for item in catalog["metadata"]["modifiers"].values()
                for guid in item.get("textureGuids", {}).values() if guid}
    if not required or len(required) > 4096 or any(not isinstance(g, str) or len(g) != 32 or any(c not in "0123456789abcdef" for c in g) for g in required):
        raise ValueError("Invalid required GUID collection")
    if args.discover_sources:
        document = dict(format="mapper-texture-sources", schema=1,
                        catalog_sha256=catalog_hash,
                        bundles=discover_texture_bundles(args.rust_root, required))
        with args.discover_sources.open("x", encoding="utf-8") as output:
            json.dump(document, output)
        return
    source_plan = read_source_plan(args.source_plan, catalog_hash, required) if args.source_plan else None
    native_paths = source_plan.get("paths") if source_plan else None
    if native_paths is not None:
        # Reuse a private native plan only while the parent owns the exact
        # content bundle lease used for GameManifest and container discovery.
        with (args.rust_root / bundles[0]).open("rb") as stream:
            if _live_fingerprint(stream, bundles[0], 8 * 1024**3) != primary_sources[0]:
                native_paths = None
    pool=Pool(args.reuse_cache)
    args.cache_root.mkdir(parents=True, exist_ok=True)
    encoder = (ThreadPoolExecutor(max_workers=1, thread_name_prefix="mapper-textures")
               if os.environ.get("MAPPER_TEXTURE_WORKERS") == "2" else nullcontext())
    with tempfile.TemporaryDirectory(prefix=".resolve-", dir=args.cache_root) as staging, encoder as worker:
        staging = Path(staging)
        (staging / "textures").mkdir()
        textures, total, paths, emitted = {}, 0, native_paths or {}, {}
        for relative in bundles:
            if relative == bundles[0] and native_paths and not source_plan["content_has_textures"]:
                continue
            report("Reading texture metadata", relative, len(textures), len(required), force=True)
            environment, stream = load(args.rust_root / relative)
            try:
                index = container_index(environment)
                if relative == bundles[0] and native_paths is None:
                    manifest = unique_asset(index, "assets/manifest.asset")
                    if manifest is None: raise ValueError("GameManifest is absent from content bundle")
                    fields = manifest.read_typetree()
                    paths = guid_paths(fields, required)
                    del fields, manifest
                    print(f"Exact GUID mappings: {len(paths)}/{len(required)}", flush=True)
                total = decode(index, paths, required, staging, relative, textures, total, pool, emitted, worker)
                report("Decoding monument textures", relative, len(textures), len(required), force=True)
            finally:
                stream.close()
            del index, environment, stream
            gc.collect()
        missing = required - textures.keys()
        if missing:
            extras = (source_plan["bundles"][2:] if source_plan is not None
                      else discover_extra_bundles(args.rust_root, paths, missing))
            for relative in extras:
                # Reopen after fingerprinting: discovery did not decode or publish pixels.
                sources.append(fingerprint(args.rust_root, relative, 8 * 1024**3))
                bundles.append(relative)
                print(f"Exact shared texture source: {relative}", flush=True)
                environment, stream = load(args.rust_root / relative)
                try:
                    index = container_index(environment)
                    total = decode(index, paths, required, staging, relative, textures, total, pool, emitted, worker)
                finally: stream.close()
                del index, environment, stream
                gc.collect()
        if required - textures.keys(): raise ValueError("Incomplete cache discarded")
        final_sources = snapshot(args.rust_root) + [fingerprint(args.rust_root, p, 8 * 1024**3) for p in bundles]
        if sources != final_sources or hashlib.sha256(read_catalog(args.catalog)).hexdigest() != catalog_hash:
            raise ValueError("Installed sources/catalog changed; cache discarded")
        identity = hashlib.sha256(json.dumps([catalog_hash, sources], sort_keys=True).encode()).hexdigest()
        destination = args.cache_root / identity
        if destination.exists(): raise ValueError(f"Cache already exists; verify rather than overwrite: {destination}")
        receipt = {"format":"papyrus-prefab-textures", "schema":1, "adapter":"papyrus-guid-rgba-zlib-2",
                   "catalog_sha256":catalog_hash, "sources":sources, "pixel_format":"RGBA8-top-left",
                   "textures":textures, "decoded_bytes":total}
        with (staging / "bindings.json").open("xb") as stream:
            stream.write(json.dumps(receipt, indent=2).encode())
            stream.flush()
            os.fsync(stream.fileno())
        publish_directory(staging, destination)
    repeated = len(textures) - len(emitted)
    report("Texture reuse", f"{pool.hits + repeated} reused, {pool.misses} decoded",len(textures),len(textures),force=True)
    print(destination / "bindings.json", flush=True)


if __name__ == "__main__": main()
