"""Read the assigned spawn scene with the existing snapshot writer."""
import argparse
from pathlib import Path
from unittest.mock import patch

import extract_spawn_source as writer
from lazy_unity_bundle import load


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--rust-root", type=Path)
    parser.add_argument("--profile", type=Path)
    args, _ = parser.parse_known_args()
    if args.rust_root is None or args.profile is None:
        return writer.main()

    manifest = writer.read_json(args.rust_root / "Bundles/AssetSceneManifest.json")
    handler = writer.read_json(args.profile)["handler_prefab"]
    scenes = [
        scene["Name"] for scene in manifest["Scenes"]
        if any(path.lower() == handler for path in scene["IncludedAssets"])
    ]
    if len(scenes) != 1:
        # The writer owns the diagnostic for missing or ambiguous assignments.
        return writer.main()

    bundle = args.rust_root / "Bundles/shared/assetscenes.bundle"
    nodes = {"BuildPlayer-" + scenes[0]}

    def load_for_spawn(path):
        if path == bundle:
            return load(path, nodes=nodes, follow_dependencies=True)
        return load(path)

    # Keep scene transport separate so existing source receipts remain valid.
    with patch.object(writer, "load", load_for_spawn):
        writer.main()


if __name__ == "__main__":
    main()
