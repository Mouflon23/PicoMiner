# Prints the firmware's USB serial log for -Seconds seconds (forever if 0). With
# several boards plugged in, -Port says which one.
param([int]$Seconds = 0, [string]$Port)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'ports.ps1')

$deadline = (Get-Date).AddSeconds(20)
do {
    $name = if ($Port) { @(Get-PicoMinerPorts) -eq $Port } else { Select-PicoMinerPort }
    if (-not $name) {
        if ((Get-Date) -gt $deadline) { throw 'PicoMiner serial port not found.' }
        Start-Sleep -Milliseconds 300
    }
} until ($name)

$serial = New-Object System.IO.Ports.SerialPort $name, 115200
$serial.DtrEnable = $true
$serial.Open()
Write-Host "Listening on $name"
$end = if ($Seconds -gt 0) { (Get-Date).AddSeconds($Seconds) } else { [DateTime]::MaxValue }
try {
    while ((Get-Date) -lt $end) {
        $text = $serial.ReadExisting()
        if ($text) { Write-Host $text -NoNewline }
        Start-Sleep -Milliseconds 100
    }
} finally {
    $serial.Close()
}
