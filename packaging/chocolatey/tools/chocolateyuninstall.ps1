# The application's own uninstaller (lgstray.exe --uninstall) asks
# questions, so the per-user installation is removed here directly.
# The settings (%APPDATA%\LGSTray) are kept.
$ErrorActionPreference = 'Stop'

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\LGSTray'
$shortcut   = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\LGSTray.lnk'

Get-Process -Name lgstray -ErrorAction SilentlyContinue |
  Where-Object { $_.Path -and $_.Path.StartsWith($installDir, [StringComparison]::OrdinalIgnoreCase) } |
  Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'LGSTray' -ErrorAction SilentlyContinue
Remove-Item -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\LGSTray' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -Path $shortcut -Force -ErrorAction SilentlyContinue
Remove-Item -Path $installDir -Recurse -Force -ErrorAction SilentlyContinue
