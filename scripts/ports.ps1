# Finds the running PicoMiners. Dot-sourced by the other scripts.

function Get-PicoMinerPorts {
    Get-CimInstance Win32_PnPEntity |
        Where-Object { $_.DeviceID -match 'VID_C0DE&PID_CAFE' -and $_.Name -match '\((COM\d+)\)' } |
        ForEach-Object { $Matches[1] }
}

# The one port to talk to. With several boards plugged in it refuses to guess: a
# wrong guess would reboot a board that is busy mining.
function Select-PicoMinerPort([string]$Port) {
    if ($Port) { return $Port }
    $ports = @(Get-PicoMinerPorts)
    if ($ports.Count -gt 1) {
        throw "Several PicoMiners are plugged in ($($ports -join ', ')): choose one with -Port."
    }
    $ports | Select-Object -First 1
}
