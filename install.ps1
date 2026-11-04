# Build Mangle from source and install it for the current user (Windows).
#   powershell -ExecutionPolicy Bypass -File .\install.ps1
#   ... -Yes           no questions
#   ... -NoFactory     skip the factory bank download
param([switch]$Yes, [switch]$NoFactory)
$ErrorActionPreference = 'Stop'
$repo = 'allthemalware/Mangle'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$clapDir = Join-Path $env:LOCALAPPDATA 'Programs\Common\CLAP'
$dataDir = Join-Path $env:APPDATA 'mangle'

if (-not $Yes -and [Environment]::UserInteractive) {
    $reply = Read-Host "Build Mangle and install it to $clapDir? [Y/n]"
    if ($reply -match '^(n|no)$') { Write-Host 'Cancelled.'; exit 0 }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (-not (Test-Path (Join-Path $cargoBin 'cargo.exe'))) {
        Write-Host 'Rust not found; installing it with rustup...'
        $exe = Join-Path $env:TEMP 'rustup-init.exe'
        Invoke-WebRequest 'https://win.rustup.rs/x86_64' -OutFile $exe
        & $exe -y --profile minimal
        if ($LASTEXITCODE -ne 0) { throw 'rustup failed. Install the Visual Studio Build Tools (C++ workload) and retry.' }
    }
    $env:PATH = "$cargoBin;$env:PATH"
}

Set-Location $here
Write-Host 'Building (the first build takes a few minutes)...'
cargo xtask bundle mangle --release
if ($LASTEXITCODE -ne 0) { throw 'Build failed. Windows needs the Visual Studio Build Tools (C++ workload).' }

New-Item -ItemType Directory -Force $clapDir | Out-Null
Copy-Item -Force 'target\bundled\mangle.clap' (Join-Path $clapDir 'mangle.clap')
Write-Host "Installed $clapDir\mangle.clap"

New-Item -ItemType Directory -Force (Join-Path $dataDir 'factory') | Out-Null
Copy-Item -Recurse -Force 'factory\*' (Join-Path $dataDir 'factory')
if (-not $NoFactory) {
    Write-Host 'Downloading the factory sounds (about 235 MB)...'
    $zip = Join-Path $env:TEMP 'mangle-factory-bank.zip'
    try {
        Invoke-WebRequest "https://github.com/$repo/releases/latest/download/mangle-factory-bank.zip" -OutFile $zip
        Expand-Archive -Force $zip $dataDir
        Write-Host "Factory sounds installed to $dataDir\factory"
    } catch {
        Write-Warning 'Could not download the factory sounds. Presets are installed.'
    } finally { Remove-Item -Force $zip -ErrorAction SilentlyContinue }
}
Write-Host 'Done. Rescan plugins in your host.'
