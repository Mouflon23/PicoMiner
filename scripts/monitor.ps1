# Prints the firmware's USB serial log for -Seconds seconds (forever if 0).
param([int]$Seconds = 0)

$ErrorActionPreference = 'Stop'

$deadline = (Get-Date).AddSeconds(20)
do {
    $device = Get-CimInstance Win32_PnPEntity |
        Where-Object { $_.DeviceID -match 'VID_C0DE&PID_CAFE' -and $_.Name -match '\((COM\d+)\)' } |
        Select-Object -First 1
    if (-not $device) {
        if ((Get-Date) -gt $deadline) { throw 'PicoMiner serial port not found.' }
        Start-Sleep -Milliseconds 300
    }
} until ($device)

$null = $device.Name -match '\((COM\d+)\)'
$port = New-Object System.IO.Ports.SerialPort $Matches[1], 115200
$port.DtrEnable = $true
$port.Open()
Write-Host "Listening on $($Matches[1])"
$end = if ($Seconds -gt 0) { (Get-Date).AddSeconds($Seconds) } else { [DateTime]::MaxValue }
try {
    while ((Get-Date) -lt $end) {
        $text = $port.ReadExisting()
        if ($text) { Write-Host $text -NoNewline }
        Start-Sleep -Milliseconds 100
    }
} finally {
    $port.Close()
}
