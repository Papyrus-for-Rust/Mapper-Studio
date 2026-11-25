#Requires -Version 7.0
param(
    [string]$Destination,
    [string]$BuildDirectory = 'target/release',
    [string]$GuiExecutable,
    [Parameter(Mandatory)][string]$PythonRuntimeDirectory,
    [Parameter(Mandatory)][string]$UnityPyModuleDirectory,
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.\-]+)?$')][string]$ReleaseVersion,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$')][string]$UpdateRepository
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workspaceRoot = Split-Path -Parent $PSScriptRoot
if ([bool]$ReleaseVersion -ne [bool]$UpdateRepository) {
    throw 'Supply both ReleaseVersion and UpdateRepository for an auto-update package.'
}
if (-not $Destination) {
    $packageName = if ($ReleaseVersion) { "Mapper-for-Rust-$ReleaseVersion-win-x64" } else { 'Mapper-for-Rust-win-x64' }
    $Destination = Join-Path 'artifacts/releases' $packageName
}
function WorkspacePath([string]$Path) {
    if (-not [IO.Path]::IsPathRooted($Path)) { $Path = Join-Path $workspaceRoot $Path }
    [IO.Path]::GetFullPath($Path)
}
$build = WorkspacePath $BuildDirectory
$output = WorkspacePath $Destination
if (Test-Path -LiteralPath $output) { throw 'Release destination already exists; select a new directory.' }
$parent = Split-Path -Parent $output
if (-not $parent) { throw 'Release destination must have a parent directory.' }
$console = Join-Path $build 'mapper.exe'
if (-not (Test-Path -LiteralPath $console -PathType Leaf)) { throw 'Build mapper.exe before staging a release.' }
if ($GuiExecutable) {
    $gui = Get-Item -LiteralPath (WorkspacePath $GuiExecutable)
} else {
    # Cargo can finish this linked binary while a running canonical GUI prevents
    # replacing the public executable. Prefer the newest compiler output.
    $candidates = @('mapper-studio.exe', 'deps/mapper_studio.exe') | ForEach-Object {
        $candidate = Join-Path $build $_
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { Get-Item -LiteralPath $candidate }
    }
    $gui = $candidates | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
    if (-not $gui) { throw 'Build mapper-studio.exe before staging a release.' }
}
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$temporary = [IO.Path]::GetFullPath((Join-Path $parent ('.mapper-stage-' + [Guid]::NewGuid().ToString('N'))))
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    Copy-Item -LiteralPath $console -Destination (Join-Path $temporary 'mapper.exe')
    Copy-Item -LiteralPath $gui.FullName -Destination (Join-Path $temporary 'mapper-studio.exe')
    Copy-Item -LiteralPath (Join-Path $workspaceRoot 'LICENSE') -Destination (Join-Path $temporary 'LICENSE')
    Copy-Item -LiteralPath (Join-Path $workspaceRoot 'licenses') -Destination (Join-Path $temporary 'licenses') -Recurse
    & (Join-Path $PSScriptRoot 'Stage-MapperMetadataRuntime.ps1') -Destination (Join-Path $temporary 'tools') -PythonRuntimeDirectory $PythonRuntimeDirectory -UnityPyModuleDirectory $UnityPyModuleDirectory | Out-Null
    & (Join-Path $PSScriptRoot 'Build-AssetReader.ps1') -Destination (Join-Path $temporary 'tools') | Out-Null
    $assetReaderSelfTest = & (Join-Path $temporary 'tools/mapper-asset-reader.exe') --self-test | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Packaged native asset reader could not start.' }
    $textureSourceSelfTest = & (Join-Path $temporary 'tools/mapper-texture-sources.exe') --self-test | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Packaged texture source reader could not start.' }
    & (Join-Path $PSScriptRoot 'Build-NativeWorker.ps1') -StudioDirectory $temporary -SelfContained
    $worker = Join-Path $temporary 'native/Papyrus.NativeWorker.exe'
    if (-not (Test-Path -LiteralPath $worker -PathType Leaf)) { throw 'Native worker was not published.' }
    $selfTest = & $worker --self-test
    if ($LASTEXITCODE -ne 0) { throw 'Packaged native worker could not start.' }
    $status = $selfTest | ConvertFrom-Json
    if ($status.format -ne 'papyrus-native-worker-self-test' -or $status.schema -ne 1 -or $status.platform -ne 'win-x64' -or $status.nativeLibraryLoaded) {
        throw 'Packaged native worker self-test returned an unsupported result.'
    }
    & (Join-Path $temporary 'mapper.exe') --version | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Packaged CLI could not start.' }
    if ($ReleaseVersion) {
        @{ version = $ReleaseVersion; repository = $UpdateRepository } |
            ConvertTo-Json -Compress |
            Set-Content -LiteralPath (Join-Path $temporary 'update-channel.json') -Encoding utf8NoBOM
    }
    $files = @(Get-ChildItem -LiteralPath $temporary -File -Recurse | Sort-Object FullName | ForEach-Object {
        if ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Release files cannot be reparse points.' }
        $relative = [IO.Path]::GetRelativePath($temporary, $_.FullName).Replace('\', '/')
        [ordered]@{ path = $relative; bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    $manifest = [ordered]@{ format = 'mapper-release-manifest'; schema = 1; platform = 'win-x64'; nativeWorkerSelfContained = $true; metadataRuntime = 'cpython312-unitypy1253-metadata1'; nativeCatalogReader = 'papyrus-native-autospawn-fields-water-1'; nativeCatalogReaderSelfTest = $assetReaderSelfTest; nativeTextureSourceReader = 'mapper-native-texture-sources-1'; nativeTextureSourceReaderSelfTest = $textureSourceSelfTest; workerSelfTest = $status; files = $files }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $temporary 'release-manifest.json') -Encoding utf8NoBOM
    & (Join-Path $PSScriptRoot 'Test-MapperRelease.ps1') -Directory $temporary -SkipLaunch | Out-Null
    # Only the private staging directory is moved, within the explicitly selected
    # output parent. Never replace an existing checkout or running application.
    if ([IO.Path]::GetDirectoryName($temporary) -ne $parent -or [IO.Path]::GetDirectoryName($output) -ne $parent) { throw 'Release staging paths escaped their output parent.' }
    [IO.Directory]::Move($temporary, $output)
    Write-Output $output
} catch {
    Write-Warning "Unpublished release staging files remain at $temporary"
    throw
}
