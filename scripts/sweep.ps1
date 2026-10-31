# Tries clock and voltage pairs on one board. Each one is built with the stress
# feature, which checks the hashing loops against the Rust SHA-256 on every nonce,
# and held for -Seconds seconds. Results are printed as they come. With several
# boards plugged in, -Port says which one.
#
#   ./scripts/sweep.ps1 -Steps 250:1200,300:1250
param([string[]]$Steps, [int]$Seconds = 60, [string]$Port)

$ErrorActionPreference = 'Stop'

foreach ($step in ($Steps -split ',')) {
    $mhz, $mv = $step -split ':'
    $env:PICOMINER_OVERRIDE_SYS_MHZ = $mhz
    $env:PICOMINER_OVERRIDE_CORE_MV = $mv

    & (Join-Path $PSScriptRoot 'flash.ps1') -Features stress -WaitSeconds 30 -Port $Port 2>&1 | Out-Null
    Start-Sleep -Seconds 3

    $log = try {
        & (Join-Path $PSScriptRoot 'monitor.ps1') -Seconds $Seconds -Port $Port 6>&1 | ForEach-Object { "$_" }
    } catch {
        "NO USB: $_"
    }
    $lines = ($log -join '') -split "`r?`n"
    $last = $lines | Where-Object { $_ -match ' H/s, core0 ' } | Select-Object -Last 1
    $trouble = $lines | Where-Object { $_ -match 'safe mode|previous boot|self-test|NO USB|panicked' } | Select-Object -First 2
    "[$mhz MHz, $mv mV] $last"
    foreach ($line in $trouble) { "    $line" }
}
