# Builds the firmware for both boards, packs it into one UF2 and flashes it through
# the bootloader. A board already in the bootloader is flashed first; otherwise a
# running PicoMiner is rebooted over USB serial, -Port saying which if there are
# several. For a new board, hold BOOTSEL while plugging it in.
param([switch]$NoBuild, [int]$WaitSeconds = 15, [string]$Port, [string]$Features)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'ports.ps1')
$root = Split-Path $PSScriptRoot -Parent
$targets = 'thumbv6m-none-eabi', 'thumbv8m.main-none-eabihf'

if (-not $NoBuild) {
    Push-Location (Join-Path $root 'firmware')
    try {
        foreach ($target in $targets) {
            $extra = if ($Features) { '--features', $Features } else { @() }
            cargo build --release --target $target @extra
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        }
    } finally {
        Pop-Location
    }
}

$uf2 = Join-Path $root 'target\picominer.uf2'
$elfs = $targets | ForEach-Object { Join-Path $root "target\$_\release\picominer" }
python (Join-Path $root 'tools\uf2.py') $uf2 @elfs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

function Find-Bootloader {
    Get-CimInstance Win32_LogicalDisk |
        Where-Object { $_.VolumeName -in 'RPI-RP2', 'RP2350' } |
        Select-Object -First 1
}

if (-not (Find-Bootloader)) {
    $name = Select-PicoMinerPort $Port
    if ($name) {
        Write-Host "Rebooting $name into the bootloader"
        $serial = New-Object System.IO.Ports.SerialPort $name, 115200
        $serial.DtrEnable = $true
        $serial.Open()
        $serial.Write("bootsel`n")
        Start-Sleep -Milliseconds 200
        try { $serial.Close() } catch { }
    }
}

$deadline = (Get-Date).AddSeconds($WaitSeconds)
while (-not ($drive = Find-Bootloader)) {
    if ((Get-Date) -gt $deadline) { throw 'No bootloader drive: hold BOOTSEL while plugging the Pico in.' }
    Start-Sleep -Milliseconds 300
}

$board = $drive.VolumeName
# The board restarts as soon as it has its own image, which can be before Windows
# has written the rest of the file.
try { Copy-Item $uf2 "$($drive.DeviceID)\" } catch { Write-Host "Board left before the end of the file: $_" }
Write-Host "Flashed $uf2 ($((Get-Item $uf2).Length) bytes) to $board on $($drive.DeviceID)"
