# dotenv-cloud installer for Windows (PowerShell).
#
#   irm https://geetchoubey.github.io/dotenv-cloud/install.ps1 | iex
#
# Environment overrides:
#   $env:DOTENV_CLOUD_VERSION   tag to install (e.g. v0.1.0-beta.3). Default: latest.
#   $env:DOTENV_CLOUD_BIN_DIR   install directory. Default: %LOCALAPPDATA%\dotenv-cloud\bin.
#
# The downloaded archive is verified against its published SHA-256 digest.

$ErrorActionPreference = 'Stop'

$Repo = 'geetchoubey/dotenv-cloud'
$Bin = 'dotenv-cloud'
$Target = 'x86_64-pc-windows-msvc'
$BinDir = if ($env:DOTENV_CLOUD_BIN_DIR) { $env:DOTENV_CLOUD_BIN_DIR } else { "$env:LOCALAPPDATA\dotenv-cloud\bin" }

function Info($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }

function Get-LatestTag {
  # Newest release including prereleases.
  $rels = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases" -Headers @{ 'User-Agent' = 'dotenv-cloud-installer' }
  return $rels[0].tag_name
}

$Tag = if ($env:DOTENV_CLOUD_VERSION) { $env:DOTENV_CLOUD_VERSION } else { Get-LatestTag }
if (-not $Tag) { throw 'could not determine the latest version' }

$Archive = "$Bin-$Tag-$Target.zip"
$Base = "https://github.com/$Repo/releases/download/$Tag"

Info "Installing $Bin $Tag ($Target)"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
  $ZipPath = Join-Path $Tmp $Archive
  Info "Downloading $Archive"
  Invoke-WebRequest -Uri "$Base/$Archive" -OutFile $ZipPath -UseBasicParsing

  # Verify integrity against the published .sha256 sidecar.
  $expected = $null
  try {
    $shaLine = (Invoke-WebRequest -Uri "$Base/$Archive.sha256" -UseBasicParsing).Content
    $expected = ($shaLine -split '\s+')[0].ToLower()
  } catch {
    Warn 'no .sha256 sidecar found; skipping integrity check'
  }
  if ($expected) {
    $actual = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "SHA-256 mismatch (expected $expected, got $actual)" }
    Info 'SHA-256 verified'
  }

  Expand-Archive -Path $ZipPath -DestinationPath $Tmp -Force
  $Src = Get-ChildItem -Path $Tmp -Recurse -Filter "$Bin.exe" | Select-Object -First 1
  if (-not $Src) { throw "could not find $Bin.exe inside the archive" }

  New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
  Copy-Item -Path $Src.FullName -Destination (Join-Path $BinDir "$Bin.exe") -Force
  Info "Installed to $BinDir\$Bin.exe"
}
finally {
  Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

# Add to the user PATH if missing.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$BinDir*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$BinDir", 'User')
  Warn "Added $BinDir to your user PATH. Restart your terminal to pick it up."
}

Write-Host ''
Info 'Done. Get started:'
Write-Host "    $Bin --help"
Write-Host "    $Bin init"
