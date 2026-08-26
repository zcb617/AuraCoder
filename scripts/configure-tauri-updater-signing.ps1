[CmdletBinding()]
param(
  [string]$Repository = "zcb617/AuraCoder",
  [switch]$CheckOnly,
  [switch]$Rotate
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$keyRelativePath = ".tauri-signing/auracoder-updater.key"
$publicKeyRelativePath = "$keyRelativePath.pub"
$passwordRelativePath = "$keyRelativePath.password"
$privateKeyPath = Join-Path $repoRoot $keyRelativePath
$publicKeyPath = Join-Path $repoRoot $publicKeyRelativePath
$privateKeyPasswordPath = Join-Path $repoRoot $passwordRelativePath
$tauriConfigPath = Join-Path $repoRoot "src-tauri/tauri.conf.json"
$publicExcludePath = Join-Path $repoRoot "scripts/github-public-excludes.txt"

# 公开导出必须始终排除私有 updater 恢复材料，缺少规则时禁止继续配置 Secret。
$publicExcludeRules = Get-Content -LiteralPath $publicExcludePath | ForEach-Object {
  $trimmedRule = $_.Trim()
  if (-not [string]::IsNullOrWhiteSpace($trimmedRule) -and -not $trimmedRule.StartsWith("#")) {
    $trimmedRule
  }
}
if ($publicExcludeRules -notcontains ".tauri-signing/**") {
  throw "The public export excludes must contain .tauri-signing/** before configuring updater secrets."
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
  throw "GitHub CLI (gh) is required. Install it and authenticate before continuing."
}

if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
  throw "pnpm is required to verify the Tauri updater signing key."
}

function Test-TauriUpdaterSigningMaterial {
  param(
    [Parameter(Mandatory = $true)]
    [string]$KeyPath,
    [Parameter(Mandatory = $true)]
    [string]$Password
  )

  $validationFilePath = Join-Path (Split-Path -Parent $KeyPath) ".signing-validation"
  [System.IO.File]::WriteAllText($validationFilePath, "Tauri updater signing validation.`n", [System.Text.UTF8Encoding]::new($false))

  try {
    $null = & pnpm exec tauri signer sign --private-key-path $KeyPath --password $Password $validationFilePath
    if ($LASTEXITCODE -ne 0) {
      throw "The local Tauri updater signing key and password do not form a usable signing pair."
    }
  }
  finally {
    Remove-Item -LiteralPath $validationFilePath, "$validationFilePath.sig" -Force -ErrorAction SilentlyContinue
  }
}

function Set-GitHubActionsSecretFromFile {
  param(
    [Parameter(Mandatory = $true)]
    [string]$SecretName,
    [Parameter(Mandatory = $true)]
    [string]$SecretFilePath,
    [Parameter(Mandatory = $true)]
    [string]$TargetRepository
  )

  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = (Get-Command gh -ErrorAction Stop).Source
  $startInfo.UseShellExecute = $false
  $startInfo.RedirectStandardInput = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  foreach ($argument in @("secret", "set", $SecretName, "--repo", $TargetRepository, "--app", "actions")) {
    [void]$startInfo.ArgumentList.Add($argument)
  }

  $process = [System.Diagnostics.Process]::new()
  $process.StartInfo = $startInfo
  if (-not $process.Start()) {
    throw "Unable to start GitHub CLI while configuring $SecretName."
  }

  $secretStream = [System.IO.File]::OpenRead($SecretFilePath)
  try {
    $secretStream.CopyTo($process.StandardInput.BaseStream)
  }
  finally {
    $secretStream.Dispose()
    $process.StandardInput.Close()
  }

  $stdout = $process.StandardOutput.ReadToEnd()
  $stderr = $process.StandardError.ReadToEnd()
  $process.WaitForExit()
  if ($process.ExitCode -ne 0) {
    throw "Unable to configure $SecretName in ${TargetRepository}: $stderr$stdout"
  }
}

if ($Rotate -and $CheckOnly) {
  throw "-Rotate cannot be used together with -CheckOnly."
}

