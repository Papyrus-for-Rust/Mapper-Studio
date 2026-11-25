# Build Mapper for Rust

The beta targets Windows x64. Install the stable Rust MSVC toolchain, the Visual Studio C++ build tools, the .NET 8 SDK, and PowerShell 7.

```powershell
cargo build --locked --release -p mapper-studio --bins -j1
pwsh -File tools/Build-NativeWorker.ps1 -StudioDirectory target/release
./target/release/mapper-studio.exe
```

The GUI is `mapper-studio.exe`. The CLI is `mapper.exe`. The worker runs native terrain generation separately from the app. For local asset preparation, build the readers with `tools/Build-AssetReader.ps1 -Destination target/release/tools`.

## Package

Keep an x64 CPython 3.12 installation and UnityPy 1.25.3 with its dependencies available. Pass their directories to the staging script. It bundles a self-contained .NET worker, the two asset readers, the Python runtime, and license notices.

```powershell
./tools/Stage-MapperRelease.ps1 `
  -PythonRuntimeDirectory 'C:/build-runtimes/python312' `
  -UnityPyModuleDirectory 'C:/build-runtimes/unitypy' `
  -ReleaseVersion 0.1.0-beta.1 `
  -UpdateRepository Papyrus-for-Rust/Mapper-Studio
./tools/Test-MapperRelease.ps1 -Directory artifacts/releases/Mapper-for-Rust-0.1.0-beta.1-win-x64
```

You can compile on another drive using `CARGO_TARGET_DIR` and pass its `release` subdirectory as `-BuildDirectory`. Finished packages go under `artifacts/releases` by default.

For GitHub, ZIP the staged directory and attach it to a prerelease tagged `v0.1.0-beta.1`. Name the asset `Mapper-for-Rust-0.1.0-beta.1-win-x64.zip`. Include the corresponding source and publish its SHA-256. Mapper checks the public release feed when updates are enabled.

## Check changes

```powershell
cargo test --locked --workspace -j1
```

If you're reporting a generation issue, include the Mapper version, map size, seeds, recipe, and the failed stage from Queue. Leave game files and private server settings out of the report.
