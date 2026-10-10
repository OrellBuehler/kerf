param([string]$Mode)
$exe = (Resolve-Path .\ffmpeg.exe).Path
$m = (Resolve-Path .\media\uhd_hevc10_hlg.mp4).Path
$proxy = (Resolve-Path .\out\a.mp4).Path
$sc = "scale='min(1280,iw)':-2:flags=bilinear"
$tm = "zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:param=0.7:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
switch ($Mode) {
  "none"   { $bgargs = $null }
  "sw"     { $bgargs = "-hide_banner -loglevel error -y -stream_loop 4 -i `"$m`" -an -vf `"$sc,$tm`" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out\bg.mp4" }
  "gpu"    { $bgargs = "-hide_banner -loglevel error -y -stream_loop 4 -hwaccel d3d11va -i `"$m`" -an -vf `"$sc,$tm`" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out\bg.mp4" }
  "swsdr"  { $bgargs = "-hide_banner -loglevel error -y -stream_loop 4 -i `"$m`" -an -vf `"$sc`" -c:v libx264 -preset veryfast -crf 24 -g 1 -threads 12 -pix_fmt yuv420p -f mp4 out\bg.mp4" }
  "gpusdr" { $bgargs = "-hide_banner -loglevel error -y -stream_loop 4 -hwaccel d3d11va -i `"$m`" -an -vf `"$sc`" -c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -g 1 -pix_fmt nv12 -f mp4 out\bg.mp4" }
}
$bg = $null
if ($bgargs) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $exe; $psi.Arguments = $bgargs; $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
  $bg = [System.Diagnostics.Process]::Start($psi)
  $bg.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::BelowNormal
  Start-Sleep -Milliseconds 1500
}
$fg = "color=c=black:s=960x540:d=0.1[base];[0:v]trim=end_frame=1,setpts=PTS-STARTPTS,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2,setsar=1[v0];[base][v0]overlay=(W-w)/2:(H-h)/2[ov0];[ov0]null[outv]"
$times = @()
for ($i = 0; $i -lt 15; $i++) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $exe
  $psi.Arguments = "-hide_banner -loglevel error -ss $(3.5 + $i*0.1) -i `"$proxy`" -filter_complex `"$fg`" -map [outv] -frames:v 1 -q:v 4 -f image2pipe -vcodec mjpeg pipe:1"
  $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true; $psi.CreateNoWindow = $true
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = [System.Diagnostics.Process]::Start($psi)
  $null = $p.StandardError.ReadToEndAsync()
  $p.StandardOutput.BaseStream.CopyTo([System.IO.Stream]::Null)
  $p.WaitForExit(); $sw.Stop()
  $times += $sw.Elapsed.TotalMilliseconds
}
$alive = $bg -and -not $bg.HasExited
if ($bg) { if (-not $bg.HasExited) { $bg.Kill() }; $bg.WaitForExit() }
$s = $times | Sort-Object
Write-Output ("{0}: scrub still median={1:N0} ms  min={2:N0}  max={3:N0}  (bg still running at end: {4})" -f $Mode, $s[7], $s[0], $s[14], $alive)
