param([int]$N = 2, [string]$Mode = "gpusdr")
$exe = (Resolve-Path .\ffmpeg.exe).Path
$m = (Resolve-Path .\media\uhd_hevc10_hlg.mp4).Path
$sc = "scale='min(1280,iw)':-2:flags=bilinear"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$ps = @()
for ($i = 0; $i -lt $N; $i++) {
  if ($Mode -eq "gpusdr") { $a = "-hide_banner -loglevel error -y -hwaccel d3d11va -i `"$m`" -an -vf `"$sc`" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out\par$i.mp4" }
  else { $a = "-hide_banner -loglevel error -y -i `"$m`" -an -vf `"$sc`" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out\par$i.mp4" }
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $exe; $psi.Arguments = $a; $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
  $ps += [System.Diagnostics.Process]::Start($psi)
}
foreach ($p in $ps) { $p.WaitForExit() }
$sw.Stop()
$cpu = ($ps | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
Write-Output ("{0} x{1}: wall={2:N0} ms total_cpu={3:N0} ms  aggregate={4:N0} fps" -f $Mode, $N, $sw.Elapsed.TotalMilliseconds, $cpu, (240 * $N / $sw.Elapsed.TotalSeconds))
