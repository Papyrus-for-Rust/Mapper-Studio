"""Share one selected-scene traversal between the two road snapshot writers."""
import argparse
from contextlib import ExitStack
from pathlib import Path
import sys
from types import SimpleNamespace
from unittest.mock import patch

import extract_infrastructure as infrastructure
import extract_road_layout_assets as layout
from lazy_unity_bundle import load
from metadata_local_transform import matrix
from metadata_object_reader import GameObjectName, NameScan
from metadata_transforms import multiply
import prefab_paths
from prefab_paths import component_readers, inspect_ordered
import road_layout_assets


class SceneObjects(NameScan):
    def __init__(self, objects):
        super().__init__(objects)
        self.roots = []
        self.replay = False

    def values(self):
        if self.replay:
            # Only selected roots enter the second writer, in discovery order.
            for object_id in self.roots:
                yield GameObjectName(self.objects[object_id])
        else:
            yield from super().values()


class SharedScenes:
    def __init__(self, resources):
        self.resources = resources
        self.selection = None
        self.env = None
        self.reports = {}
        self.infrastructure = None

    def load(self, path, *, nodes, follow_dependencies):
        # The snapshot writers select the same scene files. The outer scope
        # owns the stream, including when either writer raises an exception.
        selection = path, frozenset(nodes or ()), follow_dependencies
        if self.selection is None:
            env, stream = load(path, nodes=nodes, follow_dependencies=follow_dependencies)
            self.resources.callback(stream.close)
            self.env = SimpleNamespace(assets=[
                SimpleNamespace(name=asset.name, objects=SceneObjects(asset.objects))
                for asset in env.assets
            ])
            self.selection = selection
        elif self.selection != selection:
            raise ValueError("Road snapshot scene selections differ")
        else:
            for asset in self.env.assets:
                asset.objects.replay = True
        return self.env, self

    def close(self):
        pass

    @staticmethod
    def readers(objects):
        read, script_type = component_readers(objects)
        # Path IDs are local to their serialized asset.
        read.asset_identity = id(objects)
        read.scene_roots = objects.roots
        return read, script_type

    def capture_ordered(self, root, read, script_type):
        report = inspect_ordered(root, read, script_type)
        # Root MonumentInfo validation extends the ordered report only.
        self.infrastructure = {
            key: value for key, value in report.items() if key != "connectorOrder"
        }
        self.infrastructure["version"] = 1
        self.infrastructure["issues"] = list(report["issues"])
        self.infrastructure["connectors"] = sorted(
            report["connectors"], key=lambda connector: connector["objectId"]
        )
        return report

    def inspect_infrastructure(self, root, read, script_type):
        ordered = road_layout_assets.inspect_layout(root, read, script_type)
        self.reports[read.asset_identity, root] = ordered
        read.scene_roots.append(root)
        return self.infrastructure

    def inspect_layout(self, root, read, script_type):
        return self.reports.pop((read.asset_identity, root))


def run_writer(writer, args):
    with patch.object(sys, "argv", [writer.__file__, *map(str, args)]):
        writer.main()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-root", type=Path, required=True)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--cache-root", type=Path, required=True)
    args = parser.parse_args()
    common = ["--rust-root", args.rust_root, "--catalog", args.catalog]
    first = args.cache_root / "infrastructure"
    second = args.cache_root / "layout"
    with ExitStack() as stack:
        shared = SharedScenes(stack)
        for writer in (infrastructure, layout):
            stack.enter_context(patch.object(writer, "load", shared.load))
            stack.enter_context(patch.object(writer, "component_readers", shared.readers))
        stack.enter_context(patch.object(infrastructure, "inspect", shared.inspect_infrastructure))
        stack.enter_context(patch.object(layout, "inspect_layout", shared.inspect_layout))
        stack.enter_context(patch.object(road_layout_assets, "inspect_ordered", shared.capture_ordered))
        if sys.implementation.name == "cpython" and sys.version_info[:2] == (3, 12):
            stack.enter_context(patch.object(prefab_paths, "multiply", multiply))
            stack.enter_context(patch.object(prefab_paths, "matrix", matrix))
        run_writer(infrastructure, [*common, "--profile", args.profile, "--cache-root", first])
        sources = list(first.glob("*/infrastructure.json"))
        if len(sources) != 1:
            raise ValueError("Expected one prepared infrastructure snapshot")
        run_writer(layout, [*common, "--infrastructure", sources[0], "--cache-root", second])
        if shared.reports:
            raise ValueError("Road snapshot prefab selections differ")


if __name__ == "__main__":
    main()
