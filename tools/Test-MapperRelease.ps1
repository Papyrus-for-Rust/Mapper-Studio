#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Directory, [switch]$SkipLaunch)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workspaceRoot = Split-Path -Parent $PSScriptRoot
if (-not [IO.Path]::IsPathRooted($Directory)) { $Directory = Join-Path $workspaceRoot $Directory }
$root = [IO.Path]::GetFullPath($Directory).TrimEnd([IO.Path]::DirectorySeparatorChar)
$rootItem = Get-Item -LiteralPath $root
if (-not $rootItem.PSIsContainer -or ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Release root must be an ordinary directory.' }
$manifestPath = Join-Path $root 'release-manifest.json'
if ((Get-Item -LiteralPath $manifestPath).Length -gt 1024 * 1024) { throw 'Release manifest exceeds one MiB.' }
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json -AsHashtable
if ($manifest.format -ne 'mapper-release-manifest' -or $manifest.schema -ne 1 -or $manifest.platform -ne 'win-x64' -or $manifest.nativeWorkerSelfContained -ne $true -or $manifest.files.Count -lt 6 -or $manifest.files.Count -gt 8192) {
    throw 'Unsupported or incomplete Mapper release manifest.'
}
$children = @(Get-ChildItem -LiteralPath $root -Recurse -Force)
if ($children | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) { throw 'Release entries cannot be reparse points.' }
$paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$totalBytes = 0L
foreach ($entry in $manifest.files) {
    $relative = $entry.path
    if ($relative -isnot [string] -or $relative -notmatch '^[A-Za-z0-9._/-]+$' -or [IO.Path]::IsPathRooted($relative) -or -not $paths.Add($relative)) { throw 'Invalid or duplicate release path.' }
    $path = [IO.Path]::GetFullPath((Join-Path $root $relative))
    if (-not $path.StartsWith($root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetRelativePath($root, $path).Replace('\', '/') -cne $relative) { throw 'Release path escaped its root or is not canonical.' }
    $file = Get-Item -LiteralPath $path
    # Python packages contain valid empty __init__.py files. Bound the complete
    # runtime as well as each file, with a manifest-size limit above.
    if ($file.PSIsContainer -or ($entry.bytes -isnot [long] -and $entry.bytes -isnot [int]) -or $entry.bytes -lt 0 -or $entry.bytes -gt 1024 * 1024 * 1024 -or $file.Length -ne $entry.bytes) { throw "Release file length differs: $relative" }
    $totalBytes += $entry.bytes
    if ($totalBytes -gt 1024 * 1024 * 1024) { throw 'Release exceeds one GiB aggregate payload budget.' }
    if ($entry.sha256 -isnot [string] -or $entry.sha256 -cnotmatch '^[0-9a-f]{64}$' -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $entry.sha256) { throw "Release file checksum differs: $relative" }
}
foreach ($required in @('mapper.exe', 'mapper-studio.exe', 'LICENSE', 'native/Papyrus.NativeWorker.exe', 'native/licenses/dotnet/LICENSE.TXT', 'native/licenses/dotnet/THIRD-PARTY-NOTICES.TXT')) {
    if (-not $paths.Contains($required)) { throw "Release is missing a required file: $required" }
}
if ($manifest.ContainsKey('metadataRuntime')) {
    if ($manifest.metadataRuntime -cne 'cpython312-unitypy1253-metadata1') { throw 'Unsupported metadata runtime.' }
    foreach ($required in @('tools/python/python.exe','tools/python/python312.dll','tools/python/LICENSE.txt','tools/unitypy/UnityPy/__init__.py','tools/unitypy/unitypy-1.25.3.dist-info/licenses/LICENSE','tools/METADATA-RUNTIME.txt')) {
        if (-not $paths.Contains($required)) { throw "Release is missing metadata runtime file: $required" }
    }
}
foreach ($file in $children | Where-Object { -not $_.PSIsContainer }) {
    $relative = [IO.Path]::GetRelativePath($root, $file.FullName).Replace('\', '/')
    if ($relative -cne 'release-manifest.json' -and -not $paths.Contains($relative)) { throw "Release contains an unlisted file: $relative" }
}
$assetReaderStatus = $null
$textureSourceStatus = $null
if ($manifest.ContainsKey('nativeCatalogReader')) {
    if ($manifest.nativeCatalogReader -cne 'papyrus-native-autospawn-fields-water-1') { throw 'Unsupported native catalog reader.' }
    foreach ($required in @('tools/mapper-asset-reader.exe','licenses/unity-rs-core-0.6.2-MIT.txt','licenses/unity-rs-core-0.6.2-THIRD-PARTY.txt')) {
        if (-not $paths.Contains($required)) { throw "Release is missing native reader file: $required" }
    }
}
if ($manifest.ContainsKey('nativeTextureSourceReader')) {
    if ($manifest.nativeTextureSourceReader -cne 'mapper-native-texture-sources-1' -or -not $manifest.ContainsKey('nativeCatalogReader') -or -not $paths.Contains('tools/mapper-texture-sources.exe')) { throw 'Unsupported or missing native texture source reader.' }
}
$status = $null
$version = $null
if (-not $SkipLaunch) {
    if ($manifest.ContainsKey('nativeCatalogReader')) {
        $assetReaderStatus = & (Join-Path $root 'tools/mapper-asset-reader.exe') --self-test | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or $assetReaderStatus.format -cne 'mapper-native-catalog-reader-self-test' -or $assetReaderStatus.schema -ne 1 -or $assetReaderStatus.extractor_sha256 -cne $manifest.nativeCatalogReaderSelfTest.extractor_sha256) { throw 'Packaged native asset reader self-test differs.' }
        $applicationReader = & (Join-Path $root 'mapper.exe') installation reader | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or $applicationReader.format -cne 'mapper-native-catalog-reader-status-1' -or $applicationReader.reader_present -ne $true -or $applicationReader.extractor_sha256 -cne $assetReaderStatus.extractor_sha256) { throw 'Mapper and its native asset reader use different code versions.' }
        if ($manifest.ContainsKey('nativeTextureSourceReader')) {
            $textureSourceStatus = & (Join-Path $root 'tools/mapper-texture-sources.exe') --self-test | ConvertFrom-Json
            if ($LASTEXITCODE -ne 0 -or $textureSourceStatus.format -cne 'mapper-native-texture-sources-self-test' -or $textureSourceStatus.schema -ne 1 -or $textureSourceStatus.extractor_sha256 -cne $manifest.nativeTextureSourceReaderSelfTest.extractor_sha256 -or $applicationReader.texture_source_reader.reader_present -ne $true -or $applicationReader.texture_source_reader.extractor_sha256 -cne $textureSourceStatus.extractor_sha256) { throw 'Mapper and its native texture source reader use different code versions.' }
        }
    }
    if ($paths.Contains('tools/python/python.exe')) {
        & (Join-Path $root 'tools/python/python.exe') -I -B -c 'import sys; sys.path.insert(0,sys.argv[1]); import UnityPy,lz4.block; from UnityPy.export import Texture2DConverter; print(UnityPy.__version__)' (Join-Path $root 'tools/unitypy') | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Packaged metadata parser/texture decoder could not start.' }
    }
    $status = & (Join-Path $root 'native/Papyrus.NativeWorker.exe') --self-test
    if ($LASTEXITCODE -ne 0) { throw 'Packaged native worker could not start.' }
    $status = $status | ConvertFrom-Json
    if ($status.format -ne 'papyrus-native-worker-self-test' -or $status.schema -ne 1 -or $status.platform -ne 'win-x64' -or $status.nativeLibraryLoaded) { throw 'Packaged native worker returned an invalid self-test.' }
    $version = & (Join-Path $root 'mapper.exe') --version
    if ($LASTEXITCODE -ne 0) { throw 'Packaged Mapper CLI could not start.' }
}
[ordered]@{ format = 'mapper-release-verification'; schema = 1; filesVerified = $paths.Count; workerSelfTest = $status; assetReaderSelfTest = $assetReaderStatus; textureSourceReaderSelfTest = $textureSourceStatus; cliVersion = $version } | ConvertTo-Json -Depth 5
