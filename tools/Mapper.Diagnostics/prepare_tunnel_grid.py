"""Run the existing tunnel snapshot writer with scoped prefab traversal caches."""
from collections import OrderedDict
import argparse
import gc
import json
import mmap
from pathlib import Path
import sys
from types import SimpleNamespace
from unittest.mock import patch

import inspect_tunnel_grid as writer
from extract_verified_prefabs import _live_fingerprint
import lazy_unity_bundle
from metadata_local_transform import matrix
from metadata_object_reader import NameScan
from metadata_transforms import multiply
import prefab_paths
import tunnel_prefabs
from tunnel_identities import resolve as resolve_identities


SCENE_BATCH_SIZE = 8
ASSET_SCENES = "Bundles/shared/assetscenes.bundle"
ASSET_SCENES_LIMIT = 2 * 1024**3


class SceneDirectory(lazy_unity_bundle.LazyBundle):
    def read_files(self, blocks, nodes):
        self.names = [node.path for node in nodes]


class SceneMapping(mmap.mmap):
    def seek(self, offset, whence=0):
        # Python 3.12 mmap.seek returns None. UnityPy expects the new position.
        super().seek(offset, whence)
        return self.tell()


def open_scene(path, mode):
    stream = open(path, mode)
    try:
        if _live_fingerprint(stream, ASSET_SCENES, ASSET_SCENES_LIMIT) is None:
            return stream
        try:
            mapping = SceneMapping(stream.fileno(), 0, access=mmap.ACCESS_READ)
        except (OSError, ValueError):
            return stream
    except BaseException:
        stream.close()
        raise
    stream.close()
    return mapping


class ObjectFields:
    """Reuse recently decoded fields within one serialized asset namespace."""
    def __init__(self, read):
        self.read = read
        self.objects = None
        self.fields = OrderedDict()

    def clear(self):
        self.fields.clear()
        self.objects = None

    def __call__(self, objects, object_id):
        if objects is not self.objects:
            self.clear()
            self.objects = objects
        if object_id in self.fields:
            self.fields.move_to_end(object_id)
            return self.fields[object_id]
        result = self.read(objects, object_id)
        if result[1] and objects[object_id].byte_size <= 64 * 1024:
            self.fields[object_id] = result
            if len(self.fields) > 256:
                self.fields.popitem(last=False)
        return result


def capture_scenes(root, selected, fields):
    if sys.platform != "win32" or not selected:
        return tunnel_prefabs.capture(root, selected)

    path = root / ASSET_SCENES
    with path.open("rb") as stream:
        receipt = _live_fingerprint(stream, ASSET_SCENES, ASSET_SCENES_LIMIT)
        if receipt is not None:
            index = SceneDirectory(lazy_unity_bundle.EndianBinaryReader(stream), None, name=path.name)
    if receipt is None:
        return tunnel_prefabs.capture(root, selected)

    groups = {}
    for prefab, scene in selected.items():
        groups.setdefault("BuildPlayer-" + scene, {})[prefab] = scene
    # Preserve the bundle order the writer used before splitting the input.
    scenes = [groups[name] for name in index.names if name in groups]
    if len(scenes) != len(groups):
        raise ValueError("Missing tunnel prefab scene")
    prefabs = {}
    with patch.object(lazy_unity_bundle, "open", open_scene, create=True):
        for start in range(0, len(scenes), SCENE_BATCH_SIZE):
            if start:
                fields.clear()
                gc.collect()
            batch = {path: scene for group in scenes[start:start + SCENE_BATCH_SIZE]
                     for path, scene in group.items()}
            prefabs.update(tunnel_prefabs.capture(root, batch))
    return prefabs


def capture(root, selected):
    fields = ObjectFields(prefab_paths.read_path_object)
    inspect = tunnel_prefabs.inspect
    enabled = gc.isenabled()
    load = lazy_unity_bundle.load

    def load_names(path, **kwargs):
        environment, stream = load(path, **kwargs)
        try:
            assets = [SimpleNamespace(name=asset.name, objects=NameScan(asset.objects))
                      for asset in environment.assets]
        except BaseException:
            stream.close()
            raise
        return SimpleNamespace(assets=assets), stream

    def inspect_prefab(*args):
        try:
            return inspect(*args)
        finally:
            # The scene graph stays live. Collect only the temporary prefab data.
            gc.collect(0)

    try:
        with (patch.object(prefab_paths, "read_path_object", fields),
              patch.object(lazy_unity_bundle, "load", load_names),
              patch.object(tunnel_prefabs, "multiply", multiply),
              patch.object(tunnel_prefabs, "matrix", matrix),
              patch.object(tunnel_prefabs, "inspect", inspect_prefab)):
            gc.disable()
            return capture_scenes(root, selected, fields)
    finally:
        fields.clear()
        if enabled:
            gc.enable()
        # Release the cyclic scene graph before opening the identity bundle.
        gc.collect()


def main():
    parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
    parser.add_argument("--identity-fields", type=Path)
    args, remaining = parser.parse_known_args()
    original = writer.capture_identities

    def identities(root, prefabs):
        if args.identity_fields is not None:
            try:
                fields = json.loads(writer.bounded(args.identity_fields, 16 * 1024**2))
                return resolve_identities(fields, prefabs)
            except (OSError, ValueError, TypeError, KeyError):
                # Preserve the compatibility path for unfamiliar manifest layouts.
                pass
        return original(root, prefabs)

    optimized = (sys.implementation.name == "cpython"
                 and sys.version_info[:2] == (3, 12)
                 and "--capture-prefabs" in remaining)
    with (patch.object(writer, "capture", capture if optimized else writer.capture),
          patch.object(writer, "capture_identities", identities),
          patch.object(sys, "argv", [sys.argv[0], *remaining])):
        writer.main()


if __name__ == "__main__":
    main()
