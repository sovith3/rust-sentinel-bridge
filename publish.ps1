<#
.SYNOPSIS
    Commits the current working tree, tags it and pushes, so the Release workflow
    builds and publishes the Windows binaries automatically.

.DESCRIPTION
    The Release workflow (.github/workflows/release.yml) runs on every "v*" tag:
    it compiles native-host on windows-latest, packs
    rust-sentinel-bridge-<tag>-win64.zip (exe + install.ps1 + uninstall.ps1 + docs),
    rust-sentinel-extension.zip and SHA256SUMS.txt, and publishes the GitHub
    release. This script only has to commit and push the tag.

    It refuses to run when the version in native-host/Cargo.toml, the version in
    native-host/Cargo.lock and extension/manifest.json disagree, or when the tag
    already exists.

.PARAMETER Version
    Version to publish (for example 1.0.1). Defaults to the version already in
    native-host/Cargo.toml. When supplied, Cargo.toml, Cargo.lock and
    extension/manifest.json are bumped first.

.PARAMETER Message
    Commit message. Defaults to "chore(release): v<version>".

.PARAMETER NoPush
    Commit and tag locally, but do not push.

.PARAMETER DryRun
    Print the git commands without running them.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\publish.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\publish.ps1 -Version 1.0.2 -Message "fix: lcu stream"
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$Message,
    [switch]$NoPush,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

trap {
    Write-Host ""
    Write-Host "Publish failed: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

$root = $PSScriptRoot
if (-not $root) {
    $root = (Get-Location).Path
}

function Write-Step {
    param([string]$Text)
    Write-Host "  $Text" -ForegroundColor DarkGray
}

function Invoke-Git {
    param([string[]]$Arguments)
    if ($DryRun) {
        Write-Step ("git " + ($Arguments -join " "))
        return ""
    }
    $output = & git @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw ("git " + ($Arguments -join " ") + " failed:`n" + ($output -join "`n"))
    }
    return ($output -join "`n")
}

function Invoke-GitSoft {
    param([string[]]$Arguments)
    if ($DryRun) {
        Write-Step ("git " + ($Arguments -join " "))
        return ""
    }
    $output = & git @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        return ""
    }
    return ($output -join "`n")
}

$cargoToml = Join-Path $root "native-host\Cargo.toml"
$cargoLock = Join-Path $root "native-host\Cargo.lock"
$manifest = Join-Path $root "extension\manifest.json"

foreach ($required in @($cargoToml, $cargoLock, $manifest, (Join-Path $root "install.ps1"), (Join-Path $root "uninstall.ps1"))) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "missing $required - run this script from the repository root."
    }
}

# ----------------------------------------------------------------- versioning --

$tomlVersion = ([regex]::Match((Get-Content -LiteralPath $cargoToml -Raw), '(?m)^version\s*=\s*"([^"]+)"')).Groups[1].Value
$manifestVersion = ([regex]::Match((Get-Content -LiteralPath $manifest -Raw), '"version"\s*:\s*"([^"]+)"')).Groups[1].Value

if (-not $Version) {
    $Version = $tomlVersion
}
$Version = $Version.Trim().TrimStart("v")

if ($Version -notmatch "^\d+\.\d+\.\d+$") {
    throw "'$Version' is not a semantic version (expected MAJOR.MINOR.PATCH)."
}

$tag = "v$Version"

Write-Host ""
Write-Host "Rust Sentinel Bridge - publish $tag" -ForegroundColor Cyan
Write-Host ""

Set-Location -LiteralPath $root

# Bump the three files that carry the version when it is not the current one.
if ($tomlVersion -ne $Version) {
    if ($DryRun) {
        Write-Step "would set Cargo.toml version to $Version"
    } else {
        $text = Get-Content -LiteralPath $cargoToml -Raw
        $text = [regex]::Replace($text, '(?m)^version\s*=\s*"[^"]+"', "version = `"$Version`"", 1)
        $encoding = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($cargoToml, $text, $encoding)
        Write-Step "Cargo.toml -> $Version"
    }
}
if ($manifestVersion -ne $Version) {
    if ($DryRun) {
        Write-Step "would set extension/manifest.json version to $Version"
    } else {
        $text = Get-Content -LiteralPath $manifest -Raw
        $text = [regex]::Replace($text, '"version"\s*:\s*"[^"]+"', "`"version`": `"$Version`"", 1)
        $encoding = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($manifest, $text, $encoding)
        Write-Step "extension/manifest.json -> $Version"
    }
}

