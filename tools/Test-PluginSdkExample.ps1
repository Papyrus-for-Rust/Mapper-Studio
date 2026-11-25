#Requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$MapperExecutable,
    [Parameter(Mandatory)][string]$Manifest,
    [Parameter(Mandatory)][string]$WorkDirectory,
    [string]$SavedProject
)
$ErrorActionPreference = 'Stop'
$mapper = (Resolve-Path -LiteralPath $MapperExecutable).Path
$manifestPath = (Resolve-Path -LiteralPath $Manifest).Path
$workRoot = (New-Item -ItemType Directory -Path $WorkDirectory -Force).FullName
$output = (New-Item -ItemType Directory -Path (Join-Path $workRoot ([guid]::NewGuid().ToString('N')))).FullName

function Invoke-Mapper([string[]]$Arguments) {
    $result = & $mapper @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Mapper command failed ($($Arguments[0..1] -join ' '))." }
    $result | ConvertFrom-Json -AsHashtable
}

function Get-WorkspaceReceipts([string]$Path) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Path))
    try {
        if ([Text.Encoding]::ASCII.GetString($reader.ReadBytes(8)) -cne 'MPRJ0001') { return $null }
        $length = $reader.ReadUInt64()
        if ($length -gt 64MB) { throw 'Unexpected workspace manifest size.' }
        $null = $reader.ReadBytes(32)
        $document = [Text.Encoding]::UTF8.GetString($reader.ReadBytes([int]$length)) | ConvertFrom-Json -AsHashtable
        [ordered]@{ workspace = $document.workspace; blobs = $document.blobs } | ConvertTo-Json -Depth 100 -Compress
    } finally {
        $reader.Dispose()
    }
}

$original = Join-Path $output 'original.mapper'
if ($SavedProject) {
    Copy-Item -LiteralPath (Resolve-Path -LiteralPath $SavedProject).Path -Destination $original
} else {
    Invoke-Mapper @('project', 'new', $original, '--seed', '12345', '--size', '4000') | Out-Null
}
$before = Invoke-Mapper @('project', 'validate', $original)
$beforeHash = (Get-FileHash -LiteralPath $original -Algorithm SHA256).Hash
$expected = Invoke-Mapper @('project', 'inspect', $original)
$enabled = $null -ne $expected.performance -and $expected.performance.monuments -eq $false
$package = Invoke-Mapper @('plugins', 'inspect', $manifestPath)
if ($package.manifest.id -cne 'papyrus.example.sdk-monument-policy' -or $package.manifest.actions.Count -ne 2) {
    throw 'Unexpected Plugin SDK example package.'
}
$params = Join-Path $output 'parameters.json'
@{ enabled = $enabled } | ConvertTo-Json -Compress | Set-Content -LiteralPath $params -Encoding utf8NoBOM
$changed = Join-Path $output 'transformed.mapper'
Invoke-Mapper @('plugins', 'run', $manifestPath, 'set-monument-policy', '--project', $original,
    '--parameters', $params, '--output', $changed) | Out-Null
$after = Invoke-Mapper @('project', 'validate', $changed)
$check = Invoke-Mapper @('plugins', 'check', $manifestPath, 'check-monument-policy', '--project', $changed)

$actual = Invoke-Mapper @('project', 'inspect', $changed)
$actualEnabled = $null -eq $actual.performance -or $actual.performance.monuments -ne $false
if ($actualEnabled -ne $enabled) { throw 'The plugin did not change monument generation.' }
if (-not $expected.Contains('performance')) { $expected.performance = [ordered]@{} }
if (-not $actual.Contains('performance')) { $actual.performance = [ordered]@{} }
$expected.performance.Remove('monuments')
$actual.performance.Remove('monuments')
if (($expected.performance | ConvertTo-Json -Compress) -cne ($actual.performance | ConvertTo-Json -Compress)) {
    throw 'The plugin changed unrelated performance fields.'
}
# JSON object order is irrelevant. This fixture edits only the performance flag,
# so compare the rest of both recipe objects in their original field order.
$expected.Remove('performance')
$actual.Remove('performance')
if (($expected | ConvertTo-Json -Depth 100 -Compress) -cne ($actual | ConvertTo-Json -Depth 100 -Compress)) {
    throw 'The plugin changed unrelated recipe fields.'
}
if (-not $before.valid -or -not $after.valid -or $before.saved_payloads_verified -ne $after.saved_payloads_verified -or
    (Get-WorkspaceReceipts $original) -cne (Get-WorkspaceReceipts $changed)) {
    throw 'Workspace validation or payload preservation failed.'
}
if ($beforeHash -cne (Get-FileHash -LiteralPath $original -Algorithm SHA256).Hash) {
    throw 'The original project was modified.'
}
$requiredCount = @($expected.targets | Where-Object { $_.presence -eq 'Required' }).Count
if ($check.report.summary -cne "$requiredCount required monument targets" -or
    @($check.report.findings).Count -ne [int](-not $enabled -and $requiredCount -gt 0)) {
    throw 'The plugin did not return the expected policy report.'
}

$invalid = Join-Path $output 'invalid.json'
'{"enabled":"false"}' | Set-Content -LiteralPath $invalid -Encoding utf8NoBOM
$invalidOutput = Join-Path $output 'invalid.mapper'
& $mapper plugins run $manifestPath set-monument-policy --project $original --parameters $invalid --output $invalidOutput 2> (Join-Path $output 'invalid-response.log') | Out-Null
if ($LASTEXITCODE -eq 0 -or (Test-Path -LiteralPath $invalidOutput)) { throw 'Invalid plugin input was accepted or published.' }

[ordered]@{
    inspected = $true
    transformVerified = $true
    report = $check.report
    savedPayloadsVerified = [int]$after.saved_payloads_verified
    rejectedInvalidInput = $true
    outputDirectory = $output
} | ConvertTo-Json -Depth 8
