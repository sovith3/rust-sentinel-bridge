<#
.SYNOPSIS
    Registers Rust Sentinel Bridge as a Chrome/Edge (Chromium) native messaging host.

.DESCRIPTION
    Copies rust-sentinel-bridge.exe into %LOCALAPPDATA%\RustSentinelBridge, writes the
    native messaging manifest, stores the trusted extension id and registers the host
    under HKCU for every Chromium browser it finds. No administrator rights required.

.PARAMETER ExtensionId
    The 32-character id shown by chrome://extensions. If omitted, the script reuses a
    previously saved id or asks for one.

.PARAMETER ExePath
    Path to rust-sentinel-bridge.exe. Defaults to a copy next to this script, then to
    the cargo release output of this repository.

.PARAMETER InstallDir
    Target folder. Defaults to %LOCALAPPDATA%\RustSentinelBridge.

.PARAMETER Uninstall
    Removes the registry keys and the install folder.

.PARAMETER DryRun
    Copies the files and writes the manifest, but skips the HKCU registration (useful to
    verify the layout before touching the real browser configuration).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\install.ps1

.EXAMPLE
    .\install.ps1 -ExtensionId abcdefghijklmnopabcdefghijklmnop -ExePath .\target\release\rust-sentinel-bridge.exe
#>
[CmdletBinding()]
param(
    [string]$ExtensionId,
    [string]$ExePath,
    [string]$InstallDir,
    [switch]$Uninstall,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Show a single readable line instead of a PowerShell stack trace.
trap {
    Write-Host ""
    Write-Host "Installation failed: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "Docs: https://github.com/sovith3/rust-sentinel-bridge#quick-start" -ForegroundColor DarkGray
    exit 1
}

$HostName = "com.rustsentinel.bridge"
$ProjectUrl = "https://github.com/sovith3/rust-sentinel-bridge"

# Chromium browsers share the same native messaging scheme; the vendor key only
# exists when that browser has been started at least once.
$BrowserHosts = @(
    @{ Label = "Chrome";   Vendor = "Google";        Path = "Software\Google\Chrome\NativeMessagingHosts\$HostName" },
    @{ Label = "Edge";     Vendor = "Microsoft";     Path = "Software\Microsoft\Edge\NativeMessagingHosts\$HostName" },
    @{ Label = "Brave";    Vendor = "BraveSoftware"; Path = "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\$HostName" },
    @{ Label = "Vivaldi";  Vendor = "Vivaldi";       Path = "Software\Vivaldi\NativeMessagingHosts\$HostName" },
    @{ Label = "Chromium"; Vendor = "Chromium";      Path = "Software\Chromium\NativeMessagingHosts\$HostName" }
)

function Write-Step {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor DarkGray
}

function Resolve-InstallDir {
    param([string]$Requested)
    if ($Requested) {
        return $Requested
    }
    $base = $env:LOCALAPPDATA
    if (-not $base) {
        $base = Join-Path $env:USERPROFILE "AppData\Local"
    }
    return (Join-Path $base "RustSentinelBridge")
}

function Resolve-HostExe {
    param([string]$Requested)

    $candidates = @()
    if ($Requested) {
        $candidates += $Requested
    }
    if ($PSScriptRoot) {
        $candidates += @(
            (Join-Path $PSScriptRoot "rust-sentinel-bridge.exe"),
            (Join-Path $PSScriptRoot "native-host\target\release\rust-sentinel-bridge.exe"),
            (Join-Path $PSScriptRoot "native-host\target\x86_64-pc-windows-msvc\release\rust-sentinel-bridge.exe")
        )
    }
    $candidates += (Join-Path $PWD "rust-sentinel-bridge.exe")
    $candidates += (Join-Path $PWD "target\release\rust-sentinel-bridge.exe")

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    throw "rust-sentinel-bridge.exe not found. Unzip the release bundle first, or pass -ExePath <path>."
}

function Normalize-ExtensionId {
    param([string]$Value)
    if (-not $Value) {
        return $null
    }
    $clean = $Value.Trim()
    $clean = $clean -replace "^chrome-extension://", ""
    $clean = $clean.TrimEnd("/")
    if ($clean -match "^[a-p]{32}$") {
        return $clean
    }
    return $null
}

function Get-SavedExtensionId {
    param([string]$Directory)
    $file = Join-Path $Directory "extension_id.txt"
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        return $null
    }
    $line = Get-Content -LiteralPath $file -TotalCount 1
    return (Normalize-ExtensionId $line)
}

