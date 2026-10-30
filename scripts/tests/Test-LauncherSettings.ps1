[CmdletBinding()]
param([string]$Launcher)
$ErrorActionPreference = 'Stop'
$packageRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Launcher) { $Launcher = Join-Path $packageRoot 'horizon-offline.exe' }
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('hof-options-' + [Guid]::NewGuid().ToString('N').Substring(0,12))
$original = Join-Path $scratch 'original Minecraft'
$gameHome = Join-Path $scratch 'Horizon Offline Data\game'
New-Item -ItemType Directory -Path $original, $gameHome | Out-Null
Copy-Item -LiteralPath $Launcher -Destination (Join-Path $scratch 'horizon-offline.exe')
# Presence is enough: importing settings must never execute the official launcher or a game.
[IO.File]::WriteAllText((Join-Path $scratch 'horizon-launcher.exe'), 'fixture, never execute')
$source = Join-Path $original 'options.txt'
$contents = "lang:ru_ru`nsoundVolume_master:0.45`nkey_key.forward:key.keyboard.up`nfov:0.5`n"
[IO.File]::WriteAllText($source, $contents)
[IO.File]::WriteAllText((Join-Path $gameHome 'options.txt'), 'previous player settings')
$hash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
$settings = @{ name = 'Player'; minecraft_dir = $original; launch = $null } | ConvertTo-Json
[IO.File]::WriteAllText((Join-Path $scratch 'horizon-rust.json'), $settings)
$start = New-Object Diagnostics.ProcessStartInfo
$start.FileName = Join-Path $scratch 'horizon-offline.exe'
$start.Arguments = '--menu'
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardInput = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$start.StandardOutputEncoding = [Text.Encoding]::UTF8
$start.StandardErrorEncoding = [Text.Encoding]::UTF8
foreach ($key in @('HORIZON_HOME','HORIZON_LAUNCHER_HOME','XDG_DATA_HOME','XDG_CONFIG_HOME','HORIZON_LAUNCHER_TRASH','HORIZON_LAUNCHER_CLIPBOARD','HORIZON_LAUNCHER_DESKTOP')) {
    $start.EnvironmentVariables[$key] = Join-Path $scratch $key
}
$start.EnvironmentVariables['HORIZON_LAUNCHER_NO_SELF_UPDATE'] = '1'
$start.EnvironmentVariables['HORIZON_RUST_STANDALONE'] = '1'
$start.EnvironmentVariables['HORIZON_NO_AUDIO'] = '1'
$start.EnvironmentVariables['NO_COLOR'] = '1'
$process = [Diagnostics.Process]::Start($start)
try {
    $outTask = $process.StandardOutput.ReadToEndAsync()
    $errTask = $process.StandardError.ReadToEndAsync()
    foreach ($line in @('7','','7')) { $process.StandardInput.WriteLine($line) }
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(10000)) { $process.Kill(); throw 'Settings menu timed out.' }
    $output = $outTask.GetAwaiter().GetResult()
    $errors = $errTask.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $scratch 'menu-output.txt'), $output + $errors)
    if ($process.ExitCode -or $errors) { throw "Settings menu failed: $errors" }
    if ([IO.File]::ReadAllText((Join-Path $gameHome 'options.txt')) -cne $contents) { throw 'Settings were not imported.' }
    $backups = @(Get-ChildItem -LiteralPath (Join-Path $gameHome '.options-backups') -File)
    if ($backups.Count -ne 1 -or [IO.File]::ReadAllText($backups[0].FullName) -cne 'previous player settings') { throw 'Old settings backup is missing or duplicated.' }
    if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne $hash) { throw 'Original Minecraft settings changed.' }
    if (-not $output.Contains('Настройки уже совпадают')) { throw 'Repeat import was not detected.' }
    if (-not $output.Contains('HORIZON OFFLINE') -or -not $output.Contains('by RRM Network')) { throw 'Branding is missing.' }
    Write-Output "PASS: real menu imports options, keeps one backup, preserves source and detects repeat. Evidence: $scratch"
} finally { $process.Dispose() }
