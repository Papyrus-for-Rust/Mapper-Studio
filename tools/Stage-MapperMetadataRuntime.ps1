#Requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$Destination,
    [Parameter(Mandatory)][string]$PythonRuntimeDirectory,
    [Parameter(Mandatory)][string]$UnityPyModuleDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$pythonRoot = (Resolve-Path -LiteralPath $PythonRuntimeDirectory).Path
$moduleRoot = (Resolve-Path -LiteralPath $UnityPyModuleDirectory).Path
if (Test-Path -LiteralPath $Destination) { throw 'Metadata runtime destination must be new.' }
$output = [IO.Path]::GetFullPath($Destination)
# Exact ABI and parser version: never mix unrelated binary wheels/interpreters.
$version = & (Join-Path $pythonRoot 'python.exe') -I -B -c 'import sys,struct; print(str(sys.version_info.major)+"."+str(sys.version_info.minor)+"/"+str(struct.calcsize("P")*8))'
if ($LASTEXITCODE -ne 0 -or $version -ne '3.12/64') { throw 'Metadata runtime requires CPython 3.12 x64.' }
$metadata = Get-Content -LiteralPath (Join-Path $moduleRoot 'unitypy-1.25.3.dist-info/METADATA') -Raw
if ($metadata -notmatch '(?m)^Version: 1\.25\.3\r?$') { throw 'Metadata parser must be UnityPy 1.25.3.' }
function CopyRuntimeFile([string]$Root, [string]$Relative, [string]$Target) {
    $source = Get-Item -LiteralPath (Join-Path $Root $Relative)
    if ($source.PSIsContainer -or ($source.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Runtime inputs must be ordinary files.' }
    $destinationFile = Join-Path $Target $Relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $destinationFile) -Force | Out-Null
    Copy-Item -LiteralPath $source.FullName -Destination $destinationFile
}
$py = Join-Path $output 'python'
$modules = Join-Path $output 'unitypy'
foreach ($name in @('python.exe','python3.dll','python312.dll','vcruntime140.dll','vcruntime140_1.dll','LICENSE.txt')) {
    CopyRuntimeFile $pythonRoot $name $py
}
foreach ($folder in @('Lib','DLLs')) {
    foreach ($file in Get-ChildItem -LiteralPath (Join-Path $pythonRoot $folder) -File -Recurse) {
        $relative = [IO.Path]::GetRelativePath($pythonRoot, $file.FullName)
        if ($relative -match '(^|[\\/])(site-packages|__pycache__|tests?|idlelib|tkinter|turtledemo)([\\/]|$)' -or $file.Extension -eq '.pyc' -or $file.Name -match '^_tkinter') { continue }
        CopyRuntimeFile $pythonRoot $relative $py
    }
}
# Exclude audio conversion and its native FMOD dependency. All remaining package
# sources, compiled codec wheels, resources and license metadata are retained.
foreach ($file in Get-ChildItem -LiteralPath $moduleRoot -File -Recurse) {
    $relative = [IO.Path]::GetRelativePath($moduleRoot, $file.FullName)
    if ($relative -match '(^|[\\/])(__pycache__|bin|fmod_toolkit[^\\/]*|pyfmodex[^\\/]*)([\\/]|$)' -or $file.Extension -in @('.pyc','.dll','.so','.dylib')) { continue }
    CopyRuntimeFile $moduleRoot $relative $modules
}
# UnityPy eagerly imports its optional audio converter. Move that one import to
# the conversion call; metadata and texture decoding require no audio library.
$audioPath = Join-Path $modules 'UnityPy/export/AudioClipConverter.py'
$audio = [IO.File]::ReadAllText($audioPath).Replace("`r`n", "`n")
$importPattern = '(?m)^[\t ]*import fmod_toolkit[\t ]*\n'
$returnPattern = '(?m)^[\t ]*return fmod_toolkit\.raw_to_wav\('
if ([regex]::Matches($audio, $importPattern).Count -ne 1 -or [regex]::Matches($audio, $returnPattern).Count -ne 1) { throw 'Unsupported UnityPy audio import layout.' }
# Consume the complete import line, including indentation. Restaging a previously
# patched runtime must not leave whitespace attached to the following statement.
$audio = [regex]::Replace($audio, $importPattern, '')
$audio = [regex]::Replace($audio, $returnPattern, "    import fmod_toolkit`n    return fmod_toolkit.raw_to_wav(")
[IO.File]::WriteAllText($audioPath, $audio)
$notice = @'
Mapper metadata runtime: CPython 3.12 x64 and UnityPy 1.25.3.
Component licenses are retained in python/LICENSE.txt and unitypy/*dist-info.
UnityPy/export/AudioClipConverter.py is modified by Papyrus for Rust:
the fmod_toolkit import is deferred until audio conversion is called.
Audio conversion and FMOD binaries are not shipped or supported by this runtime.
No Rust game assets or native game libraries are included.
'@
[IO.File]::WriteAllText((Join-Path $output 'METADATA-RUNTIME.txt'), $notice)
# -B prevents mutable bytecode files from appearing in a verified release.
& (Join-Path $py 'python.exe') -I -B -c 'import sys; sys.path.insert(0,sys.argv[1]); import UnityPy,lz4.block; from UnityPy.export import Texture2DConverter; print(UnityPy.__version__)' $modules
if ($LASTEXITCODE -ne 0) { throw 'Packaged metadata runtime could not load its parser and texture decoder.' }
Write-Output $output
