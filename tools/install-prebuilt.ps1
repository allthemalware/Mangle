# Install the prebuilt plugin from a release archive (Windows).
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$clapDir = Join-Path $env:LOCALAPPDATA 'Programs\Common\CLAP'
$dataDir = Join-Path $env:APPDATA 'mangle'
New-Item -ItemType Directory -Force $clapDir, (Join-Path $dataDir 'factory') | Out-Null
Copy-Item -Force (Join-Path $here 'mangle.clap') (Join-Path $clapDir 'mangle.clap')
Copy-Item -Recurse -Force (Join-Path $here 'factory\*') (Join-Path $dataDir 'factory')
Write-Host "Installed to $clapDir. Rescan plugins in your host."
