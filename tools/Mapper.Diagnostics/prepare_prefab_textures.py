"""Run texture-only encoding workers without changing shared metadata fingerprints."""
from collections import deque
from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
import shutil
from unittest.mock import patch

from mapper_progress import report
import resolve_prefab_textures as resolver
from resolve_prefab_textures import encode_texture, unique_asset
from texture_payload_cache import Pool, payload


def prepare_texture(donors, signature, inputs):
    if donors:
        # The donor index is read-only. Keep lookup counters local to the
        # worker and let the ordered writer record each hit or miss once.
        lookup = Pool(())
        for donor in donors:
            lookup.entries[signature] = (donor,)
            stored = lookup.get(signature, inputs[1], inputs[2])
            if stored is not None:
                encoded, pixel_hash = stored
                # Pool.get already checked this digest against these bytes.
                return encoded, pixel_hash, donor[1]["storage"]["sha256"], True
    encoded, pixel_hash = encode_texture(*inputs)
    return encoded, pixel_hash, hashlib.sha256(encoded).hexdigest(), False


def decode(index, paths, required, output, bundle, textures, total, pool=None, emitted=None,
           *, worker, pending_limit):
    if emitted is None:
        emitted = {}
    stored_total = sum(item["storage"]["bytes"] for item in textures.values())
    pending = deque()
    pending_bytes = 0
    pending_budget = 192 * 1024**2

    def write_texture(guid, record, result):
        nonlocal stored_total
        signature = record["payload_sha256"]
        previous = emitted.get(signature)
        relative = record["rgba_path"]
        if previous is None:
            encoded, pixel_hash, stored_hash, reused = result.result()
            if pool is not None:
                if reused:
                    pool.hits += 1
                else:
                    pool.misses += 1
            storage = {"format":"zlib", "bytes":len(encoded),
                       "sha256":stored_hash}
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
        report("Preparing terrain textures", bundle.rsplit("/", 1)[-1], len(textures), len(required))

    def write_next():
        nonlocal pending_bytes
        guid, record, result, size = pending.popleft()
        write_texture(guid, record, result)
        pending_bytes -= size

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
        donors = pool.entries.get(signature) if pool is not None else None
        # Budget raw inputs and expanded pixels, including results waiting
        # behind a slower texture. Large textures drain the queue first.
        pending_size = len(source_data) + count
        if donors:
            # Donor validation holds the stored stream and decoded pixels
            # together. Pool.get limits the stored stream to this size.
            pending_size += count + 65536
        while pending and pending_bytes + pending_size > pending_budget:
            write_next()
        already_queued = any(record["payload_sha256"] == signature for _, record, _, _ in pending)
        result = None
        if signature not in emitted and not already_queued:
            inputs = (source_data, width, height, texture.m_TextureFormat, version, platform, blob)
            result = worker.submit(prepare_texture, donors, signature, inputs)
        # Read Unity objects on this thread and always publish in GUID order.
        pending.append((guid, {
            "asset_path": path, "bundle_relative_path": bundle,
            "rgba_path": f"textures/{guid}.rgba.zlib", "width": width, "height": height,
            "bytes": count, "sha256": None, "payload_sha256": signature,
        }, result, pending_size))
        pending_bytes += pending_size
        if len(pending) > pending_limit:
            write_next()
        total += count
    while pending:
        write_next()
    return total


def main():
    setting = os.environ.get("MAPPER_TEXTURE_WORKERS")
    if setting not in ("3", "4", "5", "6"):
        return resolver.main()

    encoders = int(setting) - 1
    with ThreadPoolExecutor(max_workers=encoders, thread_name_prefix="mapper-textures") as worker:
        def parallel_decode(index, paths, required, output, bundle, textures, total,
                            pool=None, emitted=None, _serial_worker=None):
            return decode(index, paths, required, output, bundle, textures, total, pool,
                          emitted, worker=worker, pending_limit=2 * encoders)

        with patch.object(resolver, "decode", parallel_decode):
            return resolver.main()


if __name__ == "__main__":
    main()
