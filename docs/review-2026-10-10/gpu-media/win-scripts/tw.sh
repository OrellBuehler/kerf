#!/bin/bash
# usage: tw.sh N "args..."   (runs ffmpeg.exe in cwd with the arg line, prints wall/cpu)
N=$1; shift
powershell.exe -NoProfile -ExecutionPolicy Bypass -File t.ps1 -Exe "$(wslpath -w "$PWD/ffmpeg.exe")" -ArgLine "$*" -N $N 2>&1 | tr -d '\r'
