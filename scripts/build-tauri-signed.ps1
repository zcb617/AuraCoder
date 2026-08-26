[CmdletBinding()]
param(
  [string]$KeyPath = "D:\work\auracoder-updater.key"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $KeyPath -PathType Leaf)) {
  throw "Tauri signing private key was not found: $KeyPath"
}

$projectRoot = Split-Path -Parent $PSScriptRoot
$hadSigningKey = Test-Path Env:TAURI_SIGNING_PRIVATE_KEY
$previousSigningKey = $env:TAURI_SIGNING_PRIVATE_KEY
$hadSigningKeyPassword = Test-Path Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
$previousSigningKeyPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
$secureSigningKeyPassword = Read-Host -Prompt "Enter the Tauri signing key password" -AsSecureString

if ($secureSigningKeyPassword.Length -eq 0) {
  throw "A Tauri signing key password is required."
}

$signingKeyPasswordBstr = [IntPtr]::Zero

try {
  $env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -LiteralPath $KeyPath -Raw
  $signingKeyPasswordBstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureSigningKeyPassword)
  $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($signingKeyPasswordBstr)

  Push-Location $projectRoot
  try {
    & pnpm tauri:build
    if ($LASTEXITCODE -ne 0) {
      throw "pnpm tauri:build failed with exit code $LASTEXITCODE"
    }
  }
  finally {
    Pop-Location
  }
}
finally {
  if ($signingKeyPasswordBstr -ne [IntPtr]::Zero) {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($signingKeyPasswordBstr)
  }
  $secureSigningKeyPassword.Dispose()

  if ($hadSigningKey) {
    $env:TAURI_SIGNING_PRIVATE_KEY = $previousSigningKey
  }
  else {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
  }

  if ($hadSigningKeyPassword) {
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $previousSigningKeyPassword
  }
  else {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
  }
}
