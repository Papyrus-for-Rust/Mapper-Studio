"""Prepare monument metadata with bounded name and transform reads."""
from contextlib import ExitStack
import sys
from types import SimpleNamespace
from unittest.mock import patch

import extract_monument_source as writer
from metadata_local_transform import matrix
from metadata_object_reader import NameScan
from metadata_transforms import multiply


class AssetNames:
    def __init__(self, asset):
        self.asset = asset
        self.objects = NameScan(asset.objects)

    def __getattr__(self, name):
        return getattr(self.asset, name)


def main():
    load = writer.load

    def load_names(path, **kwargs):
        environment, stream = load(path, **kwargs)
        selected = kwargs.get('nodes')
        if not selected:
            return environment, stream
        try:
            assets = [AssetNames(asset) if asset.name in selected else asset
                      for asset in environment.assets]
        except BaseException:
            stream.close()
            raise
        return SimpleNamespace(assets=assets), stream

    with ExitStack() as stack:
        stack.enter_context(patch.object(writer, 'load', load_names))
        if sys.implementation.name == "cpython" and sys.version_info[:2] == (3, 12):
            stack.enter_context(patch.object(writer, "matrix", matrix))
            stack.enter_context(patch.object(writer, "multiply", multiply))
        writer.main()


if __name__ == '__main__':
    main()
