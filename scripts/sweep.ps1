# Tries clock and voltage pairs on the connected board. Each one is built with the
# stress feature, which checks the assembly against the Rust SHA-256 on every
# nonce, and held for -Seconds seconds. Results are printed as they come.
#
#   ./scripts/sweep.ps1 -Steps 250:1200,300:1250
param([string[]]$Steps, [int]$Seconds = 60)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$firmware = Join-Path $root 'firmware'

foreach ($step in ($Steps -split ',')) {
    $mhz, $mv = $step -split ':'
    $env:PICOMINER_OVERRIDE_SYS_MHZ = $mhz
    $env:PICOMINER_OVERRIDE_CORE_MV = $mv
    Push-Location $firmware
    try {
        cargo build --release --features stress 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "build failed for $step" }
    } finally {
        Pop-Location
    }

    & (Join-Path $PSScriptRoot 'flash.ps1') -NoBuild -WaitSeconds 30 | Out-Null
    Start-Sleep -Seconds 3

    $log = try {
        & (Join-Path $PSScriptRoot 'monitor.ps1') -Seconds $Seconds 6>&1 | ForEach-Object { "$_" }
    } catch {
        "NO USB: $_"
    }
    $lines = ($log -join '') -split "`r?`n"
    $last = $lines | Where-Object { $_ -match ' H/s, core0 ' } | Select-Object -Last 1
    $trouble = $lines | Where-Object { $_ -match 'safe mode|previous boot|self-test|NO USB|panicked' } | Select-Object -First 2
    "[$mhz MHz, $mv mV] $last"
    foreach ($line in $trouble) { "    $line" }
}
