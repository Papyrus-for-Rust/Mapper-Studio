#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Destination)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$project = Join-Path $PSScriptRoot 'Mapper.AssetProbe'
& cargo build --manifest-path (Join-Path $project 'Cargo.toml') --release --locked --bins -j1
if ($LASTEXITCODE -ne 0) { throw 'Native asset reader build failed.' }
$metadata = & cargo metadata --manifest-path (Join-Path $project 'Cargo.toml') --format-version 1 --no-deps | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $metadata.target_directory) { throw 'Cannot locate the native reader build directory.' }
$reader = Join-Path $metadata.target_directory 'release/mapper-asset-reader.exe'
$status = & $reader --self-test | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $status.format -cne 'mapper-native-catalog-reader-self-test' -or $status.schema -ne 1 -or $status.backend -cne 'unity-rs-core-0.6.2') { throw 'Native asset reader self-test failed.' }
function SourceFingerprint([string[]]$Names) {
    $digest = [Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
    try {
        foreach ($name in $Names) {
            $relative = if ($name.EndsWith('.rs')) { 'src/' + $name } else { $name }
            $digest.AppendData([Text.Encoding]::UTF8.GetBytes($name))
            $digest.AppendData([byte[]]@(0))
            $digest.AppendData([IO.File]::ReadAllBytes((Join-Path $project $relative)))
        }
        [Convert]::ToHexString($digest.GetHashAndReset()).ToLowerInvariant()
    } finally { $digest.Dispose() }
}
$expected = SourceFingerprint @('main.rs','geometry.rs','pages.rs','values.rs','map_labels.rs','Cargo.toml','Cargo.lock')
if ($status.extractor_sha256 -cne $expected) { throw 'Native asset reader differs from the checked-out source.' }
$textureReader = Join-Path $metadata.target_directory 'release/mapper-texture-sources.exe'
$textureStatus = & $textureReader --self-test | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $textureStatus.format -cne 'mapper-native-texture-sources-self-test' -or $textureStatus.schema -ne 1 -or $textureStatus.backend -cne 'unity-rs-core-0.6.2') { throw 'Native texture source reader self-test failed.' }
$textureExpected = SourceFingerprint @('bin/mapper-texture-sources.rs','texture_sources.rs','pages.rs','Cargo.toml','Cargo.lock')
if ($textureStatus.extractor_sha256 -cne $textureExpected) { throw 'Native texture source reader differs from the checked-out source.' }
if (-not [IO.Path]::IsPathRooted($Destination)) { $Destination = Join-Path $workspaceRoot $Destination }
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
Copy-Item -LiteralPath $reader -Destination (Join-Path $Destination 'mapper-asset-reader.exe')
Copy-Item -LiteralPath $textureReader -Destination (Join-Path $Destination 'mapper-texture-sources.exe')
$status | ConvertTo-Json -Compress
