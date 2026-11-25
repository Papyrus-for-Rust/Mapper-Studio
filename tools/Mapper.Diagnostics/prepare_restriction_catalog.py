"""Extract the restriction catalog with bounded scene and transform optimizations."""
import gc
import sys
from unittest.mock import patch
from weakref import ref

from extract_verified_prefabs import main as write_catalog
from metadata_local_transform import matrix
from metadata_transforms import multiply
import no_build_geometry
import prefab_geometry


MAX_SHARED_TRANSFORMS = 8192


class SharedTransforms:
    """Keep one prefab's transforms for its restriction and water passes."""

    def __init__(self):
        self.restrictions = no_build_geometry.inspect
        self.water = prefab_geometry.inspect
        self.key = None
        self.locals = {}
        self.worlds = {}

    def clear(self):
        self.key = None
        self.locals.clear()
        self.worlds.clear()

    def local(self, fields):
        key = id(fields)
        cached = self.locals.get(key)
        if cached is not None:
            return cached[1]
        result = matrix(fields)
        if len(self.locals) < MAX_SHARED_TRANSFORMS:
            # Keep the fields alive so their identity cannot be reused.
            self.locals[key] = (fields, result)
        return result

    def world(self, parent, local):
        key = (id(parent), id(local))
        cached = self.worlds.get(key)
        if cached is not None:
            return cached[2]
        result = multiply(parent, local)
        if len(self.worlds) < MAX_SHARED_TRANSFORMS:
            self.worlds[key] = (parent, local, result)
        return result

    def inspect_restrictions(self, root, read, *args, **kwargs):
        self.clear()
        self.key = (root, ref(read))
        with (patch.object(no_build_geometry, "matrix", self.local),
              patch.object(no_build_geometry, "multiply", self.world)):
            return self.restrictions(root, read, *args, **kwargs)

    def inspect_water(self, root, read, *args, **kwargs):
        try:
            if self.key is not None and self.key[0] == root and self.key[1]() is read:
                with (patch.object(prefab_geometry, "matrix", self.local),
                      patch.object(prefab_geometry, "multiply", self.world)):
                    return self.water(root, read, *args, **kwargs)
            return self.water(root, read, *args, **kwargs)
        finally:
            self.clear()


def collect_by_scene(write):
    """Collect released Unity scenes without rescanning the retained mesh cache."""
    original = gc.collect
    enabled = gc.isenabled()
    scenes = 0

    def collect(generation=None):
        nonlocal scenes
        if generation is None:
            scenes += 1
            # Scene objects stay young until the producer releases the scene.
            # Collect older cycles periodically, including retained dependencies.
            generation = 2 if scenes % 8 == 0 else 0
        return original(generation)

    gc.disable()
    gc.collect = collect
    try:
        return write()
    finally:
        gc.collect = original
        if enabled:
            gc.enable()
        else:
            gc.disable()


def capture(write):
    transforms = SharedTransforms()
    with (patch.object(prefab_geometry, "multiply", multiply),
          patch.object(no_build_geometry, "multiply", multiply),
          patch.object(prefab_geometry, "matrix", matrix),
          patch.object(no_build_geometry, "matrix", matrix),
          patch.object(no_build_geometry, "inspect", transforms.inspect_restrictions),
          patch.object(prefab_geometry, "inspect", transforms.inspect_water)):
        return collect_by_scene(write)


def main():
    if (sys.implementation.name == "cpython"
            and sys.version_info[:2] == (3, 12)
            and "--building-restrictions" in sys.argv[1:]):
        capture(write_catalog)
    else:
        write_catalog()


if __name__ == "__main__":
    main()
