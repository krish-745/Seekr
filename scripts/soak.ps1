# Soak test: samples Seekr's processes every few minutes for hours, to a CSV in
# %LOCALAPPDATA%\Seekr\soak. Start it detached and leave Seekr in normal use:
#   Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','scripts\soak.ps1','-Hours','72'
# Summarize a run (memory and handle growth, average CPU, GPU states, battery drain):
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\soak.ps1 -Report
param(
    [double]$Hours = 72,
    [int]$EveryMinutes = 5,
    [switch]$Report
)
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:LOCALAPPDATA 'Seekr\soak'
New-Item -ItemType Directory -Force $dir | Out-Null
$names = 'seekr', 'seekr-indexer', 'seekr-svc', 'llama-server'

if ($Report) {
    $file = Get-ChildItem $dir -Filter 'soak-*.csv' | Sort-Object LastWriteTime | Select-Object -Last 1
    if (-not $file) { throw "no soak run in $dir" }
    $rows = Import-Csv $file.FullName
    $first, $last = $rows[0], $rows[-1]
    $hours = ([datetime]$last.time - [datetime]$first.time).TotalHours
    "{0}: {1} samples over {2:N1} hours" -f $file.Name, $rows.Count, $hours
    foreach ($name in $names) {
        $mine = @($rows | Where-Object process -eq $name)
        if ($mine.Count -lt 2) { continue }
        $a, $b = $mine[0], $mine[-1]
        # A service's CPU time can't be read without administrator rights.
        $cpu = if ($a.cpu_s -and $b.cpu_s) {
            '{0:N3}% of the machine' -f (([double]$b.cpu_s - [double]$a.cpu_s) / (([datetime]$b.time - [datetime]$a.time).TotalSeconds) * 100 / [Environment]::ProcessorCount)
        } else { 'not readable' }
        "{0,-14} working set {1:N0} -> {2:N0} MB (max {3:N0}), private {4:N0} -> {5:N0} MB, handles {6} -> {7} (max {8}), average CPU {9}" -f $name,
            ([double]$a.ws_mb), ([double]$b.ws_mb), ($mine | Measure-Object ws_mb -Maximum).Maximum,
            ([double]$a.private_mb), ([double]$b.private_mb), $a.handles, $b.handles,
            ($mine | Measure-Object handles -Maximum).Maximum, $cpu
    }
    $states = $rows | Where-Object process -eq 'gpu' | Group-Object pstate | ForEach-Object { "$($_.Name) x$($_.Count)" }
    "GPU states: " + ($states -join ', ')
    $battery = @($rows | Where-Object { $_.process -eq 'battery' -and $_.on_battery -eq 'True' })
    if ($battery.Count -ge 2) {
        $span = ([datetime]$battery[-1].time - [datetime]$battery[0].time).TotalHours
        if ($span -gt 0) {
            "Battery: {0:N1}% per hour over {1:N1} hours on battery (all programs, not only Seekr)" -f (([double]$battery[0].charge - [double]$battery[-1].charge) / $span), $span
        }
    }
    return
}

$csv = Join-Path $dir ("soak-{0:yyyyMMdd-HHmm}.csv" -f (Get-Date))
'time,process,pid,cpu_s,ws_mb,private_mb,handles,threads,pstate,charge,on_battery' | Set-Content $csv -Encoding utf8
$end = (Get-Date).AddHours($Hours)
while ((Get-Date) -lt $end) {
    $now = (Get-Date).ToString('o')
    $lines = foreach ($p in Get-Process $names -ErrorAction SilentlyContinue) {
        '{0},{1},{2},{3:F2},{4:F1},{5:F1},{6},{7},,,' -f $now, $p.Name, $p.Id, $p.TotalProcessorTime.TotalSeconds,
            ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB), $p.HandleCount, $p.Threads.Count
    }
    $pstate = try { (& nvidia-smi --query-gpu=pstate --format=csv,noheader 2>$null | Select-Object -First 1).Trim() } catch { '' }
    $lines += '{0},gpu,,,,,,,{1},,' -f $now, $pstate
    $b = Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($b) { $lines += '{0},battery,,,,,,,,{1},{2}' -f $now, $b.EstimatedChargeRemaining, ($b.BatteryStatus -eq 1) }
    $lines | Add-Content $csv -Encoding utf8
    Start-Sleep -Seconds ($EveryMinutes * 60)
}
