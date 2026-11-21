# The application's own uninstaller (batly.exe --uninstall) asks
# questions, so the per-user installation is removed here directly.
# The settings (%APPDATA%\Batly) are kept.
$ErrorActionPreference = 'Stop'

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\Batly'
$shortcut   = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Batly.lnk'

Get-Process -Name batly -ErrorAction SilentlyContinue |
  Where-Object { $_.Path -and $_.Path.StartsWith($installDir, [StringComparison]::OrdinalIgnoreCase) } |
  Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Batly' -ErrorAction SilentlyContinue
Remove-Item -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Batly' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -Path $shortcut -Force -ErrorAction SilentlyContinue
Remove-Item -Path $installDir -Recurse -Force -ErrorAction SilentlyContinue
