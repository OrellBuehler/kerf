param([string]$Exe, [string]$ArgLine, [int]$N = 3, [int]$Warm = 0)
$walls = @(); $cpus = @()
for ($i = 0; $i -lt ($N + $Warm); $i++) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $Exe
  $psi.Arguments = $ArgLine
  $psi.UseShellExecute = $false
  $psi.RedirectStandardError = $true
  $psi.RedirectStandardOutput = $true
  $psi.CreateNoWindow = $true
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = [System.Diagnostics.Process]::Start($psi)
  $errTask = $p.StandardError.ReadToEndAsync()
  $stdTask = $p.StandardOutput.BaseStream.CopyToAsync([System.IO.Stream]::Null)
  $p.WaitForExit()
  $stdTask.Wait()
  $sw.Stop()
  $cpu = $p.TotalProcessorTime.TotalMilliseconds
  if ($p.ExitCode -ne 0) { Write-Output ("ERROR exit=" + $p.ExitCode + " " + $errTask.Result.Substring([Math]::Max(0, $errTask.Result.Length - 400))); exit 1 }
  if ($i -ge $Warm) { $walls += $sw.Elapsed.TotalMilliseconds; $cpus += $cpu }
}
$w = $walls | Sort-Object; $c = $cpus | Sort-Object
$mid = [int][Math]::Floor($w.Count / 2)
Write-Output ("wall_med_ms={0:N0} wall_min_ms={1:N0} cpu_med_ms={2:N0}" -f $w[$mid], $w[0], $c[$mid])
