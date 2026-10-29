[CmdletBinding()]
param(
    [string]$Manifest,
    [string]$ExactTest,
    [ValidateRange(1, 1800)][int]$TimeoutSeconds = 1500,
    [string]$WorkerConfiguration
)
$ErrorActionPreference = 'Stop'

if ($WorkerConfiguration) {
    $configuration = Get-Content -LiteralPath $WorkerConfiguration -Raw | ConvertFrom-Json
    $result = 1
    try {
        Set-Location -LiteralPath $configuration.workspace
        foreach ($property in $configuration.environment.PSObject.Properties) {
            [Environment]::SetEnvironmentVariable($property.Name, [string]$property.Value, 'Process')
        }
        $artifacts = @(Get-Content -LiteralPath $configuration.manifest | ForEach-Object { $_ | ConvertFrom-Json } |
            Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.executable })
        if ($configuration.exactTest) {
            $artifacts = @($artifacts | Where-Object { $_.target.name -eq 'resident_listeners' })
        }
        if (-not $artifacts.Count) { throw 'No native test artifacts were produced' }
        $result = 0
        foreach ($artifact in $artifacts) {
            if ($configuration.exactTest) {
                $listing = & $artifact.executable $configuration.exactTest --ignored --exact --list
                if ($LASTEXITCODE -ne 0 -or $listing -notcontains "$($configuration.exactTest): test") {
                    throw 'Missing exact frontend watch test'
                }
                & $artifact.executable $configuration.exactTest --ignored --exact *> $configuration.log
            } else {
                & $artifact.executable *>> $configuration.log
            }
            if ($LASTEXITCODE -ne 0) { $result = 1 }
        }
    } catch {
        $_ | Out-String | Add-Content -LiteralPath $configuration.log
        $result = 1
    } finally {
        $pendingResult = $configuration.result + '.pending'
        Set-Content -LiteralPath $pendingResult -Value $result
        Move-Item -LiteralPath $pendingResult -Destination $configuration.result
    }
    exit $result
}

if (-not $Manifest) { throw 'Manifest is required' }
$taskName = 'rsiversi-native-tests-' + [Guid]::NewGuid().ToString('N')
$temporary = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$directory = Join-Path $temporary $taskName
$null = New-Item -ItemType Directory -Path $directory
$configurationPath = Join-Path $directory 'configuration.json'
$environment = @{}
foreach ($name in @('PATH', 'CARGO_HOME', 'RUSTUP_HOME', 'CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS', 'RSI_TEST_NODE')) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ($null -ne $value) { $environment[$name] = $value }
}
$configuration = @{
    workspace = (Get-Location).Path
    manifest = (Resolve-Path -LiteralPath $Manifest).Path
    exactTest = $ExactTest
    environment = $environment
    log = Join-Path $directory 'output.log'
    result = Join-Path $directory 'exit-code.txt'
}
$configuration | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $configurationPath
$registered = $false
try {
    $action = New-ScheduledTaskAction -Execute (Join-Path $PSHOME 'pwsh.exe') -Argument (
        '-NoLogo -NoProfile -NonInteractive -File "{0}" -WorkerConfiguration "{1}"' -f $PSCommandPath, $configurationPath)
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType S4U -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds $TimeoutSeconds) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    $null = Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -ErrorAction Stop
    $registered = $true
    Start-ScheduledTask -TaskName $taskName -ErrorAction Stop
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not (Test-Path -LiteralPath $configuration.result)) {
        if ([DateTime]::UtcNow -ge $deadline) { throw "Native test task exceeded $TimeoutSeconds seconds" }
        Start-Sleep -Seconds 1
    }
    $result = [int](Get-Content -LiteralPath $configuration.result -Raw)
} finally {
    if ($registered) {
        Stop-ScheduledTask -TaskName $taskName
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
    if (Test-Path -LiteralPath $configuration.log) { Get-Content -LiteralPath $configuration.log }
    Write-Host "Native test evidence: $directory"
}
exit $result
