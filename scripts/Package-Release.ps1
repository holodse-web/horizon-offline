[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = Split-Path -Parent $project }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$cargo = [IO.File]::ReadAllText((Join-Path $project 'Cargo.toml'))
$version = [regex]::Match($cargo, '(?m)^version = "([0-9.]+)"').Groups[1].Value
if (-not $version) { throw 'Missing package version.' }
$executable = Join-Path $project 'horizon-offline.exe'
if ((& $executable --version) -ne "horizon-offline $version") { throw 'Build version does not match Cargo.toml.' }
$archive = Join-Path $OutputDirectory "Horizon-Offline-$version.zip"
$temporary = $archive + '.tmp'
Add-Type -AssemblyName System.IO.Compression
$stream = [IO.File]::Create($temporary)
try {
    $zip = New-Object IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create, $true)
    try {
        $files = [ordered]@{
            'horizon-offline.exe' = $executable
            'horizon-launcher.exe' = (Join-Path $project 'horizon-launcher.exe')
            'README.md' = (Join-Path $project 'docs\RELEASE-README.md')
            'README.ru.md' = (Join-Path $project 'docs\RELEASE-README.ru.md')
        }
        foreach ($name in $files.Keys) {
            $entry = $zip.CreateEntry($name, [IO.Compression.CompressionLevel]::Optimal)
            $target = $entry.Open()
            $source = [IO.File]::OpenRead($files[$name])
            try { $source.CopyTo($target) } finally { $source.Dispose(); $target.Dispose() }
        }
    } finally { $zip.Dispose() }
} finally { $stream.Dispose() }
Move-Item -LiteralPath $temporary -Destination $archive -Force
$lines = @($archive, $executable, (Join-Path $project 'horizon-launcher.exe')) | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant(), [IO.Path]::GetFileName($_)
}
[IO.File]::WriteAllLines((Join-Path $OutputDirectory 'SHA256SUMS.txt'), $lines, (New-Object Text.UTF8Encoding($false)))
Write-Output "Ready: $archive (2 EXEs + 2 short guides)"
