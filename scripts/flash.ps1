# Builds the firmware and flashes it through the UF2 bootloader. A Pico already
# running PicoMiner is rebooted over USB serial; otherwise hold BOOTSEL while
# plugging it in.
param([switch]$NoBuild, [int]$WaitSeconds = 15)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$elf = Join-Path $root 'target\thumbv6m-none-eabi\release\picominer'

if (-not $NoBuild) {
    Push-Location (Join-Path $root 'firmware')
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Pop-Location
    }
}

function Find-Bootloader {
    Get-CimInstance Win32_LogicalDisk | Where-Object { $_.VolumeName -eq 'RPI-RP2' } | Select-Object -First 1
}

if (-not (Find-Bootloader)) {
    $device = Get-CimInstance Win32_PnPEntity |
        Where-Object { $_.DeviceID -match 'VID_C0DE&PID_CAFE' -and $_.Name -match '\((COM\d+)\)' } |
        Select-Object -First 1
    if ($device) {
        $null = $device.Name -match '\((COM\d+)\)'
        Write-Host "Rebooting $($Matches[1]) into the bootloader"
        $port = New-Object System.IO.Ports.SerialPort $Matches[1], 115200
        $port.DtrEnable = $true
        $port.Open()
        $port.Write("bootsel`n")
        Start-Sleep -Milliseconds 200
        try { $port.Close() } catch { }
    }
}

$deadline = (Get-Date).AddSeconds($WaitSeconds)
while (-not ($drive = Find-Bootloader)) {
    if ((Get-Date) -gt $deadline) { throw 'No RPI-RP2 drive: hold BOOTSEL while plugging the Pico in.' }
    Start-Sleep -Milliseconds 300
}

$uf2 = "$elf.uf2"
elf2uf2-rs $elf $uf2
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Copy-Item $uf2 "$($drive.DeviceID)\"
Write-Host "Flashed $uf2 ($((Get-Item $uf2).Length) bytes) to $($drive.DeviceID)"
