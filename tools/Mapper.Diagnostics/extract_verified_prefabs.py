"""Publish a source-bound numeric prefab catalog; never reuse unversioned caches.

Run with the same Python/UnityPy environment as extract_prefab_modifiers.py.
No game models/textures are exported. Sources are hashed before and after extraction.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from mapper_paths import publish_directory
import runpy
import sys
import tempfile
from functools import lru_cache
from mapper_progress import report

CATALOG_FORMAT = "papyrus-prefab-catalog"
ADAPTER = "papyrus-unity-autospawn-fields-building-4"


def supported_catalog(catalog, *, water=False):
    layouts = {(ADAPTER, 5), ("papyrus-unity-autospawn-fields-water-3", 5), ("papyrus-unity-guid-fields-water-2", 5)}
    if not water:
        layouts.add(("papyrus-unity-guid-fields-1", 4))
    return (catalog.get('format') == CATALOG_FORMAT and catalog.get('schema') == 1
            and (catalog.get('adapter'), catalog.get('metadata', {}).get('format')) in layouts)


def _file_identity(stream):
    """Match the complete Windows file ID of the parent's retained handle."""
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes
    import msvcrt

    class FileIdInfo(ctypes.Structure):
        _fields_ = [("volume", ctypes.c_ulonglong), ("identifier", ctypes.c_ubyte * 16)]

    query = ctypes.WinDLL("kernel32", use_last_error=True).GetFileInformationByHandleEx
    query.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    query.restype = wintypes.BOOL
    info = FileIdInfo()
    if not query(msvcrt.get_osfhandle(stream.fileno()), 18, ctypes.byref(info), ctypes.sizeof(info)):
        return None
    return info.volume, bytes(info.identifier).hex()


@lru_cache(maxsize=1)
def _live_receipts(path, parent):
    if os.name != "nt" or not path:
        return []
    try:
        with open(path, "rb") as stream:
            raw = stream.read(64 * 1024 + 1)
        if len(raw) > 64 * 1024:
            return []
        document = json.loads(raw)
        sources = document["sources"]
        if (document["format"] != "mapper-live-source-leases-1" or document["parent"] != parent
                or not isinstance(sources, list) or not 1 <= len(sources) <= 64):
            return []
        for source in sources:
            if (type(source["volume"]) is not int or not 0 <= source["volume"] < 2**64
                    or type(source["bytes"]) is not int or not 0 < source["bytes"] <= 16 * 1024**3
                    or not isinstance(source["file_id"], str) or len(source["file_id"]) != 32
                    or not isinstance(source["sha256"], str) or len(source["sha256"]) != 64
                    or any(c not in "0123456789abcdef" for c in source["file_id"] + source["sha256"])):
                return []
        return sources
    except (OSError, ValueError, KeyError, TypeError):
        return []


def _live_fingerprint(stream, relative, limit):
    sources = _live_receipts(os.environ.get("MAPPER_VERIFIED_SOURCES"), os.getppid())
    if not sources:
        return None
    identity = _file_identity(stream)
    size = os.fstat(stream.fileno()).st_size
    matches = [source for source in sources if identity == (source["volume"], source["file_id"])
               and source["bytes"] == size and size <= limit]
    if not matches or any(source != matches[0] for source in matches):
        return None
    return {"relative_path": relative, "sha256": matches[0]["sha256"], "bytes": size}


