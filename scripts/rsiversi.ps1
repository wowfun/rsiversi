# Minimal native installer. Compatible with Windows PowerShell 5.1.
[CmdletBinding(PositionalBinding=$false)]
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'RSIversi'),
    [string]$Binary,
    [switch]$NoPath,
    [Alias('h')][switch]$Help
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Help) {
    Write-Output 'Usage: powershell -File scripts/rsiversi.ps1 [-Prefix DIR] [-Binary FILE] [-NoPath]'
    Write-Output 'Install only rsi into DIR\bin and add it to the user PATH.'
    Write-Output 'Without -Binary, build the core from this checkout using cargo --release.'
    Write-Output 'Add interfaces afterwards with rsi plugin install web or desktop.'
    exit 0
}
$root = Split-Path -Parent $PSScriptRoot
function File-Digest([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hash.ComputeHash($stream)) }
    finally { $hash.Dispose(); $stream.Dispose() }
}
$prefixPath = [IO.Path]::GetFullPath($Prefix)
$binPath = Join-Path $prefixPath 'bin'
if (!$Binary) {
    Push-Location $root
    try {
        $cargo = (Get-Command cargo -CommandType Application).Source
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = $cargo
        $start.Arguments = 'build --locked --release -p rsi-cli'
        $start.WorkingDirectory = $root
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
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
        foreach ($key in @('CARGO_TARGET_DIR', 'RUSTUP_TOOLCHAIN')) {
            $value = [Environment]::GetEnvironmentVariable($key)
            if ($null -ne $value) { $start.EnvironmentVariables[$key] = $value }
        }
        $start.EnvironmentVariables['CI'] = 'true'
        $build = [Diagnostics.Process]::Start($start)
        $build.WaitForExit()
        try { if ($build.ExitCode -ne 0) { throw 'Core build failed; nothing was installed.' } }
        finally { $build.Dispose() }
        $target = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $root 'target' }
        $Binary = Join-Path $target 'release\rsi.exe'
    } finally { Pop-Location }
}
$source = Get-Item -LiteralPath $Binary
if ($source.PSIsContainer -or ($source.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'Binary must be an ordinary executable file.'
}
if (!(Test-Path -LiteralPath $binPath)) {
    New-Item -ItemType Directory -Path $binPath | Out-Null
    $policy = New-Object Security.AccessControl.DirectorySecurity
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $policy.SetOwner($identity)
    $policy.SetAccessRuleProtection($true, $false)
    foreach ($sid in @($identity.Value, 'S-1-5-18', 'S-1-5-32-544')) {
        $principal = New-Object Security.Principal.SecurityIdentifier($sid)
        $rule = New-Object Security.AccessControl.FileSystemAccessRule($principal, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $policy.AddAccessRule($rule)
    }
    $directoryInfo = New-Object IO.DirectoryInfo($binPath)
    if ($PSVersionTable.PSEdition -eq 'Core') {
        [IO.FileSystemAclExtensions]::SetAccessControl($directoryInfo, $policy)
    } else {
        $directoryInfo.SetAccessControl($policy)
    }
}
$destination = Join-Path $binPath 'rsi.exe'
if (Test-Path -LiteralPath $destination) {
    throw 'rsi is already installed here. Use rsi core upgrade --local PACKAGE, or choose another prefix.'
}
$staged = Join-Path $binPath ('.rsi-install-' + [guid]::NewGuid().ToString('N') + '.exe')
try {
    Copy-Item -LiteralPath $source.FullName -Destination $staged
    if ((File-Digest $source.FullName) -ne (File-Digest $staged)) {
        throw 'Binary changed while copying.'
    }
    & $staged --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed executable failed its version check.' }
    [IO.File]::Move($staged, $destination)
} finally {
    if (Test-Path -LiteralPath $staged) { Remove-Item -LiteralPath $staged }
}
if (!$NoPath) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($userPath -split ';' | Where-Object { $_ })
    if ($entries -notcontains $binPath) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $binPath) -join ';'), 'User')
    }
    if (($env:Path -split ';') -notcontains $binPath) { $env:Path = "$binPath;$env:Path" }
}
Write-Output "Installed $destination"
Write-Output 'Open a new terminal and run rsi. Install optional packages with rsi plugin install.'
