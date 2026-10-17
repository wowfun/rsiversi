# ASCII-only bootstrap for Windows PowerShell 5.1 and PowerShell 7.
$ErrorActionPreference = 'Stop'
$script:Root = $PSScriptRoot
$script:Forward = @($args)
function Quote-Native([string]$Value) {
    # Microsoft CRT argv quoting, including empty values and trailing slashes.
    return '"' + [regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
function Start-Native([string]$Program, [string[]]$Arguments, [bool]$PreserveExit = $false) {
    $command = Get-Command $Program -CommandType Application -ErrorAction Stop | Select-Object -First 1
    if ([IO.Path]::GetExtension($command.Source) -in @('.cmd', '.bat')) { throw "Native executable required: $Program" }
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $command.Source
    $start.Arguments = ($Arguments | ForEach-Object { Quote-Native $_ }) -join ' '
    $start.WorkingDirectory = $script:Root
    $start.UseShellExecute = $false
    $child = [Diagnostics.Process]::Start($start)
    try { $child.WaitForExit(); $code = $child.ExitCode } finally { $child.Dispose() }
    if ($PreserveExit) { exit $code }
    if ($code -ne 0) { throw "$Program exited with code $code" }
}
function Read-Versions {
    $rust = [regex]::Match([IO.File]::ReadAllText((Join-Path $script:Root 'rust-toolchain.toml')), '(?m)^\s*channel\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"').Groups[1].Value
    $package = [IO.File]::ReadAllText((Join-Path $script:Root 'apps/web/package.json')) | ConvertFrom-Json
    $bindgen = [regex]::Match([IO.File]::ReadAllText((Join-Path $script:Root 'Cargo.lock')), '(?s)\[\[package\]\]\s*name = "wasm-bindgen"\s*version = "([0-9.]+)"').Groups[1].Value
    if (!$rust -or !$bindgen -or $package.packageManager -notmatch '^pnpm@([0-9.]+)$') { throw 'Invalid repository tool version declarations' }
    return @{ Rust=$rust; Bindgen=$bindgen; Node=$package.engines.node }
}
function Test-Prerequisites([bool]$Full) {
    $problems = New-Object 'Collections.Generic.List[string]'
    if ([Environment]::OSVersion.Version.Build -lt 26100 -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { $problems.Add('Windows 11 24H2+ x64 is required.') }
    foreach ($tool in @('git.exe', 'rustup.exe', 'node.exe')) {
        if (!(Get-Command $tool -CommandType Application -ErrorAction SilentlyContinue)) { $problems.Add("Missing $tool. Install Git for Windows, Rustup, and Node as described in the source-install guide.") }
    }
    if (Get-Command git.exe -ErrorAction SilentlyContinue) {
        # PowerShell 5.1 promotes redirected native stderr to NativeCommandError.
        # Diagnose Git's exit status without losing the other prerequisite errors.
        $previousPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            $null = & git.exe -C $script:Root rev-parse --show-toplevel 2>$null
            $checkoutStatus = $LASTEXITCODE
            $gitLongPaths = & git.exe -C $script:Root config --get core.longpaths 2>$null
        } finally { $ErrorActionPreference = $previousPreference }
        if ($checkoutStatus -ne 0) { $problems.Add('A Git clone is required; source ZIP archives are unsupported.') }
        if ($gitLongPaths -ne 'true') { [Console]::Error.WriteLine('Git long paths are not enabled for this checkout. For deep paths, run: git config core.longpaths true') }
    }
    $drive = New-Object IO.DriveInfo ([IO.Path]::GetPathRoot($script:Root))
    if ($drive.DriveType -ne 'Fixed' -or $drive.DriveFormat -ne 'NTFS') { $problems.Add('Use a local NTFS checkout; network shares and other filesystems are unsupported.') }
    $vswhere = $null
    if (${env:ProgramFiles(x86)}) { $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe' }
    if (!$vswhere -or !(Test-Path -LiteralPath $vswhere) -or !(& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)) { $problems.Add('Install Visual Studio Build Tools: Desktop development with C++, MSVC x64 and Windows SDK.') }
    $sdkRoot = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -Name KitsRoot10 -ErrorAction SilentlyContinue).KitsRoot10
    if (!$sdkRoot) { $sdkRoot = (Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots' -Name KitsRoot10 -ErrorAction SilentlyContinue).KitsRoot10 }
    $sdkReady = $false
    if ($sdkRoot -and (Test-Path -LiteralPath (Join-Path $sdkRoot 'Lib'))) {
        foreach ($version in (Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'Lib') -Directory)) {
            if ((Test-Path -LiteralPath (Join-Path $version.FullName 'um/x64/kernel32.lib')) -and (Test-Path -LiteralPath (Join-Path $version.FullName 'ucrt/x64/ucrt.lib'))) { $sdkReady = $true }
        }
    }
    if (!$sdkReady) { $problems.Add('Windows SDK x64 libraries missing. Add Windows SDK in Visual Studio Installer.') }
    if (Get-Command node.exe -ErrorAction SilentlyContinue) {
        $version = & node.exe -p 'process.versions.node'
        $minimum = (Read-Versions).Node -replace '^>=', ''
        if ([version]$version -lt [version]$minimum) { $problems.Add("Node $minimum or newer is required; found $version.") }
    }
    $longPaths = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -ErrorAction SilentlyContinue).LongPathsEnabled
    if ($longPaths -ne 1 -or $script:Root.Length -gt 75) { [Console]::Error.WriteLine('Path guidance: use a short checkout path; nested build dependencies may exceed Windows path limits. No registry settings were changed.') }
    if ($Full) {
        $versions = Read-Versions
        if (Get-Command rustup.exe -ErrorAction SilentlyContinue) {
            $installed = & rustup.exe toolchain list
            if (!($installed -match ([regex]::Escape($versions.Rust)))) { $problems.Add('Pinned Rust toolchain missing; run setup.') }
            else {
                $targets = & rustup.exe target list --installed --toolchain $versions.Rust
                if ($targets -notcontains 'wasm32-unknown-unknown') { $problems.Add('WASM Rust target missing; run setup.') }
            }
        }
        if (!(Test-Path -LiteralPath (Join-Path $script:Root 'target/rsi-tools/pnpm/node_modules/pnpm/bin/pnpm.mjs'))) { $problems.Add('Repository pnpm missing; run setup.') }
        if (!(Test-Path -LiteralPath (Join-Path $script:Root 'target/rsi-tools/wasm-bindgen/bin/wasm-bindgen.exe'))) { $problems.Add('Repository wasm-bindgen missing; run setup.') }
    }
    foreach ($problem in $problems) { [Console]::Error.WriteLine($problem) }
    return $problems.Count -eq 0
}
function Current-Bundle {
    $record = Join-Path $script:Root 'target/rsi-app/current'
    if (!(Test-Path -LiteralPath $record -PathType Leaf)) { throw 'No published application. Run setup, then build.' }
    if ((Get-Item -LiteralPath $record -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Current publication record must be an ordinary file' }
    $stream = [IO.File]::OpenRead($record)
    try {
        # Application publication grammar: apps/devtools/README.md.
        $bytes = New-Object byte[] 22
        $count = 0
        while ($count -lt $bytes.Length) {
            $read = $stream.Read($bytes, $count, $bytes.Length - $count)
            if (!$read) { break }
            $count += $read
        }
        $id = [Text.Encoding]::ASCII.GetString($bytes, 0, $count)
    } finally { $stream.Dispose() }
    if ($id -cnotmatch '\A[0-9]{20}\n\z') { throw 'Invalid current publication identifier' }
    $bundle = Join-Path $script:Root ('target/rsi-app/generations/' + $id.TrimEnd("`n") + '/bundle')
    foreach ($name in @('rsi.exe','receipt.json','.rsi-generation.lock')) {
        if (!(Test-Path -LiteralPath (Join-Path $bundle $name) -PathType Leaf)) { throw "Incomplete publication ($name). Run build." }
    }
    return $bundle
}
try {
    if (!$script:Forward.Count) { throw 'Usage: rsi.ps1 doctor|setup|build [--debug]|web [ARGS]|tui [ARGS]|path' }
    $operation = [string]$script:Forward[0]
    $rest = @(); if ($script:Forward.Count -gt 1) { $rest = @($script:Forward[1..($script:Forward.Count - 1)]) }
    if ($operation -in @('doctor','setup','path') -and $rest.Count) { throw "$operation takes no arguments" }
    switch -CaseSensitive ($operation) {
        'doctor' { if (!(Test-Prerequisites $true)) { exit 1 }; [Console]::WriteLine('Source prerequisites are ready.'); exit 0 }
        'setup' {
            if (!(Test-Prerequisites $false)) { exit 1 }
            $versions = Read-Versions
            Start-Native 'rustup.exe' @('toolchain','install',$versions.Rust,'--profile','minimal')
            Start-Native 'rustup.exe' @('target','add','wasm32-unknown-unknown','--toolchain',$versions.Rust)
            $node = (Get-Command node.exe -CommandType Application | Select-Object -First 1).Source
            $npmCommand = Get-Command npm.cmd -CommandType Application -ErrorAction Stop | Select-Object -First 1
            $npm = Join-Path ([IO.Path]::GetDirectoryName($npmCommand.Source)) 'node_modules/npm/bin/npm-cli.js'
            if (!(Test-Path -LiteralPath $npm)) { throw 'The Node installation must include npm.' }
            $tools = Join-Path $script:Root 'target/rsi-tools'
            $pnpmRoot = Join-Path $tools 'pnpm'
            [IO.Directory]::CreateDirectory($pnpmRoot) | Out-Null
            foreach ($name in @('package.json','package-lock.json')) { Copy-Item -LiteralPath (Join-Path $script:Root ('apps/devtools/tools/' + $name)) -Destination (Join-Path $pnpmRoot $name) -Force }
            Start-Native $node @($npm,'ci','--prefix',$pnpmRoot,'--ignore-scripts','--no-audit','--no-fund')
            $bindgenRoot = Join-Path $tools 'wasm-bindgen'
            $bindgen = Join-Path $bindgenRoot 'bin/wasm-bindgen.exe'
            if (!(Test-Path -LiteralPath $bindgen) -or (& $bindgen --version) -ne ('wasm-bindgen ' + $versions.Bindgen)) {
                Start-Native 'cargo.exe' @('install','--locked','--root',$bindgenRoot,'--version',$versions.Bindgen,'wasm-bindgen-cli')
            }
            [Console]::WriteLine('Setup complete. Next: rsi.ps1 build')
        }
        'build' {
            if (!(Test-Prerequisites $true)) { exit 1 }
            Start-Native 'cargo.exe' @('build','--locked','-p','rsi-app-tools','--target-dir',(Join-Path $script:Root 'target'))
            # Run a private copy so another build can relink the shared tool on Windows.
            $runners = Join-Path $script:Root 'target/rsi-tools/runners'
            $runner = Join-Path $runners ([Guid]::NewGuid().ToString('N'))
            [IO.Directory]::CreateDirectory($runner) | Out-Null
            $program = Join-Path $runner 'rsi-app-tools.exe'
            Copy-Item -LiteralPath (Join-Path $script:Root 'target/debug/rsi-app-tools.exe') -Destination $program
            try { Start-Native $program (@('dist','web') + $rest) }
            finally {
                if ([IO.Path]::GetFullPath($runner).StartsWith([IO.Path]::GetFullPath($runners) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
                    try { Remove-Item -LiteralPath $runner -Recurse -Force }
                    catch { [Console]::Error.WriteLine("Could not remove private build runner: " + $_.Exception.Message) }
                }
            }
        }
        'path' { Write-Output (Current-Bundle) }
        'web' { $bundle = Current-Bundle; Start-Native (Join-Path $bundle 'rsi.exe') (@('web') + $rest) $true }
        'tui' {
            if (!$env:WT_SESSION) { throw 'Open PowerShell or Git Bash inside Windows Terminal to use the TUI. mintty and classic conhost are unsupported.' }
            $bundle = Current-Bundle; Start-Native (Join-Path $bundle 'rsi.exe') (@('tui') + $rest) $true
        }
        default { throw "Unknown command: $operation" }
    }
} catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }
