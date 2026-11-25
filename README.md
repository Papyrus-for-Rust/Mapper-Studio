# Mapper for Rust

Mapper is a Windows desktop app for making custom maps for Rust servers. Build a recipe, generate a map, then adjust monuments or routes before exporting it.

## Try the beta

1. Download the Windows x64 ZIP from [Releases](https://github.com/Papyrus-for-Rust/Mapper-Studio/releases), extract it, and open `mapper-studio.exe`.
2. Select your Rust installation. You can manage other installations from Settings. Mapper prepares the selected game files on first use and keeps a local cache for later runs.
3. Set the map size and enter a seed, or click the randomize button. Select **Generate** to open a map tab and watch the job progress.

You can queue more maps while one is running. Drag the six-dot grip on a waiting map to reorder it, or use the arrow beside **Generate** to queue a batch.

On a finished map, drag a monument to request a new position or click a road, trail, or rail to edit its spline. **Apply changes** processes the pending edits together. Use the map menu to export a `.map` or PNG.

Save a `.mapper` project to keep your maps and edits. Export a `.rec` recipe when you only want the generation settings.

## Beta limits

Mapper reads game data from your own installation. Supported builds and available features depend on what Mapper can verify there. If a map cannot be composed or exported, open its issue report in Queue. Test a map on a separate server before using it for a live wipe.

- [Build from source](docs/BUILD.md)
- [Command-line examples](docs/CLI.md)
- [Write a plugin](docs/PLUGINS.md)
- [Report a bug](https://github.com/Papyrus-for-Rust/Mapper-Studio/issues)

Mapper's original code is licensed under [AGPL-3.0-only](LICENSE). Game files and third-party components retain their own terms. Mapper for Rust is an independent project and is unaffiliated with Facepunch Studios.
