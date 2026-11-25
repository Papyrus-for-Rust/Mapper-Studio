param([string]$OutputDirectory = 'artifacts/plugins/recipe-tools')
$ErrorActionPreference = 'Stop'
& cargo build -p mapper-studio --example recipe_plugin --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Example plugin build failed' }
$taskPluginDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $taskPluginDirectory -Force | Out-Null
$taskPluginExecutable = Join-Path $taskPluginDirectory 'recipe-tools.exe'
Copy-Item -LiteralPath 'target/release/examples/recipe_plugin.exe' -Destination $taskPluginExecutable -Force
$taskPluginManifest = [ordered]@{
    format = 'papyrus-mapper-plugin'
    api_version = 1
    id = 'papyrus.example.recipe-tools'
    name = 'Recipe tools example'
    version = '0.1.0'
    description = 'Example local plugin for monument seeds and content policy.'
    executable = 'recipe-tools.exe'
    executable_sha256 = (Get-FileHash -LiteralPath $taskPluginExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    capabilities = @('recipe.transform')
    actions = @(
        [ordered]@{id='shuffle-monuments';title='Shuffle monument seed';description='Advance only the monument seed. Optional parameters: steps (1-1000).'},
        [ordered]@{id='set-monument-policy';title='Set monument generation';description='Set the monument policy with parameters: enabled (true or false).'}
    )
}
$taskPluginManifestPath = Join-Path $taskPluginDirectory 'mapper-plugin.json'
$taskPluginJson = $taskPluginManifest | ConvertTo-Json -Depth 6
[System.IO.File]::WriteAllText($taskPluginManifestPath, $taskPluginJson, [System.Text.UTF8Encoding]::new($false))
Write-Output $taskPluginManifestPath
