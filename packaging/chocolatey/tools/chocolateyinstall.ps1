# Template: filled in by packaging/update-manifests.sh.
$ErrorActionPreference = 'Stop'

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  fileType       = 'exe'
  url64bit       = 'https://github.com/illama/illama_logitechbatteries/releases/download/v{{VERSION}}/LGSTray-Setup-{{VERSION}}.exe'
  checksum64     = '{{SETUP_SHA256}}'
  checksumType64 = 'sha256'
  # Silent per-user installation; keeps the "start with Windows" choice.
  silentArgs     = '/S'
  validExitCodes = @(0)
  softwareName   = 'LGSTray*'
}

Install-ChocolateyPackage @packageArgs
