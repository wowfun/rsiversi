# Independent removal launcher; Windows PowerShell 5.1 compatible.
[CmdletBinding(PositionalBinding=$false)]
param(
    [string]$Prefix,
    [string]$Uninstaller,
    [string]$ConfigHome,
    [string]$StateHome,
    [string]$CacheHome,
    [switch]$Purge,
    [switch]$DryRun,
    [switch]$NoPath,
    [switch]$Force,
    [switch]$Yes,
    [Alias('h')][switch]$Help
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Set-StrictMode -Version Latest
trap { [Console]::Error.WriteLine("rsi uninstall: " + $_.Exception.Message); exit 1 }
if ($Help) {
    Write-Output 'Usage: powershell -File uninstall.ps1 [-Prefix DIR] [-Uninstaller FILE] [-ConfigHome DIR] [-StateHome DIR] [-CacheHome DIR] [-Purge] [-DryRun] [-NoPath] [-Force] [-Yes]'
    Write-Output 'Remove an installation without running rsi or Cargo. Homes are preserved by default.'
    Write-Output '-Purge clears shared runtime data and saved credentials, preserving user-authored configuration.'
    Write-Output '-Force permits confirmed fixed-file removal without a valid record; it never force-kills a Host.'
    Write-Output 'After uninstall, later -Purge requires an external prebuilt -Uninstaller FILE.'
    exit 0
}
foreach ($name in @('Prefix', 'Uninstaller', 'ConfigHome', 'StateHome', 'CacheHome')) {
    if ($PSBoundParameters.ContainsKey($name) -and [string]::IsNullOrEmpty($PSBoundParameters[$name])) { throw "$name must not be empty." }
}
if (!$Prefix) {
    if ((Split-Path -Leaf $PSScriptRoot) -eq 'rsi' -and (Split-Path -Leaf (Split-Path -Parent $PSScriptRoot)) -eq 'libexec') {
        $Prefix = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    } else { $Prefix = Join-Path $env:LOCALAPPDATA 'RSIversi' }
}
$prefixPath = [IO.Path]::GetFullPath($Prefix)
$control = Join-Path $prefixPath 'libexec\rsi'
$external = $PSBoundParameters.ContainsKey('Uninstaller')
if (!$Uninstaller) { $Uninstaller = Join-Path $control 'rsi-uninstall.exe' }
$Uninstaller = [IO.Path]::GetFullPath($Uninstaller)
function Attributes([string]$Path) {
    try { return [IO.File]::GetAttributes($Path) }
    catch [IO.FileNotFoundException] { return $null }
    catch [IO.DirectoryNotFoundException] { return $null }
}
function File-Digest([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $hash.Dispose(); $stream.Dispose() }
}
foreach ($base in @($prefixPath, $control, (Split-Path -Parent $Uninstaller))) {
    for ($parent = $base; $parent; $parent = [IO.Path]::GetDirectoryName($parent)) {
        $value = Attributes $parent
        if ($null -ne $value -and ($value -band [IO.FileAttributes]::ReparsePoint)) { throw "Path traverses a link/junction at $parent. Pass its resolved directory explicitly." }
        if ($null -ne $value -and !($value -band [IO.FileAttributes]::Directory)) { throw "Parent is not a directory: $parent" }
    }
}
$attributes = Attributes $Uninstaller
if ($null -eq $attributes) {
    $remaining = @('bin\rsi.exe', 'libexec\rsi\rsi-uninstall.exe', 'libexec\rsi\uninstall.ps1', 'libexec\rsi\install.json', 'bin\.rsi-core-upgrade.pending.json') | Where-Object { $null -ne (Attributes (Join-Path $prefixPath $_)) }
    if (!$Purge -and !$remaining) { Write-Output "No installation files found at $prefixPath; homes were not cleaned."; exit 0 }
    throw "Uninstaller missing at $Uninstaller. Supply -Uninstaller FILE from a trusted prebuilt core package; no working rsi or Cargo is required."
}
if ($attributes -band ([IO.FileAttributes]::Directory -bor [IO.FileAttributes]::ReparsePoint)) { throw "Uninstaller is not an ordinary file: $Uninstaller" }
$digest = File-Digest $Uninstaller
if (!$external) {
    $recordPath = Join-Path $control 'install.json'
    $recordAttributes = Attributes $recordPath
    if ($null -eq $recordAttributes -or ($recordAttributes -band ([IO.FileAttributes]::Directory -bor [IO.FileAttributes]::ReparsePoint))) { throw 'No reliable tool record. Supply a trusted external -Uninstaller FILE and -Force.' }
    try {
        if ((Get-Item -LiteralPath $recordPath).Length -gt 65536) { throw 'oversized record' }
        $record = [IO.File]::ReadAllText($recordPath) | ConvertFrom-Json
        if ($record.format -ne 1 -or $record.uninstaller -ne $digest) { throw 'record mismatch' }
    } catch {
        throw 'No reliable tool record. Supply a trusted external -Uninstaller FILE and -Force.'
    }
}
$arguments = @('uninstall', '--interface-version', '1', '--prefix', $prefixPath)
foreach ($entry in @(@('ConfigHome','--config-home'), @('StateHome','--state-home'), @('CacheHome','--cache-home'))) {
    if ($PSBoundParameters.ContainsKey($entry[0])) { $arguments += @($entry[1], [IO.Path]::GetFullPath($PSBoundParameters[$entry[0]])) }
}
foreach ($entry in @(@($Purge,'--purge'), @($DryRun,'--dry-run'), @($NoPath,'--no-path'), @($Force,'--force'), @($Yes,'--yes'))) {
    if ($entry[0]) { $arguments += $entry[1] }
}
if ($DryRun) { & $Uninstaller @arguments; exit $LASTEXITCODE }
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('rsi-uninstall-' + [guid]::NewGuid().ToString('N'))
$temporary = [IO.Path]::GetFullPath($temporary)
if ($temporary.StartsWith($prefixPath.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Temporary execution directory must be outside the installation prefix.' }
New-Item -ItemType Directory -Path $temporary | Out-Null
$exitCode = 1
try {
    $policy = New-Object Security.AccessControl.DirectorySecurity
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $policy.SetOwner($identity); $policy.SetAccessRuleProtection($true, $false)
    foreach ($sid in @($identity.Value, 'S-1-5-18', 'S-1-5-32-544')) {
        $rule = New-Object Security.AccessControl.FileSystemAccessRule((New-Object Security.Principal.SecurityIdentifier($sid)), 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $policy.AddAccessRule($rule)
    }
    $directory = New-Object IO.DirectoryInfo($temporary)
    if ($PSVersionTable.PSEdition -eq 'Core') { [IO.FileSystemAclExtensions]::SetAccessControl($directory, $policy) } else { $directory.SetAccessControl($policy) }
    $copy = Join-Path $temporary 'rsi-uninstall.exe'
    Copy-Item -LiteralPath $Uninstaller -Destination $copy
    if ((File-Digest $copy) -ne $digest) { throw 'Uninstaller changed while copying.' }
    & $copy @arguments
    $exitCode = $LASTEXITCODE
} finally {
    # Only the exact private directory created above is eligible for cleanup.
    if ($temporary -and [IO.Path]::GetFullPath($temporary) -eq $temporary -and (Split-Path -Leaf $temporary) -like 'rsi-uninstall-*') {
        Remove-Item -LiteralPath $temporary -Recurse -Force
    }
}
exit $exitCode
