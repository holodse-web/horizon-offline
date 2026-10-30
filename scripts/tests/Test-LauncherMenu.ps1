[CmdletBinding()]
param([string]$Launcher)
$ErrorActionPreference = 'Stop'
$packageRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Launcher) { $Launcher = Join-Path $packageRoot 'horizon-offline.exe' }
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('hof-menu-' + [Guid]::NewGuid().ToString('N').Substring(0,12))
New-Item -ItemType Directory -Path $scratch | Out-Null
Copy-Item -LiteralPath $Launcher -Destination (Join-Path $scratch 'horizon-offline.exe')
[IO.File]::WriteAllText((Join-Path $scratch 'horizon-launcher.exe'), 'fixture, never execute')
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
$start.EnvironmentVariables['HORIZON_NO_AUDIO'] = '1'
$start.EnvironmentVariables['HORIZON_RUST_STANDALONE'] = '1'
$start.EnvironmentVariables['NO_COLOR'] = '1'
$process = [Diagnostics.Process]::Start($start)
try {
    $outputTask = $process.StandardOutput.ReadToEndAsync()
    $errorTask = $process.StandardError.ReadToEndAsync()
    # Quit is the ONLY input: this smoke test must never select a game mode.
    $process.StandardInput.WriteLine('0')
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(10000)) { $process.Kill(); throw 'Menu did not exit after the scripted quit.' }
    $output = $outputTask.GetAwaiter().GetResult()
    $errors = $errorTask.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $scratch 'menu-output.txt'), $output)
    if ($process.ExitCode -ne 0) { throw "Menu smoke failed: $errors" }
    foreach ($label in @('HORIZON OFFLINE', 'by RRM Network', 'Импортировать настройки Minecraft')) {
        if (-not $output.Contains($label)) { throw "Menu is missing: $label" }
    }
    $output
    if ($output.Contains('Fabric Java')) { throw 'Rejected Java game mode is still advertised.' }
    "PASS: Horizon Offline branding and menu; quit without starting a game. Evidence: $scratch"
} finally { $process.Dispose() }