# Cargo.lock must match or "cargo build --locked" fails inside the workflow.
if (-not $DryRun) {
    $lockText = Get-Content -LiteralPath $cargoLock -Raw
    $pattern = '(?s)(name = "rust-sentinel-bridge"\r?\nversion = ")[^"]+(")'
    if ([regex]::IsMatch($lockText, $pattern)) {
        $lockText = [regex]::Replace($lockText, $pattern, "`${1}$Version`${2}", 1)
        $encoding = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($cargoLock, $lockText, $encoding)
        Write-Step "Cargo.lock -> $Version"
    }
}

if (-not $DryRun) {
    $tomlNow = ([regex]::Match((Get-Content -LiteralPath $cargoToml -Raw), '(?m)^version\s*=\s*"([^"]+)"')).Groups[1].Value
    $manifestNow = ([regex]::Match((Get-Content -LiteralPath $manifest -Raw), '"version"\s*:\s*"([^"]+)"')).Groups[1].Value
    if ($tomlNow -ne $Version -or $manifestNow -ne $Version) {
        throw "version mismatch after the bump: Cargo.toml=$tomlNow manifest=$manifestNow expected=$Version"
    }
}

# ----------------------------------------------------------------- git checks --

$insideRepo = Invoke-Git @("rev-parse", "--is-inside-work-tree")
if (-not $DryRun -and $insideRepo.Trim() -ne "true") {
    throw "$root is not a git repository."
}

$branch = "main"
if (-not $DryRun) {
    $current = Invoke-Git @("rev-parse", "--abbrev-ref", "HEAD")
    if ($current.Trim()) {
        $branch = $current.Trim()
    }
}

$existingTag = ""
if (-not $DryRun) {
    $existingTag = Invoke-Git @("tag", "--list", $tag)
    if ($existingTag.Trim()) {
        throw "tag $tag already exists. Use -Version <next> or delete it with 'git tag -d $tag'."
    }
}

# A machine without a global identity would fail at the commit step.
if (-not $DryRun) {
    $name = Invoke-GitSoft @("config", "--get", "user.name")
    if (-not $name.Trim()) {
        Invoke-Git @("config", "user.name", "sovith3") | Out-Null
        Write-Step "set repository user.name to sovith3 (no global identity found)"
    }
    $email = Invoke-GitSoft @("config", "--get", "user.email")
    if (-not $email.Trim()) {
        Invoke-Git @("config", "user.email", "sovith3@users.noreply.github.com") | Out-Null
        Write-Step "set repository user.email to sovith3@users.noreply.github.com"
    }
}

if (-not $Message) {
    $Message = "chore(release): $tag"
}

# ------------------------------------------------------------------- publish --

Invoke-Git @("add", "--all") | Out-Null
Invoke-Git @("commit", "--message", $Message) | Out-Null
Invoke-Git @("tag", "--annotate", $tag, "--message", "Rust Sentinel Bridge $tag") | Out-Null

if ($NoPush) {
    Write-Host ""
    Write-Host "Committed and tagged $tag locally (not pushed)." -ForegroundColor Yellow
    Write-Host "Push it later with:" -ForegroundColor DarkGray
    Write-Host "  git push origin $branch --follow-tags" -ForegroundColor DarkGray
    exit 0
}

if ($DryRun) {
    Write-Host ""
    Write-Host "Dry run finished: nothing was committed, tagged or pushed." -ForegroundColor Yellow
    Write-Host ""
    exit 0
}

Invoke-Git @("push", "origin", $branch, "--follow-tags") | Out-Null

Write-Host ""
Write-Host "Pushed $tag." -ForegroundColor Green
Write-Host ""
Write-Host "The Release workflow now builds the Windows assets (~4 min):" -ForegroundColor Cyan
Write-Host "  https://github.com/sovith3/rust-sentinel-bridge/actions/workflows/release.yml"
Write-Host "The release appears at:" -ForegroundColor Cyan
Write-Host "  https://github.com/sovith3/rust-sentinel-bridge/releases/tag/$tag"
Write-Host ""
Write-Host "If the push asks for credentials, sign in with a GitHub token or Git Credential Manager."
Write-Host ""
