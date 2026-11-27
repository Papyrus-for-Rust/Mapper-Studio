#Requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$Directory,
    [string]$Destination = 'artifacts/releases',
    [string]$Compiler = 'ISCC.exe'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workspace = Split-Path -Parent $PSScriptRoot
$source = [IO.Path]::GetFullPath($Directory, $workspace)
$output = [IO.Path]::GetFullPath($Destination, $workspace)
if ($output.Equals($source, [StringComparison]::OrdinalIgnoreCase) -or
    $output.StartsWith($source.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Choose an installer output directory outside the staged release.'
}

& (Join-Path $PSScriptRoot 'Test-MapperRelease.ps1') -Directory $source -SkipLaunch | Out-Null
$channel = Get-Content -LiteralPath (Join-Path $source 'update-channel.json') -Raw | ConvertFrom-Json
$version = $channel.version
if ($version -isnot [string] -or $version -notmatch '^(?<numeric>[0-9]+\.[0-9]+\.[0-9]+)(?:-[0-9A-Za-z.-]+)?$') {
    throw 'Stage a release with -ReleaseVersion and -UpdateRepository first.'
}
$numeric = $Matches.numeric
$compilerPath = (Get-Command $Compiler -CommandType Application).Source
New-Item -ItemType Directory -Path $output -Force | Out-Null
$setup = Join-Path $output "Mapper-for-Rust-$version-Setup-win-x64.exe"
if (Test-Path -LiteralPath $setup) { throw "Installer already exists: $setup" }
& $compilerPath "/DPackageDirectory=$source" "/DReleaseVersion=$version" "/DNumericVersion=$numeric" "/O$output" (Join-Path $PSScriptRoot 'MapperSetup.iss')
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $setup -PathType Leaf)) {
    throw 'Inno Setup failed to build the Mapper installer.'
}
$hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $([IO.Path]::GetFileName($setup))" | Set-Content -LiteralPath "$setup.sha256" -Encoding ascii
Get-Item -LiteralPath $setup
