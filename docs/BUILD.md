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

## Windows installer

Build the Windows setup wizard from the same verified directory used for the ZIP. Run this with [Inno Setup](https://jrsoftware.org/inno-setup/downloads) 6.6 or later and point `-Compiler` to `ISCC.exe` if it is outside your PATH.

```powershell
./tools/Build-MapperInstaller.ps1 -Directory artifacts/releases/Mapper-for-Rust-0.1.0-beta.1-win-x64
```

Attach `Mapper-for-Rust-0.1.0-beta.1-Setup-win-x64.exe` and its checksum to the same release. Open the setup executable and choose **Install**. Setup includes the runtime and adds Mapper to the Start menu for the current Windows account. Use Windows Settings to uninstall. For unattended installs, pass `/SILENT /NORESTART`. Keep publishing the ZIP and its checksum for in-app updates. The installer uses the same update channel and leaves projects and preferences in place.

The installer and app share the Papyrus artwork in `src/mapper-studio/assets/branding`. After changing the SVG, regenerate the transparent PNG and multi-size ICO. Then run `rc.exe /nologo /fo papyrus.res papyrus.rc` from the branding directory using the Windows SDK resource compiler. The checked-in icon resource keeps ordinary Cargo builds independent of the resource compiler.

## Check changes

```powershell
cargo test --locked --workspace -j1
```

If you're reporting a generation issue, include the Mapper version, map size, seeds, recipe, and the failed stage from Queue. Leave game files and private server settings out of the report.
