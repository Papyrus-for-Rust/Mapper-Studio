param([string]$OutputDirectory = 'artifacts/plugins/sdk-monument-policy')
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$artifact = $null
& cargo build --manifest-path (Join-Path $workspace 'Cargo.toml') -p mapper-plugin-sdk --example monument_policy --locked --message-format=json-render-diagnostics | ForEach-Object {
    $event = $_ | ConvertFrom-Json
    if ($event.reason -eq 'compiler-artifact' -and $event.executable -and $event.target.name -eq 'monument_policy' -and $event.target.kind -contains 'example') {
        $artifact = $event.executable
    }
    if ($event.reason -eq 'compiler-message' -and $event.message.rendered) {
        Write-Host $event.message.rendered.TrimEnd()
    }
}
if ($LASTEXITCODE -ne 0) { throw 'Plugin SDK example build failed.' }
if (-not $artifact -or -not (Test-Path -LiteralPath $artifact -PathType Leaf)) { throw 'Cargo did not report the plugin executable.' }
$directory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$exe = Join-Path $directory 'monument-policy.exe'
Copy-Item -LiteralPath $artifact -Destination $exe -Force
$manifestPath = Join-Path $directory 'mapper-plugin.json'
$manifest = [ordered]@{
    format = 'papyrus-mapper-plugin'
    api_version = 1
    id = 'papyrus.example.sdk-monument-policy'
    name = 'Monument policy'
    version = '0.1.0'
    description = 'Set whether a recipe generates monuments.'
    executable = 'monument-policy.exe'
    executable_sha256 = (Get-FileHash -LiteralPath $exe).Hash.ToLowerInvariant()
    capabilities = @('recipe.transform', 'recipe.inspect')
    actions = @([ordered]@{
        id = 'set-monument-policy'
        title = 'Set monument generation'
        description = 'Change monument generation for this recipe.'
        parameters = @([ordered]@{
            id = 'enabled'
            label = 'Generate monuments'
            control = [ordered]@{ type = 'boolean'; default = $true }
        })
    }, [ordered]@{
        id = 'check-monument-policy'
        title = 'Check monument policy'
        description = 'Report required targets and whether monuments are disabled.'
        capability = 'recipe.inspect'
        parameters = @()
    })
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $manifestPath -Encoding utf8NoBOM
Write-Output $manifestPath