if ($Rotate) {
  $archiveDir = Join-Path $repoRoot (".tauri-signing/retired/" + (Get-Date -Format "yyyyMMdd-HHmmss"))
  New-Item -ItemType Directory -Path $archiveDir -Force | Out-Null

  foreach ($existingPath in @($privateKeyPath, $publicKeyPath, $privateKeyPasswordPath)) {
    if (Test-Path -LiteralPath $existingPath) {
      Move-Item -LiteralPath $existingPath -Destination $archiveDir
    }
  }

  $passwordBytes = [byte[]]::new(32)
  [System.Security.Cryptography.RandomNumberGenerator]::Fill($passwordBytes)
  $signingPassword = [Convert]::ToBase64String($passwordBytes).TrimEnd("=").Replace("+", "-").Replace("/", "_")
  [System.IO.File]::WriteAllText($privateKeyPasswordPath, $signingPassword, [System.Text.UTF8Encoding]::new($false))

  $null = & pnpm exec tauri signer generate --ci --password $signingPassword --write-keys $privateKeyPath
  if ($LASTEXITCODE -ne 0) {
    throw "Unable to generate a new Tauri updater signing key."
  }

  Test-TauriUpdaterSigningMaterial -KeyPath $privateKeyPath -Password $signingPassword

  $newEmbeddedPublicKey = [System.IO.File]::ReadAllText($publicKeyPath).Trim()
  $tauriConfigText = [System.IO.File]::ReadAllText($tauriConfigPath)
  $updatedTauriConfigText = [System.Text.RegularExpressions.Regex]::Replace(
    $tauriConfigText,
    '("pubkey"\s*:\s*")[^"]+("\s*)',
    ('${1}' + $newEmbeddedPublicKey + '${2}'),
    1
  )
  if ($updatedTauriConfigText -eq $tauriConfigText) {
    throw "Unable to update the updater public key in src-tauri/tauri.conf.json."
  }
  [System.IO.File]::WriteAllText($tauriConfigPath, $updatedTauriConfigText, [System.Text.UTF8Encoding]::new($false))

  Write-Output "A new Tauri updater signing key was generated. Previous local recovery material was saved to the Gogs recovery archive path $archiveDir; commit that archive to private Gogs."
}

foreach ($requiredPath in @($privateKeyPath, $publicKeyPath, $privateKeyPasswordPath, $tauriConfigPath)) {
  if (-not (Test-Path -LiteralPath $requiredPath)) {
    throw "Required signing file is missing: $requiredPath"
  }
}

foreach ($keyPath in @($keyRelativePath, $publicKeyRelativePath, $passwordRelativePath)) {
  & git -C $repoRoot check-ignore -q -- $keyPath
  if ($LASTEXITCODE -eq 0) {
    throw "Recovery material $keyPath must enter private Gogs and cannot be Git ignored."
  }
}

$publicKeyText = [System.IO.File]::ReadAllText($publicKeyPath).Trim()
$signingPassword = [System.IO.File]::ReadAllText($privateKeyPasswordPath).Trim()
if ([string]::IsNullOrWhiteSpace($signingPassword)) {
  throw "The local signing-key password file is empty: $privateKeyPasswordPath"
}
$expectedPublicKey = $publicKeyText
$tauriConfig = Get-Content -LiteralPath $tauriConfigPath -Raw | ConvertFrom-Json

if ($tauriConfig.plugins.updater.pubkey -ne $expectedPublicKey) {
  throw "The public key in src-tauri/tauri.conf.json does not match the local recovery key."
}

Test-TauriUpdaterSigningMaterial -KeyPath $privateKeyPath -Password $signingPassword

if ($CheckOnly) {
  Write-Output "Tauri updater signing configuration is consistent for $Repository. Active recovery files must be committed to private Gogs."
  exit 0
}

Set-GitHubActionsSecretFromFile -SecretName "TAURI_SIGNING_PRIVATE_KEY" -SecretFilePath $privateKeyPath -TargetRepository $Repository
Set-GitHubActionsSecretFromFile -SecretName "TAURI_SIGNING_PRIVATE_KEY_PASSWORD" -SecretFilePath $privateKeyPasswordPath -TargetRepository $Repository

$secretNames = & gh secret list --repo $Repository --app actions --json name --jq '.[].name'
if ($LASTEXITCODE -ne 0 -or $secretNames -notcontains "TAURI_SIGNING_PRIVATE_KEY" -or $secretNames -notcontains "TAURI_SIGNING_PRIVATE_KEY_PASSWORD") {
  throw "The signing secrets were written but could not be verified."
}

Write-Output "Tauri updater signing secrets are configured for $Repository. Active recovery files must be committed to private Gogs."