def fingerprint(root: Path, relative: str, limit: int) -> dict:
    path = root / relative
    before = path.stat()
    if not path.is_file() or not 0 < before.st_size <= limit:
        raise ValueError(f"Source exceeds its inspection budget: {relative}")
    report("Verifying Rust files", relative, 0, before.st_size, "bytes", force=True)
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as stream:
        receipt = _live_fingerprint(stream, relative, limit)
        if receipt is not None:
            report("Verifying Rust files", relative, receipt["bytes"], receipt["bytes"], "bytes", force=True)
            return receipt
        for chunk in iter(lambda: stream.read(65536), b""):
            count += len(chunk)
            if count > limit:
                raise ValueError(f"Source grew beyond its budget: {relative}")
            digest.update(chunk)
            report("Verifying Rust files", relative, count, before.st_size, "bytes")
    report("Verifying Rust files", relative, count, before.st_size, "bytes", force=True)
    after = path.stat()
    if count != before.st_size or (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise ValueError(f"Source changed while hashing: {relative}")
    return {"relative_path": relative, "sha256": digest.hexdigest(), "bytes": count}


def snapshot(root: Path, *, building=False) -> list[dict]:
    native = [name for name in ("RustClient_Data", "RustDedicated_Data")
              if (root / name / "Plugins/x86_64/RustNative.dll").is_file()]
    if len(native) != 1:
        raise ValueError("Select a root with exactly one supported client/server native library")
    sources = [fingerprint(root, native[0] + "/Plugins/x86_64/RustNative.dll", 64 * 1024**2),
            fingerprint(root, "Bundles/AssetSceneManifest.json", 32 * 1024**2),
            fingerprint(root, "Bundles/shared/assetscenes.bundle", 2 * 1024**3)]
    if building and (root / native[0] / 'globalgamemanagers').is_file():
        sources.append(fingerprint(root, native[0] + '/globalgamemanagers', 32 * 1024**2))
        for name in ('content','monuments'):
            relative=f'Bundles/shared/{name}.bundle'
            if (root/relative).is_file():sources.append(fingerprint(root,relative,8*1024**3))
    return sources


def extractor_fingerprint() -> str:
    digest = hashlib.sha256()
    for name in ("extract_prefab_modifiers.py", "prefab_geometry.py", "no_build_geometry.py", "decor_collider_meshes.py", "lazy_unity_bundle.py"):
        digest.update(name.encode("ascii") + b"\0")
        digest.update(Path(__file__).with_name(name).read_bytes())
    return digest.hexdigest()


from mapper_paths import data_root


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-root", type=Path, required=True)
    parser.add_argument("--cache-root", type=Path, default=data_root()/"papyrus-prefab-cache")
    parser.add_argument("--building-restrictions", action="store_true")
    args = parser.parse_args()
    source = snapshot(args.rust_root, building=args.building_restrictions)
    adapter = ADAPTER if len(source) >= 4 else 'papyrus-unity-autospawn-fields-water-3'
    extractor_hash = extractor_fingerprint()
    source_id = hashlib.sha256(json.dumps({"sources":source,"extractor":extractor_hash,"adapter":adapter}, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    args.cache_root.mkdir(parents=True, exist_ok=True)
    destination = args.cache_root / source_id
    if destination.exists():
        raise ValueError(f"Catalog already exists; verify it rather than overwriting: {destination}")
    extractor = Path(__file__).with_name("extract_prefab_modifiers.py")
    with tempfile.TemporaryDirectory(prefix=".extract-", dir=args.cache_root) as staging:
        staging = Path(staging)
        raw = staging / "modifiers.json"
        previous_argv = sys.argv
        try:
            sys.argv = [str(extractor), "--rust-root", str(args.rust_root), "--output", str(raw)]
            if args.building_restrictions:sys.argv.append('--building-restrictions')
            runpy.run_path(str(extractor), run_name="__main__")
        finally:
            sys.argv = previous_argv
        if source != snapshot(args.rust_root, building=args.building_restrictions) or extractor_hash != extractor_fingerprint():
            raise ValueError("Sources changed during extraction; catalog discarded")
        if raw.stat().st_size > 16 * 1024**2:
            raise ValueError("Catalog metadata exceeds 16 MiB")
        metadata = json.loads(raw.read_text(encoding="utf-8"))
        catalog = {"format": CATALOG_FORMAT, "schema": 1, "adapter": adapter,
                   "extractor_sha256": extractor_hash, "sources": source, "metadata": metadata}
        encoded = (json.dumps(catalog, separators=(',', ':')) if adapter == ADAPTER else json.dumps(catalog, indent=2)).encode()
        if len(encoded) > 16 * 1024**2:
            raise ValueError("Catalog exceeds 16 MiB")
        raw.unlink()  # Only this extractor-owned temporary file.
        with (staging / "catalog.json").open("xb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        # Rename the complete directory; no partially extracted catalog can be accepted.
        publish_directory(staging, destination)
    print(destination / "catalog.json")


if __name__ == "__main__":
    main()
