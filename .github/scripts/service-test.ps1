# Installs the windows_service example as a real Windows service, drives it
# with sc.exe (start, paramchange, control 128, stop), and checks what it
# logged and its exit code. The service is always deleted afterwards, even
# when a step fails. Needs an administrator shell (GitHub's Windows
# runners are).
#
# Usage: service-test.ps1 <path to windows_service.exe>

param([Parameter(Mandatory = $true)][string]$Exe)

$ErrorActionPreference = 'Stop'
$name = 'signal-mod-ci-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$log = Join-Path $env:RUNNER_TEMP "$name.log"
if (-not $env:RUNNER_TEMP) { $log = Join-Path $env:TEMP "$name.log" }
$exePath = (Resolve-Path $Exe).Path

function Wait-State([string]$want, [int]$seconds = 30) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        $state = (Get-Service -Name $name).Status.ToString()
        if ($state -eq $want) { return }
        Start-Sleep -Milliseconds 200
    }
    throw "service $name did not reach $want (last: $state)"
}

function Wait-Log([string]$line, [int]$seconds = 30) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path $log) -and ((Get-Content $log) -contains $line)) { return }
        Start-Sleep -Milliseconds 200
    }
    throw "the service never logged '$line'"
}

function Sc([string[]]$arguments) {
    $output = & sc.exe @arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "sc.exe $($arguments -join ' ') failed: $output" }
    $output
}

try {
    Sc @('create', $name, 'binPath=', "`"$exePath`" `"$log`"", 'start=', 'demand') | Out-Null
    Sc @('start', $name) | Out-Null
    Wait-State 'Running'
    Wait-Log 'running'

    Sc @('control', $name, 'paramchange') | Out-Null
    Wait-Log 'User2'
    Sc @('control', $name, '128') | Out-Null
    Wait-Log 'User1'

    Sc @('stop', $name) | Out-Null
    Wait-State 'Stopped'
    Wait-Log 'Terminate'
    Wait-Log 'stopped'

    $query = Sc @('query', $name) | Out-String
    if ($query -notmatch 'WIN32_EXIT_CODE\s*:\s*0\b') {
        throw "unexpected exit code:`n$query"
    }
    Write-Host "service test passed"
    Get-Content $log
}
finally {
    # Always remove the service, whatever happened above.
    & sc.exe stop $name 2>&1 | Out-Null
    & sc.exe delete $name 2>&1 | Out-Null
    if (Test-Path $log) { Write-Host "--- service log ---"; Get-Content $log }
}
