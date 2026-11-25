"""Load only the prefab scenes used by static decoration's snapshot writer."""
import argparse
import json
from pathlib import Path
from unittest.mock import patch

import extract_decor_source as writer
from lazy_unity_bundle import load


def capture(root):
    asset_bundle = root / 'Bundles/shared/assetscenes.bundle'
    folders = set()
    original_stage = writer.stage_data

    def stage_data(*args, **kwargs):
        stage = original_stage(*args, **kwargs)
        folders.add('assets/bundled/prefabs/autospawn/' + stage['folder'] + '/')
        return stage

    def load_for_decoration(path):
        if path != asset_bundle or not folders:
            return load(path)
        manifest = json.loads((root / 'Bundles/AssetSceneManifest.json').read_bytes())
        prefixes = tuple(folders)
        nodes = {
            'BuildPlayer-' + scene['Name']
            for scene in manifest['Scenes']
            if any(asset.lower().endswith('.prefab') and asset.lower().startswith(prefixes)
                   for asset in scene['IncludedAssets'])
        }
        if not nodes:
            return load(path)
        try:
            return load(path, nodes=nodes, follow_dependencies=True)
        except ValueError as error:
            if not str(error).startswith('Requested UnityFS nodes are missing:'):
                raise
            # Let the writer report missing prefab roots as before.
            return load(path)

    with (patch.object(writer, 'stage_data', stage_data),
          patch.object(writer, 'load', load_for_decoration)):
        writer.main()


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument('--rust-root', type=Path)
    args, _ = parser.parse_known_args()
    if args.rust_root is None:
        writer.main()
    else:
        capture(args.rust_root)


if __name__ == '__main__':
    main()
