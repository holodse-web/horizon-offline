# Run menu item 6 against a fake official launcher, using only isolated fixture data.
[CmdletBinding()]
param([string]$Launcher)
$ErrorActionPreference = 'Stop'
$packageRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Launcher) { $Launcher = Join-Path $packageRoot 'horizon-offline.exe' }
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('hof-update-' + [Guid]::NewGuid().ToString('N').Substring(0,12))
New-Item -ItemType Directory -Path $scratch | Out-Null
$source = Join-Path $scratch 'fixture.cs'
$fixture = Join-Path $scratch 'fixture.exe'
[IO.File]::WriteAllText($source, @'
using System;
using System.IO;
using System.Text;
class Fixture {
    static int Main(string[] args) {
        Console.OutputEncoding = new UTF8Encoding(false);
        string home = Environment.GetEnvironmentVariable("HORIZON_LAUNCHER_HOME");
        string expectedHome = Environment.GetEnvironmentVariable("HORIZON_FIXTURE_HOME");
        if ((home != expectedHome && !home.StartsWith(Path.Combine(expectedHome, ".rust-updates") + Path.DirectorySeparatorChar)) ||
            Environment.GetEnvironmentVariable("HORIZON_LAUNCHER_NO_SELF_UPDATE") != "1") return 99;
        Directory.CreateDirectory(home);
        Directory.CreateDirectory(expectedHome);
        File.AppendAllText(Path.Combine(expectedHome, "calls.log"), String.Join(" ", args) + "\n");
        string mode = Environment.GetEnvironmentVariable("HORIZON_FIXTURE_MODE");
        string versions = Path.Combine(home, "versions");
        string target = Path.Combine(versions, "nightly-new");
        if (args[0] == "versions" && args[1] == "--installed") {
            string tags = "";
            if (Directory.Exists(versions)) foreach (string dir in Directory.GetDirectories(versions)) {
                if (File.Exists(Path.Combine(dir, ".installed"))) {
                    if (tags.Length > 0) tags += ",";
                    tags += "\"" + Path.GetFileName(dir) + "\"";
                }
            }
            Console.WriteLine("{\"installed\":[" + tags + "]}");
            return 0;
        }
        if (args[0] == "versions") {
            if (mode == "offline") { Console.Error.WriteLine("offline fixture"); return 7; }
            if (mode == "bad-json") { Console.WriteLine("not json"); return 0; }
            if (mode == "empty") { Console.WriteLine("[]"); return 0; }
            string available = mode == "unsupported" ? "false" : "true";
            string installed = File.Exists(Path.Combine(target, ".installed")) ? "true" : "false";
            Console.WriteLine("[{\"tag\":\"archived\",\"available_here\":true,\"installed\":false,\"prerelease\":true}," +
                "{\"tag\":\"nightly-new\",\"available_here\":" + available + ",\"installed\":" + installed + ",\"prerelease\":false}]");
            return 0;
        }
        if (args[0] == "install") {
            if (args[1] != "nightly-new") return 98;
            if (mode == "install-fails") return 8;
            if (mode != "unconfirmed") {
                Directory.CreateDirectory(target);
                File.WriteAllText(Path.Combine(target, "horizon-client.exe"), "fixture new client, never run");
                File.WriteAllText(Path.Combine(target, ".installed"), "");
                File.WriteAllText(Path.Combine(target, "unchanged.txt"), "same physical file");
                File.WriteAllText(Path.Combine(target, "added.txt"), "new file");
                File.WriteAllText(Path.Combine(target, "research-lock.json"), "{\"minecraft\":{\"version\":\"26.3\",\"client_sha1\":\"c\",\"server_sha1\":\"s\"}}");
                string sources = Path.Combine(target, "crates/horizon-prepare/src");
                Directory.CreateDirectory(sources);
                File.WriteAllText(Path.Combine(sources, "lib.rs"), "pub const OUTPUT_VERSION: u32 = 1;");
            }
            return 0;
        }
        return 97;
    }
}
'@)
$compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
& $compiler /nologo /target:exe "/out:$fixture" $source
if ($LASTEXITCODE) { throw 'Fixture compiler failed.' }
$results = @()
foreach ($layout in @('standalone', 'package')) {
    foreach ($mode in @('current', 'repeat', 'first', 'new', 'offline', 'bad-json', 'empty', 'unsupported', 'install-fails', 'unconfirmed')) {
        $root = Join-Path $scratch ($layout + '-' + $mode + ' space ' + [char]0x044F)
        New-Item -ItemType Directory -Path $root | Out-Null
        Copy-Item -LiteralPath $Launcher -Destination (Join-Path $root 'horizon-offline.exe')
        Copy-Item -LiteralPath $fixture -Destination (Join-Path $root 'horizon-launcher.exe')
        if ($layout -eq 'package') {
            New-Item -ItemType Directory -Path (Join-Path $root 'scripts') | Out-Null
            [IO.File]::WriteAllText((Join-Path $root 'scripts\Start-HorizonFabric.ps1'), "throw 'Unexpected preflight'")
            $data = Join-Path $root 'isolated launcher data'
        } else { $data = Join-Path $root 'Horizon Rust Data' }
        New-Item -ItemType Directory -Path $data | Out-Null
        if ($mode -ne 'first') {
            $tag = if ($mode -in @('current', 'repeat')) { 'nightly-new' } else { 'old' }
            $version = Join-Path $data ('versions\' + $tag)
            New-Item -ItemType Directory -Path $version | Out-Null
            [IO.File]::WriteAllText((Join-Path $version '.installed'), '')
            [IO.File]::WriteAllText((Join-Path $version 'horizon-client.exe'), 'fixture only, never run')
            [IO.File]::WriteAllText((Join-Path $version 'unchanged.txt'), 'same physical file')
            [IO.File]::SetLastWriteTimeUtc((Join-Path $version 'unchanged.txt'), ([DateTime]'2020-01-02T03:04:05Z').ToUniversalTime())
            [IO.File]::WriteAllText((Join-Path $version 'obsolete.txt'), 'remove only if tracked')
            [IO.File]::WriteAllText((Join-Path $version '.horizon-rust-files.json'), '["horizon-client.exe","obsolete.txt"]')
            [IO.File]::WriteAllText((Join-Path $version 'player-extra.txt'), 'keep player extra')
            [IO.File]::WriteAllText((Join-Path $version 'research-lock.json'), '{"minecraft":{"version":"26.3","client_sha1":"c","server_sha1":"s"}}')
            $prepareSources = Join-Path $version 'crates\horizon-prepare\src'
            New-Item -ItemType Directory -Force -Path $prepareSources | Out-Null
            [IO.File]::WriteAllText((Join-Path $prepareSources 'lib.rs'), 'pub const OUTPUT_VERSION: u32 = 1;')
            $resources = Join-Path $version '.local\horizon-client\26.3'
            New-Item -ItemType Directory -Force -Path $resources | Out-Null
            [IO.File]::WriteAllText((Join-Path $resources 'horizon-resources.json'), '{}')
        }
        $settingsFile = Join-Path $root 'horizon-rust.json'
        $saved = @{ name = 'Alex_9'; minecraft_dir = 'D:\fixture Minecraft'; launch = @{
            client = 'old-client'; resources = 'old-resources'; home = 'old-home'
        }} | ConvertTo-Json -Depth 3
        [IO.File]::WriteAllText($settingsFile, $saved)
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = Join-Path $root 'horizon-offline.exe'
        $start.Arguments = '--menu'
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardInput = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.Encoding]::UTF8
        $start.StandardErrorEncoding = [Text.Encoding]::UTF8
        foreach ($key in @('HORIZON_HOME', 'XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'HORIZON_LAUNCHER_TRASH', 'HORIZON_LAUNCHER_CLIPBOARD', 'HORIZON_LAUNCHER_DESKTOP')) {
            $start.EnvironmentVariables[$key] = Join-Path $root $key
        }
        $start.EnvironmentVariables['HORIZON_LAUNCHER_HOME'] = $data
        $start.EnvironmentVariables['HORIZON_FIXTURE_HOME'] = $data
        $start.EnvironmentVariables['HORIZON_FIXTURE_MODE'] = $mode
        $start.EnvironmentVariables['HORIZON_LAUNCHER_NO_SELF_UPDATE'] = '1'
$start.EnvironmentVariables['HORIZON_OFFLINE_NO_UPDATE'] = '1'
        $start.EnvironmentVariables['HORIZON_RUST_STANDALONE'] = '0'
        $start.EnvironmentVariables['HORIZON_NO_AUDIO'] = '1'
        $start.EnvironmentVariables['NO_COLOR'] = '1'
        $process = [Diagnostics.Process]::Start($start)
        try {
            $outTask = $process.StandardOutput.ReadToEndAsync()
            $errTask = $process.StandardError.ReadToEndAsync()
            $process.StandardInput.WriteLine('6')
            if ($mode -eq 'repeat') {
                $process.StandardInput.WriteLine('')
                $process.StandardInput.WriteLine('6')
            }
            $process.StandardInput.Close()
            if (-not $process.WaitForExit(15000)) { $process.Kill(); throw "$layout/$mode timed out" }
            $output = $outTask.GetAwaiter().GetResult()
            $errors = $errTask.GetAwaiter().GetResult()
            [IO.File]::WriteAllText((Join-Path $root 'menu-output.txt'), $output + $errors)
            if ($process.ExitCode) { throw "$layout/$mode menu failed: $errors" }
        } finally { $process.Dispose() }
        $calls = @(Get-ChildItem -LiteralPath $data -Filter 'calls.log' -Recurse -File | ForEach-Object { [IO.File]::ReadAllLines($_.FullName) })
        $installs = @($calls | Where-Object { $_ -like 'install *' })
        $after = [IO.File]::ReadAllText($settingsFile)
        $changed = $mode -in @('first', 'new')
        $expected = switch ($mode) {
            'current' { 'Уже обновлено.' }
            'repeat' { 'Уже обновлено.' }
            'first' { 'Клиент Horizon nightly-new установлен.' }
            'new' { 'Клиент Horizon обновлён до nightly-new.' }
            'offline' { 'offline fixture' }
            'bad-json' { 'некорректный ответ' }
            'empty' { 'нет доступной версии' }
            'unsupported' { 'нет доступной версии' }
            'install-fails' { 'Не удалось установить' }
            'unconfirmed' { 'не подтверждена' }
        }
        if (-not $output.Contains($expected)) { throw "$layout/$mode missing '$expected': $output" }
        $expectedInstalls = if ($mode -in @('first', 'new', 'install-fails', 'unconfirmed')) { 1 } else { 0 }
        if ($installs.Count -ne $expectedInstalls) { throw "$layout/$mode unexpected downloads: $calls" }
        if (-not $changed -and $after -cne $saved) { throw "$layout/$mode changed saved paths" }
        if ($changed) {
            $settings = $after | ConvertFrom-Json
            if ($null -ne $settings.launch -or $settings.name -ne 'Alex_9' -or $settings.minecraft_dir -ne 'D:\fixture Minecraft') {
                throw "$layout/$mode failed to refresh paths or changed user preferences"
            }
            if ($mode -eq 'new') {
                $installedVersion = Join-Path $data 'versions\nightly-new'
                if (([IO.File]::GetLastWriteTimeUtc((Join-Path $installedVersion 'unchanged.txt'))) -ne ([DateTime]'2020-01-02T03:04:05Z').ToUniversalTime()) { throw 'Unchanged file was rewritten.' }
                if (-not (Test-Path -LiteralPath (Join-Path $installedVersion 'player-extra.txt'))) { throw 'Player extra file disappeared.' }
                if (Test-Path -LiteralPath (Join-Path $installedVersion 'obsolete.txt')) { throw 'Tracked obsolete file was not removed.' }
                if (-not (Test-Path -LiteralPath (Join-Path $installedVersion '.local\horizon-client\26.3\horizon-resources.json'))) { throw 'Compatible resources were discarded.' }
                if (-not $output.Contains('Готовые ресурсы Minecraft сохранены')) { throw 'Resource reuse was not reported.' }
            }
        }
        if ($mode -eq 'repeat' -and ($output.Split(@('Уже обновлено.'), [StringSplitOptions]::None).Count -ne 3)) {
            throw "$layout/$mode did not report both checks"
        }
        $results += [pscustomobject]@{ layout = $layout; scenario = $mode; passed = $true; installs = $installs.Count }
        Write-Output "PASS: $layout / $mode"
    }
}
$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $scratch 'results.json') -Encoding UTF8
Write-Output "PASS: $($results.Count) isolated update-menu scenarios. Evidence: $scratch"
