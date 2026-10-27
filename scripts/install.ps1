# Minimal native installer. Compatible with Windows PowerShell 5.1.
[CmdletBinding(PositionalBinding=$false)]
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'RSIversi'),
    [string]$Binary,
    [string]$Uninstaller,
    [switch]$NoPath,
    [Alias('h')][switch]$Help
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Set-StrictMode -Version Latest
if ($Help) {
    Write-Output 'Usage: powershell -File scripts/install.ps1 [-Prefix DIR] [-Binary FILE -Uninstaller FILE] [-NoPath]'
    Write-Output 'Install rsi into DIR\bin with an independent uninstaller in DIR\libexec\rsi.'
    Write-Output 'Use this script for the first installation; existing destinations are refused before building.'
    Write-Output 'Without -Binary, build the core and uninstaller from this checkout using cargo --release.'
    Write-Output 'Add interfaces afterwards with rsi plugin install web or desktop.'
    exit 0
}
foreach ($name in @('Prefix', 'Binary', 'Uninstaller')) {
    if ($PSBoundParameters.ContainsKey($name) -and [string]::IsNullOrEmpty($PSBoundParameters[$name])) {
        [Console]::Error.WriteLine("$name must not be empty.")
        exit 2
    }
}
$root = Split-Path -Parent $PSScriptRoot
function Write-InstallProgress([string]$Message) {
    [Console]::Error.WriteLine("[rsi] $Message")
}
function Get-InstallAttributes([string]$Path) {
    try { return [IO.File]::GetAttributes($Path) }
    catch [IO.FileNotFoundException] { return $null }
    catch [IO.DirectoryNotFoundException] { return $null }
}
function Assert-InstallDestination {
    $attributes = Get-InstallAttributes $destination
    if ($null -eq $attributes) { return }
    $alternative = 'Choose another empty prefix with -Prefix DIR.'
    if ($attributes -band ([IO.FileAttributes]::Directory -bor [IO.FileAttributes]::ReparsePoint)) {
        throw "Installation destination is occupied by a directory or link: $destination`n$alternative"
    }
    $quoted = $destination.Replace("'", "''")
    throw "rsi is already installed at $destination`nUpgrade this installation (PowerShell):`n  & '$quoted' core upgrade --local 'ABSOLUTE_CORE_PACKAGE'`nReplace ABSOLUTE_CORE_PACKAGE with a core package directory containing package.json, not rsi.exe.`nPackage preparation: $root\docs\user\source-install.md`n$alternative"
}
trap {
    Write-InstallProgress $_.Exception.Message
    exit 1
}
$prefixPath = [IO.Path]::GetFullPath($Prefix)
$binPath = Join-Path $prefixPath 'bin'
$destination = Join-Path $binPath 'rsi.exe'
$controlPath = Join-Path $prefixPath 'libexec\rsi'
$recordPath = Join-Path $controlPath 'install.json'
if ([IO.File]::Exists($recordPath) -and (Get-Item -LiteralPath $recordPath).Length -le 65536) {
    $recordText = [IO.File]::ReadAllText($recordPath)
    if ($recordText -match '"phase"\s*:\s*"(uninstalling|installing)"') {
        throw "Incomplete installation maintenance at $recordPath. Finish uninstall before reinstalling."
    }
}
Assert-InstallDestination
foreach ($name in @('rsi-uninstall.exe', 'uninstall.ps1', 'install.json')) {
    $path = Join-Path $controlPath $name
    if ($null -ne (Get-InstallAttributes $path)) { throw "Installation residue at $path. Finish uninstall before reinstalling, or choose another -Prefix DIR." }
}
if ($Binary -and !$Uninstaller) { throw '-Binary requires -Uninstaller FILE.' }
if (!$Binary -and $Uninstaller) { throw '-Uninstaller requires -Binary FILE.' }
foreach ($base in @($binPath, $controlPath)) {
for ($parent = $base; $parent; $parent = [IO.Path]::GetDirectoryName($parent)) {
    $attributes = Get-InstallAttributes $parent
    if ($null -ne $attributes -and ($attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Installation path traverses a link/junction at $parent. Pass its resolved directory with -Prefix DIR."
    }
    if ($null -ne $attributes -and ![IO.Directory]::Exists($parent)) {
        throw "Installation parent is not an accessible directory: $parent`nChoose another empty prefix with -Prefix DIR."
    }
}
}
Write-InstallProgress "Preparing installation in $binPath..."
if (!$Binary) {
    Push-Location -LiteralPath $root
    try {
        Write-InstallProgress 'Preparing release build...'
        $cargo = (Get-Command cargo -CommandType Application | Select-Object -First 1).Source
        # MSYS can supply case aliases that .NET Framework's environment dictionary rejects.
        $inherited = [Environment]::GetEnvironmentVariables()
        $aliases = @{}
        foreach ($name in $inherited.Keys) {
            if (!$aliases.ContainsKey($name)) { $aliases[$name] = @() }
            $aliases[$name] += $name
        }
        foreach ($names in $aliases.Values) {
            if ($names.Count -lt 2) { continue }
            $value = [Environment]::GetEnvironmentVariable($names[0])
            foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $null) }
            [Environment]::SetEnvironmentVariable($names[0], $value)
        }
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = $cargo
        $start.Arguments = 'build --locked --release -p rsi-cli -p rsi-uninstall'
        $start.WorkingDirectory = $root
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $false
        $start.EnvironmentVariables.Clear()
        $start.EnvironmentVariables['CARGO_HOME'] = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
        $start.EnvironmentVariables['RUSTUP_HOME'] = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
        foreach ($declaration in Get-Content -LiteralPath (Join-Path $root 'apps\devtools\build-environment.txt')) {
            if ($declaration -cnotmatch '^([A-Za-z_][A-Za-z0-9_()]*)(=([a-z0-9_-]+(/[a-z0-9_-]+)*))?$') { throw 'Invalid build environment declaration.' }
            $key = $Matches[1]
            if ($Matches.ContainsKey(3)) {
                $value = Join-Path $root ('target\rsi-build\' + $Matches[3])
                New-Item -ItemType Directory -Force -Path $value | Out-Null
                $start.EnvironmentVariables[$key] = $value
            } else {
                $value = [Environment]::GetEnvironmentVariable($key)
                if ($null -ne $value) { $start.EnvironmentVariables[$key] = $value }
            }
        }
        $start.EnvironmentVariables['CI'] = 'true'
        Write-InstallProgress 'Building rsi (release); the first build may take several minutes...'
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $build = [Diagnostics.Process]::Start($start)
        try {
            while (!$build.WaitForExit(15000)) {
                Write-InstallProgress ('Still building rsi ({0}s elapsed)...' -f [math]::Floor($timer.Elapsed.TotalSeconds))
            }
            if ($build.ExitCode -ne 0) { throw 'Core build failed; nothing was installed.' }
            Write-InstallProgress ('Build completed in {0}s.' -f [math]::Floor($timer.Elapsed.TotalSeconds))
        } finally {
            $timer.Stop()
            if (!$build.HasExited) {
                & "$env:SystemRoot\System32\taskkill.exe" /PID $build.Id /T /F | Out-Null
                $build.WaitForExit()
            }
            $build.Dispose()
        }
        $target = if (!$env:CARGO_TARGET_DIR) { Join-Path $root 'target' }
            elseif ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) }
            else { [IO.Path]::GetFullPath((Join-Path $root $env:CARGO_TARGET_DIR)) }
        $Binary = Join-Path $target 'release\rsi.exe'
        $Uninstaller = Join-Path $target 'release\rsi-uninstall.exe'
    } finally { Pop-Location }
} else {
    Write-InstallProgress "Using existing binary: $Binary"
}
$arguments = @('install', '--interface-version', '1', '--prefix', $prefixPath, '--binary', [IO.Path]::GetFullPath($Binary), '--uninstaller', [IO.Path]::GetFullPath($Uninstaller), '--launcher', (Join-Path $PSScriptRoot 'uninstall.ps1'))
if ($NoPath) { $arguments += '--no-path'; Write-InstallProgress 'Skipping PATH update (--no-path).' }
& ([IO.Path]::GetFullPath($Uninstaller)) @arguments
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output 'Restart your terminal application and run rsi. Install optional packages with rsi plugin install.'