function Write-Utf8NoBom {
    param([string]$Path, [string]$Content)
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Remove-Host {
    param([string]$Directory)

    $removed = 0
    foreach ($browser in $BrowserHosts) {
        $key = "HKCU:\$($browser.Path)"
        if (Test-Path -LiteralPath $key) {
            Remove-Item -LiteralPath $key -Recurse -Force
            Write-Step "removed $($browser.Label) registration"
            $removed += 1
        }
    }
    if (Test-Path -LiteralPath $Directory) {
        Remove-Item -LiteralPath $Directory -Recurse -Force
        Write-Step "deleted $Directory"
    }

    Write-Host ""
    Write-Host "Rust Sentinel Bridge uninstalled ($removed browser keys removed)." -ForegroundColor Green
    Write-Host "Remove the unpacked extension from chrome://extensions as well." -ForegroundColor DarkGray
}

# ---------------------------------------------------------------- entry point --

if ($env:OS -ne "Windows_NT") {
    throw "This installer targets Windows 10/11."
}

$installDir = Resolve-InstallDir -Requested $InstallDir

if ($Uninstall) {
    Write-Host ""
    Write-Host "Rust Sentinel Bridge - uninstall" -ForegroundColor Cyan
    Remove-Host -Directory $installDir
    exit 0
}

Write-Host ""
Write-Host "Rust Sentinel Bridge - native messaging installer" -ForegroundColor Cyan
Write-Host ""

$sourceExe = Resolve-HostExe -Requested $ExePath
$targetExe = Join-Path $installDir "rust-sentinel-bridge.exe"

$savedId = $null
if (Test-Path -LiteralPath $installDir -PathType Container) {
    $savedId = Get-SavedExtensionId -Directory $installDir
}

if (-not $ExtensionId) {
    if ($savedId) {
        Write-Step "reusing saved extension id $savedId"
        $ExtensionId = $savedId
    } else {
        Write-Host "Open chrome://extensions, enable Developer mode and load the unpacked" -ForegroundColor Yellow
        Write-Host "extension folder. The card shows a 32-character id (letters a-p only)." -ForegroundColor Yellow
        Write-Host ""
        $ExtensionId = Read-Host "Paste the extension id"
    }
}

$normalized = Normalize-ExtensionId $ExtensionId
if (-not $normalized) {
    throw "Invalid extension id '$ExtensionId'. Expected 32 characters in the range a-p (paste either the id or chrome-extension://<id>/)."
}
$ExtensionId = $normalized

# -------------------------------------------------------------------- install --

New-Item -ItemType Directory -Force -Path $installDir | Out-Null

$sourceFull = [System.IO.Path]::GetFullPath($sourceExe)
$targetFull = [System.IO.Path]::GetFullPath($targetExe)
if ($sourceFull -ne $targetFull) {
    try {
        Copy-Item -LiteralPath $sourceFull -Destination $targetFull -Force
    } catch {
        throw "Cannot copy the host binary to $targetFull. Close every browser that uses the bridge and run this script again. Details: $($_.Exception.Message)"
    }
    Write-Step "installed $targetFull"
} else {
    Write-Step "binary already in place at $targetFull"
}

$manifestPath = Join-Path $installDir "$HostName.json"
$manifest = [ordered]@{
    name            = $HostName
    description     = "Rust Sentinel Bridge native messaging host (Apache-2.0)."
    path            = $targetFull
    type            = "stdio"
    allowed_origins = @("chrome-extension://$ExtensionId/")
}
$manifestJson = $manifest | ConvertTo-Json -Depth 5
Write-Utf8NoBom -Path $manifestPath -Content $manifestJson
Write-Step "wrote $manifestPath"

Write-Utf8NoBom -Path (Join-Path $installDir "extension_id.txt") -Content "$ExtensionId`r`n"
Write-Step "trusted extension id $ExtensionId"

$registered = @()
foreach ($browser in $BrowserHosts) {
    $vendorKey = "HKCU:\Software\$($browser.Vendor)"
    $isPrimary = $browser.Label -eq "Chrome" -or $browser.Label -eq "Edge"
    if (-not $isPrimary -and -not (Test-Path -LiteralPath $vendorKey)) {
        continue
    }

    $key = "HKCU:\$($browser.Path)"
    if (-not $DryRun) {
        New-Item -Path $key -Force | Out-Null
        Set-ItemProperty -Path $key -Name "(Default)" -Value $manifestPath
    }
    $registered += $browser.Label
    if ($DryRun) {
        Write-Step "would register $($browser.Label) (HKCU)"
    } else {
        Write-Step "registered $($browser.Label) (HKCU)"
    }
}

if ($registered.Count -eq 0) {
    throw "No browser registry key could be written."
}

# --------------------------------------------------------------------- recap ---

Write-Host ""
Write-Host "Done. " -ForegroundColor Green -NoNewline
Write-Host ("Host '{0}' is active for: {1}." -f $HostName, ($registered -join ", "))
if ($DryRun) {
    Write-Host "Dry run: no HKCU key was written and no browser will see this host yet." -ForegroundColor Yellow
}
Write-Host ""
Write-Host "Next steps" -ForegroundColor Cyan
Write-Host "  1. Reload the extension in chrome://extensions (the reload arrow on its card)."
Write-Host "  2. Open the live demo: https://sovith3.github.io/rust-sentinel-bridge/"
Write-Host "  3. If the dot stays amber, check $installDir\bridge.log"
Write-Host ""
Write-Host "Uninstall at any time:  powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall"
Write-Host "Documentation:          $ProjectUrl"
Write-Host ""
