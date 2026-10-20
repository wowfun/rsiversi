# ASCII-only bootstrap for Windows PowerShell 5.1 and PowerShell 7.
$ErrorActionPreference = 'Stop'
$script:Root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$script:Forward = @($args)
function Show-Help([string]$Operation) {
    $lines = switch -CaseSensitive ($Operation) {
        'doctor' {
            "Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 $Operation [tui|web|desktop] [--with-ssh-helper] [-h|--help]"
            'Default target: tui (Rust/native compiler only; no Node, WASM or musl).'
            'doctor checks prerequisites. setup prepares pinned Rust and target-specific local tools.'
            'Web/Desktop add Node, pnpm and wasm-bindgen; Linux Desktop also needs GTK/WebKit development libraries.'
            'Linux Web/Desktop include the musl SSH helper; TUI opts in with --with-ssh-helper.'
            'Install system prerequisites separately; setup does not change machine security settings.'
        }
        'setup' {
            "Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 $Operation [tui|web|desktop] [--with-ssh-helper] [-h|--help]"
            'Default target: tui (Rust/native compiler only; no Node, WASM or musl).'
            'doctor checks prerequisites. setup prepares pinned Rust and target-specific local tools.'
            'Web/Desktop add Node, pnpm and wasm-bindgen; Linux Desktop also needs GTK/WebKit development libraries.'
            'Linux Web/Desktop include the musl SSH helper; TUI opts in with --with-ssh-helper.'
            'Install system prerequisites separately; setup does not change machine security settings.'
        }
        'build' {
            'Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 build [tui|web|desktop] [ABSOLUTE_OUTPUT] [--debug] [--with-ssh-helper] [--verbose] [-h|--help]'
            'Default target: tui. Default profile: release; --debug selects debug.'
            'TUI includes native CLI, Headless, ACP, HTTP API and management. Web adds assets/Worker; Desktop adds the window application.'
            'Linux Web/Desktop include the SSH helper (always release); TUI opts in with --with-ssh-helper. Windows rejects this flag.'
            'Without ABSOLUTE_OUTPUT, publish under target/rsi-app/TARGET; each target has a separate build cache.'
            'ABSOLUTE_OUTPUT must be a new absolute directory outside target/rsi-app.'
            'Windows: use a drive-rooted or UNC path; forward and backward slashes are accepted.'
            'Progress and diagnostics go to stderr; stdout contains the result JSON. --verbose mirrors the full build log.'
            'Example: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 build desktop --debug'
        }
        'dev' {
            'Usage: scripts/rsiversi.ps1 dev tui [-- TUI_OPTIONS] | dev web [--port PORT] [--no-open] [--no-watch]'
            'Build an external debug product using ordinary state and credentials; explicit XDG overrides are respected.'
            'Web keeps React HMR and renderer publication. Rust/Worker edits require restart.'
            'Use fixture-dev on Linux/WSL for isolated keyless layout work and smoke checks.'
        }
        'path' {
            'Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 path [tui|web|desktop] [-h|--help]'
            'Print the selected current managed bundle directory (default: tui).'
            'Requires a successful managed build; never invokes build tools or falls back to another target.'
        }
        default {
            'RSIversi source entrypoint'
            'Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 COMMAND [ARGS]'
            ''
            'Commands:'
            '  doctor [TARGET]         Check target-specific source-build prerequisites.'
            '  setup [TARGET]          Prepare target-specific pinned tools.'
            '  build [TARGET] [OUTPUT] Build the selected application (release by default; --debug for debug).'
            '  dev tui|web [OPTIONS]   Build and supervise the standard debug product.'
            '  path [TARGET]           Print its current managed bundle directory.'
            '  tui [--from TARGET] [ARGS] Start TUI; TARGET may be tui, web or desktop.'
            '  web [--from TARGET] [ARGS] Start Web; TARGET may be web or desktop.'
            '  desktop [ARGS]          Start the Desktop application.'
            ''
            'TARGET defaults to tui for management commands. Launch defaults to its own target.'
            'Use --from web/desktop to run clients from the same build family and share a Linux Service Host.'
            'Only a leading --from is consumed; an optional -- ends script option parsing.'
            'OUTPUT is a new absolute directory outside target/rsi-app; omit it for a managed build.'
            'Use COMMAND --help for management help. web/tui/desktop help is forwarded to the application.'
            'Launch and path never build or select another publication automatically.'
            'Supports Windows PowerShell 5.1 and PowerShell 7 on Windows 11 24H2+ x64.'
            'Windows TUI requires Windows Terminal; Desktop requires WebView2 at launch.'
            'The repository is located from this script, not the current working directory.'
            ''
            'Example: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 setup desktop'
            '         powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 build desktop --verbose'
            '         powershell -NoProfile -ExecutionPolicy Bypass -File scripts/rsiversi.ps1 desktop'
            'Guide: docs/user/source-install.md'
        }
    }
    foreach ($line in $lines) { [Console]::WriteLine($line) }
}
function Quote-Native([string]$Value) {
    # Microsoft CRT argv quoting, including empty values and trailing slashes.
    return '"' + [regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
function Start-Native([string]$Program, [string[]]$Arguments, [bool]$PreserveExit = $false, [hashtable]$Environment = $null) {
    $command = Get-Command $Program -CommandType Application -ErrorAction Stop | Select-Object -First 1
    if ([IO.Path]::GetExtension($command.Source) -in @('.cmd', '.bat')) { throw "Native executable required: $Program" }
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $command.Source
    $start.Arguments = ($Arguments | ForEach-Object { Quote-Native $_ }) -join ' '
    $start.WorkingDirectory = if ($PreserveExit) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath('.') } else { $script:Root }
    $start.UseShellExecute = $false
    if ($null -ne $Environment) {
        $start.EnvironmentVariables.Clear()
        foreach ($key in $Environment.Keys) { $start.EnvironmentVariables[$key] = $Environment[$key] }
    }
    $child = [Diagnostics.Process]::Start($start)
    try { $child.WaitForExit(); $code = $child.ExitCode } finally { $child.Dispose() }
    if ($PreserveExit) { exit $code }
    if ($code -ne 0) { throw "$Program exited with code $code" }
}
function Development-BuildEnvironment {
    $environment = @{}
    $cache = Join-Path $script:Root 'target/rsi-app'
    foreach ($declaration in [IO.File]::ReadAllLines((Join-Path $script:Root 'apps/devtools/build-environment.txt'))) {
        if ($declaration -cnotmatch '^(?<key>[A-Za-z_][A-Za-z0-9_()]*)(?:=(?<path>[a-z0-9_-]+(?:/[a-z0-9_-]+)*))?$') { throw 'Invalid build environment declaration' }
        $key = $Matches['key']; $suffix = $Matches['path']
        if ($suffix) {
            $value = Join-Path $cache $suffix
            [IO.Directory]::CreateDirectory($value) | Out-Null
            $environment[$key] = $value
        } else {
            $value = [Environment]::GetEnvironmentVariable($key)
            if ($null -ne $value) { $environment[$key] = $value }
        }
    }
    if (!$environment.ContainsKey('CARGO_HOME')) { $environment['CARGO_HOME'] = Join-Path $env:USERPROFILE '.cargo' }
    if (!$environment.ContainsKey('RUSTUP_HOME')) { $environment['RUSTUP_HOME'] = Join-Path $env:USERPROFILE '.rustup' }
    $environment['CI'] = 'true'
    return $environment
}
function Read-Versions {
    $rust = [regex]::Match([IO.File]::ReadAllText((Join-Path $script:Root 'rust-toolchain.toml')), '(?m)^\s*channel\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"').Groups[1].Value
    if (!$rust) { throw 'Invalid repository Rust toolchain version declaration' }
    if ($script:Target -eq 'tui') { return @{ Rust=$rust } }
    $package = [IO.File]::ReadAllText((Join-Path $script:Root 'apps/web/package.json')) | ConvertFrom-Json
    $bindgen = [regex]::Match([IO.File]::ReadAllText((Join-Path $script:Root 'Cargo.lock')), '(?s)\[\[package\]\]\s*name = "wasm-bindgen"\s*version = "([0-9.]+)"').Groups[1].Value
    if (!$rust -or !$bindgen -or $package.packageManager -notmatch '^pnpm@([0-9.]+)$') { throw 'Invalid repository tool version declarations' }
    return @{ Rust=$rust; Bindgen=$bindgen; Node=$package.engines.node }
}
function Test-Prerequisites([bool]$Full) {
    $problems = New-Object 'Collections.Generic.List[string]'
    $versions = Read-Versions
    $setup = "scripts/rsiversi.ps1 setup $script:Target"
    if ([Environment]::OSVersion.Version.Build -lt 26100 -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { $problems.Add('Windows 11 24H2+ x64 is required.') }
    $required = @('git.exe', 'rustup.exe'); if ($script:Target -ne 'tui') { $required += @('node.exe', 'npm.cmd') }
    foreach ($tool in $required) {
        if (!(Get-Command $tool -CommandType Application -ErrorAction SilentlyContinue)) { $problems.Add("Missing $tool. See target-specific prerequisites in the source-install guide.") }
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
    if ($script:Target -ne 'tui' -and (Get-Command node.exe -ErrorAction SilentlyContinue)) {
        $version = & node.exe -p 'process.versions.node'
        $minimum = $versions.Node -replace '^>=', ''
        if ([version]$version -lt [version]$minimum) { $problems.Add("Node $minimum or newer is required; found $version.") }
    }
    $longPaths = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -ErrorAction SilentlyContinue).LongPathsEnabled
    if ($longPaths -ne 1 -or $script:Root.Length -gt 75) { [Console]::Error.WriteLine('Path guidance: use a short checkout path; nested build dependencies may exceed Windows path limits. No registry settings were changed.') }
    if ($Full) {
        if (Get-Command rustup.exe -ErrorAction SilentlyContinue) {
            $installed = & rustup.exe toolchain list
            if (!($installed -match ('^' + [regex]::Escape($versions.Rust) + '(?:-x86_64-pc-windows-msvc)?(?:\s+\([^)]+\))?$'))) { $problems.Add("Pinned Rust toolchain missing; run $setup.") }
            elseif ($script:Target -ne 'tui') {
                $targets = & rustup.exe target list --installed --toolchain $versions.Rust
                if ($targets -notcontains 'wasm32-unknown-unknown') { $problems.Add("WASM Rust target missing; run $setup.") }
            }
        }
        if ($script:Target -ne 'tui') {
        if (!(Test-Path -LiteralPath (Join-Path $script:Root 'target/rsi-tools/pnpm/node_modules/pnpm/bin/pnpm.mjs'))) { $problems.Add("Repository pnpm missing; run $setup.") }
        if (!(Test-Path -LiteralPath (Join-Path $script:Root 'target/rsi-tools/wasm-bindgen/bin/wasm-bindgen.exe'))) { $problems.Add("Repository wasm-bindgen missing; run $setup.") }
    }
    }
    foreach ($problem in $problems) { [Console]::Error.WriteLine($problem) }
    return $problems.Count -eq 0
}
function Current-Bundle([string]$Target) {
    $record = Join-Path $script:Root ('target/rsi-app/' + $Target + '/current')
    if (!(Test-Path -LiteralPath $record -PathType Leaf)) { throw "No $Target publication. Run scripts/rsiversi.ps1 setup $Target, then build $Target." }
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
    $bundle = Join-Path $script:Root ('target/rsi-app/' + $Target + '/generations/' + $id.TrimEnd("`n") + '/bundle')
    $names = @('rsi.exe','receipt.json','.rsi-generation.lock'); if ($Target -eq 'desktop') { $names += 'rsi-desktop.exe' }
    foreach ($name in $names) {
        if (!(Test-Path -LiteralPath (Join-Path $bundle $name) -PathType Leaf)) { throw "Incomplete $Target publication ($name). Run scripts/rsiversi.ps1 build $Target." }
    }
    return $bundle
}
try {
    if (!$script:Forward.Count -or $script:Forward[0] -cin @('--help','-h')) { Show-Help ''; exit 0 }
    $operation = [string]$script:Forward[0]
    $rest = @(); if ($script:Forward.Count -gt 1) { $rest = @($script:Forward[1..($script:Forward.Count - 1)]) }
    if ($operation -cin @('doctor','setup','build','path','dev') -and ($rest -ccontains '--help' -or $rest -ccontains '-h')) { Show-Help $operation; exit 0 }
    $script:Target = 'tui'
    if ($operation -cin @('doctor','setup','build','path')) {
        if ($rest.Count -and $rest[0] -cin @('tui','web','desktop')) { $script:Target = $rest[0]; $rest = @($rest | Select-Object -Skip 1) }
        $seen = @{}; $output = $null
        foreach ($argument in $rest) {
            if ($argument -ceq '--with-ssh-helper') { throw 'SSH helper publication requires Linux; omit --with-ssh-helper on Windows' }
            if ($operation -ceq 'build' -and $argument -cin @('--debug','--verbose') -and !$seen.ContainsKey($argument)) { $seen[$argument]=$true }
            elseif ($operation -ceq 'build' -and !$output -and $argument -cmatch '^(?:[A-Za-z]:[/\\]|[/\\]{2})') { $output=$argument }
            else { throw "Invalid or duplicate argument: $argument. Use $operation --help." }
        }
    }
    if ($operation -cin @('tui','web','desktop')) {
        $script:Target = $operation
        if ($operation -cne 'desktop' -and $rest.Count -and $rest[0] -ceq '--from') {
            if ($rest.Count -lt 2 -or $rest[1] -cnotin @('tui','web','desktop')) { throw '--from requires tui, web or desktop' }
            $script:Target = $rest[1]; $rest = @($rest | Select-Object -Skip 2)
            if ($operation -ceq 'web' -and $script:Target -ceq 'tui') { throw 'Web requires --from web or --from desktop' }
        }
        if ($rest.Count -and $rest[0] -ceq '--') { $rest = @($rest | Select-Object -Skip 1) }
    }
    switch -CaseSensitive ($operation) {
        'dev' {
            if (!$rest.Count -or $rest[0] -cnotin @('tui','web')) { throw 'dev requires tui or web' }
            $debugger = [runspace]::DefaultRunspace.Debugger
            $debugMode = $debugger.DebugMode
            try {
                # Console break belongs to native supervision, not the script debugger.
                $debugger.SetDebugMode([System.Management.Automation.DebugModes]::None)
                $script:Target = $rest[0]
                $caller = if ($env:RSI_DEV_CALLER_CWD) { $env:RSI_DEV_CALLER_CWD } else { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath('.') }
                Start-Native 'cargo.exe' @('build','--locked','-p','rsi-app-tools','--target-dir',(Join-Path $script:Root 'target')) $false (Development-BuildEnvironment)
                $runners = [IO.Path]::GetFullPath((Join-Path $script:Root 'target/rsi-tools/runners'))
                $runner = Join-Path $runners ([Guid]::NewGuid().ToString('N'))
                [IO.Directory]::CreateDirectory($runner) | Out-Null
                $program = Join-Path $runner 'rsi-app-tools.exe'
                Copy-Item -LiteralPath (Join-Path $script:Root 'target/debug/rsi-app-tools.exe') -Destination $program
                $oldCaller = $env:RSI_DEV_CALLER_CWD
                try {
                    $env:RSI_DEV_CALLER_CWD = $caller
                    Start-Native $program (@('dev') + $rest)
                } finally {
                    $env:RSI_DEV_CALLER_CWD = $oldCaller
                    $resolvedRunner = [IO.Path]::GetFullPath($runner)
                    if ([IO.Path]::GetDirectoryName($resolvedRunner).Equals($runners, [StringComparison]::OrdinalIgnoreCase)) {
                        try {
                            Remove-Item -LiteralPath $program -Force
                            Remove-Item -LiteralPath $resolvedRunner
                        } catch { [Console]::Error.WriteLine('Private development runner retained: ' + $resolvedRunner) }
                    }
                }
            } finally { $debugger.SetDebugMode($debugMode) }
        }
        'doctor' { if (!(Test-Prerequisites $true)) { exit 1 }; [Console]::WriteLine('Source prerequisites are ready.'); exit 0 }
        'setup' {
            if (!(Test-Prerequisites $false)) { exit 1 }
            $versions = Read-Versions
            Start-Native 'rustup.exe' @('toolchain','install',$versions.Rust,'--profile','minimal')
            if ($script:Target -ne 'tui') {
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
            }
            [Console]::WriteLine("Setup complete. Next: scripts/rsiversi.ps1 build $script:Target")
        }
        'build' {
            if (!(Test-Prerequisites $true)) { exit 1 }
            [Console]::Error.WriteLine('Preparing the development build tool (application profile selected separately)...')
            Start-Native 'cargo.exe' @('build','--locked','-p','rsi-app-tools','--target-dir',(Join-Path $script:Root 'target'))
            # Run a private copy so another build can relink the shared tool on Windows.
            $runners = Join-Path $script:Root 'target/rsi-tools/runners'
            $runner = Join-Path $runners ([Guid]::NewGuid().ToString('N'))
            [IO.Directory]::CreateDirectory($runner) | Out-Null
            $program = Join-Path $runner 'rsi-app-tools.exe'
            Copy-Item -LiteralPath (Join-Path $script:Root 'target/debug/rsi-app-tools.exe') -Destination $program
            try { Start-Native $program (@('dist',$script:Target) + $rest) }
            finally {
                if ([IO.Path]::GetFullPath($runner).StartsWith([IO.Path]::GetFullPath($runners) + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
                    try { Remove-Item -LiteralPath $runner -Recurse -Force }
                    catch { [Console]::Error.WriteLine("Could not remove private build runner: " + $_.Exception.Message) }
                }
            }
        }
        'path' { Write-Output (Current-Bundle $script:Target) }
        'desktop' { $bundle = Current-Bundle 'desktop'; Start-Native (Join-Path $bundle 'rsi-desktop.exe') $rest $true }
        'web' { $bundle = Current-Bundle $script:Target; Start-Native (Join-Path $bundle 'rsi.exe') (@('web') + $rest) $true }
        'tui' {
            if (!$env:WT_SESSION) { throw 'Open PowerShell or Git Bash inside Windows Terminal to use the TUI. mintty and classic conhost are unsupported.' }
            $bundle = Current-Bundle $script:Target; Start-Native (Join-Path $bundle 'rsi.exe') (@('tui') + $rest) $true
        }
        default { throw "Unknown command: $operation. Run scripts/rsiversi.ps1 --help." }
    }
} catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }
