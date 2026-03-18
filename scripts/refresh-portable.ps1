param(
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$sourceExe = Join-Path $repoRoot "target\release\workspace-terminal-desktop.exe"
$portableDir = Join-Path $repoRoot "target\release\bundle\portable\Workspace Terminal Portable"
$portableExe = Join-Path $portableDir "Workspace Terminal Portable.exe"
$sourceTmuxExe = Join-Path $repoRoot "target\release\tmux.exe"
$legacyRootExe = Join-Path $repoRoot "Workspace Terminal Portable.exe"
$manifestPath = Join-Path $repoRoot "apps\desktop\src-tauri\Cargo.toml"

function Assert-UnlockedFile {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Label
  )

  if (-not (Test-Path -LiteralPath $Path)) {
    return
  }

  try {
    $stream = [System.IO.File]::Open(
      $Path,
      [System.IO.FileMode]::Open,
      [System.IO.FileAccess]::ReadWrite,
      [System.IO.FileShare]::None
    )
    $stream.Close()
    $stream.Dispose()
  } catch {
    throw "$Label is locked by another process. Close it and retry. Path: $Path"
  }
}

function Get-Sha256 {
  param([Parameter(Mandatory = $true)][string]$Path)

  $sha = [System.Security.Cryptography.SHA256]::Create()
  $stream = [System.IO.File]::OpenRead($Path)
  try {
    $hashBytes = $sha.ComputeHash($stream)
    return ([System.BitConverter]::ToString($hashBytes)).Replace("-", "")
  } finally {
    $stream.Close()
    $stream.Dispose()
    $sha.Dispose()
  }
}

if (-not $SkipBuild) {
  Write-Host "[portable] Building embedded tmux shim binary..."
  cargo build --manifest-path $manifestPath --release --bin tmux
  if ($LASTEXITCODE -ne 0) {
    throw "tmux shim build failed."
  }

  if (-not (Test-Path -LiteralPath $sourceTmuxExe)) {
    throw "Release tmux shim executable not found: $sourceTmuxExe"
  }

  Write-Host "[portable] Building release app executable with embedded tmux shim..."
  $previousEmbeddedShimPath = $env:WORKSPACE_TERMINAL_EMBED_TMUX_PATH
  try {
    $env:WORKSPACE_TERMINAL_EMBED_TMUX_PATH = $sourceTmuxExe
    cargo build --manifest-path $manifestPath --release --bin workspace-terminal-desktop
    if ($LASTEXITCODE -ne 0) {
      throw "Release app build failed."
    }
  } finally {
    if ($null -ne $previousEmbeddedShimPath) {
      $env:WORKSPACE_TERMINAL_EMBED_TMUX_PATH = $previousEmbeddedShimPath
    } else {
      Remove-Item Env:WORKSPACE_TERMINAL_EMBED_TMUX_PATH -ErrorAction SilentlyContinue
    }
  }
}

if (-not (Test-Path -LiteralPath $sourceExe)) {
  throw "Release source executable not found: $sourceExe"
}

New-Item -ItemType Directory -Path $portableDir -Force | Out-Null
Assert-UnlockedFile -Path $portableExe -Label "Portable executable"

$portableShimDir = Join-Path $portableDir "shim"
if (Test-Path -LiteralPath $portableShimDir) {
  Remove-Item -LiteralPath $portableShimDir -Recurse -Force
}

Write-Host "[portable] Syncing release executable to portable path..."
Copy-Item -LiteralPath $sourceExe -Destination $portableExe -Force

$sourceHash = Get-Sha256 -Path $sourceExe
$portableHash = Get-Sha256 -Path $portableExe
if ($sourceHash -ne $portableHash) {
  throw "Portable hash verification failed. Source and destination hashes differ."
}

$sourceMeta = Get-Item -LiteralPath $sourceExe
$portableMeta = Get-Item -LiteralPath $portableExe

Write-Host "[portable] Refresh completed."
Write-Host "  Source   : $sourceExe"
Write-Host "  Portable : $portableExe"
Write-Host "  SHA256   : $portableHash"
Write-Host "  Embedded tmux source : $sourceTmuxExe"
Write-Host "  Size     : $($portableMeta.Length) bytes"
Write-Host "  Source Updated  : $($sourceMeta.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))"
Write-Host "  Portable Updated: $($portableMeta.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))"

if (Test-Path -LiteralPath $legacyRootExe) {
  $legacyHash = Get-Sha256 -Path $legacyRootExe
  if ($legacyHash -ne $portableHash) {
    Write-Warning "Legacy root executable is stale and excluded from deployment verification: $legacyRootExe"
  } else {
    Write-Warning "Legacy root executable matches but is still excluded from deployment verification: $legacyRootExe"
  }
}
