param([string]$StudioDirectory = 'target/debug', [switch]$SelfContained)
$ErrorActionPreference = 'Stop'
if ($SelfContained -and $PSVersionTable.PSVersion.Major -lt 7) { throw 'Self-contained worker packaging requires PowerShell 7 or newer.' }
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$destination = if ([IO.Path]::IsPathRooted($StudioDirectory)) { $StudioDirectory } else { Join-Path $workspaceRoot $StudioDirectory }
if (-not (Test-Path -LiteralPath $destination -PathType Container)) { throw 'Build the studio directory first.' }
$publishArguments = @('publish', (Join-Path $workspaceRoot 'src/Papyrus.NativeWorker/Papyrus.NativeWorker.csproj'), '-c', 'Release', '-r', 'win-x64', '--self-contained', $SelfContained.IsPresent.ToString().ToLowerInvariant(), '-o', (Join-Path $destination 'native'))
if ($SelfContained) {
    $publishArguments += @('-p:PublishSingleFile=true', '-p:IncludeNativeLibrariesForSelfExtract=true', '-p:DebugType=None', '-p:DebugSymbols=false')
}
dotnet @publishArguments
if ($LASTEXITCODE -ne 0) { throw 'Native worker build failed.' }
if ($SelfContained) {
    # Retain the notices from the exact restored runtime pack, rather than
    # assuming a runtime version or copying notices from another installed SDK.
    $assetsPath = Join-Path $workspaceRoot 'src/Papyrus.NativeWorker/obj/project.assets.json'
    $assets = Get-Content -LiteralPath $assetsPath -Raw | ConvertFrom-Json -AsHashtable
    $runtime = @($assets.project.frameworks.Values.downloadDependencies | Where-Object { $_.name -eq 'Microsoft.NETCore.App.Runtime.win-x64' })
    if ($runtime.Count -ne 1) { throw 'Cannot resolve the published runtime pack notices.' }
    $version = [regex]::Match($runtime[0].version, '^\[([0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.]+)?), \1\]$')
    if (-not $version.Success) { throw 'Runtime pack version is not an exact restored version.' }
    $packs = @($assets.packageFolders.Keys | ForEach-Object {
        $pack = Join-Path $_ ('microsoft.netcore.app.runtime.win-x64/' + $version.Groups[1].Value)
        if ((Test-Path -LiteralPath (Join-Path $pack 'LICENSE.TXT')) -and (Test-Path -LiteralPath (Join-Path $pack 'THIRD-PARTY-NOTICES.TXT'))) { $pack }
    })
    if ($packs.Count -ne 1) { throw 'Restored runtime notices are missing or ambiguous.' }
    $notices = Join-Path $destination 'native/licenses/dotnet'
    New-Item -ItemType Directory -Path $notices -Force | Out-Null
    foreach ($name in @('LICENSE.TXT', 'THIRD-PARTY-NOTICES.TXT')) {
        Copy-Item -LiteralPath (Join-Path $packs[0] $name) -Destination (Join-Path $notices $name)
    }
}
