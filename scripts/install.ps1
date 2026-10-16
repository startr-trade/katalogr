# katalogr installer for Windows x86_64.
# Usage: install.ps1 [-Version vX.Y.Z] [-Dir PATH]
# Env: KATALOGR_VERSION, KATALOGR_INSTALL_DIR, KATALOGR_RELEASE_BASE (test hook; file://DIR reads local files).
# Exit codes: 0 ok, 1 failure, 2 usage or unsupported platform, 3 checksum mismatch.
param(
  [string]$Version = $(if ($env:KATALOGR_VERSION) { $env:KATALOGR_VERSION } else { 'latest' }),
  [string]$Dir = $(if ($env:KATALOGR_INSTALL_DIR) { $env:KATALOGR_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'katalogr\bin' })
)
$ErrorActionPreference = 'Stop'
$Base = if ($env:KATALOGR_RELEASE_BASE) { $env:KATALOGR_RELEASE_BASE } else { 'https://github.com/startr-trade/katalogr/releases' }
$Target = 'x86_64-pc-windows-msvc'

function Fail([int]$Code, [string]$Msg) { [Console]::Error.WriteLine("install.ps1: $Msg"); exit $Code }

if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -notin @('AMD64')) {
  Fail 2 "unsupported platform: $env:PROCESSOR_ARCHITECTURE"
}

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("katalogr-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null

function Fetch([string]$Asset, [string]$Dest) {
  if ($Base.StartsWith('file://')) {
    Copy-Item -LiteralPath (Join-Path $Base.Substring(7) $Asset) -Destination $Dest
  } else {
    if ($Version -eq 'latest') { $url = "$Base/latest/download/$Asset" } else { $url = "$Base/download/$Version/$Asset" }
    if (-not $url.StartsWith('https://')) { Fail 2 "only https downloads are allowed: $url" }
    Invoke-WebRequest -Uri $url -OutFile $Dest -UseBasicParsing
  }
}

try {
  Fetch 'SHA256SUMS' (Join-Path $Tmp 'SHA256SUMS')
  $lines = @(Get-Content (Join-Path $Tmp 'SHA256SUMS') | Where-Object { $_.Trim() })
  if ($Version -eq 'latest') {
    $m = @($lines | Where-Object { ($_ -split '\s+', 2)[1].TrimStart('*').EndsWith("-$Target.zip") })
    if ($m.Count -ne 1) { Fail 2 "expected exactly one $Target archive in SHA256SUMS, found $($m.Count)" }
    $Asset = ($m[0] -split '\s+', 2)[1].TrimStart('*')
  } else {
    $Asset = "katalogr-$Version-$Target.zip"
  }
  $line = @($lines | Where-Object { ($_ -split '\s+', 2)[1].TrimStart('*') -eq $Asset })
  if ($line.Count -ne 1) { Fail 2 "$Asset is not listed in SHA256SUMS" }
  $expected = ($line[0] -split '\s+', 2)[0].ToLower()

  $zip = Join-Path $Tmp $Asset
  Fetch $Asset $zip
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
  if ($actual -ne $expected) {
    [Console]::Error.WriteLine("install.ps1: checksum mismatch for $Asset")
    [Console]::Error.WriteLine("  expected: $expected")
    [Console]::Error.WriteLine("  actual:   $actual")
    Remove-Item -Recurse -Force $Tmp
    exit 3
  }

  $x = Join-Path $Tmp 'x'
  Expand-Archive -LiteralPath $zip -DestinationPath $x
  $exe = Join-Path $x 'katalogr.exe'
  if (-not (Test-Path $exe)) { Fail 1 'archive does not contain katalogr.exe' }
  New-Item -ItemType Directory -Force -Path $Dir | Out-Null
  Copy-Item -LiteralPath $exe -Destination (Join-Path $Dir 'katalogr.exe') -Force
  $installed = Join-Path $Dir 'katalogr.exe'
  Write-Host "installed ${installed}: $(& $installed --version)"
  if (-not (($env:PATH -split ';') -contains $Dir)) {
    Write-Host "note: $Dir is not on your PATH; add it in your user environment variables."
  }
} finally {
  if (Test-Path $Tmp) { Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue }
}
