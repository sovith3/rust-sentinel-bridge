<#
.SYNOPSIS
    Installs Rust Sentinel Bridge as a Chrome/Edge (Chromium) native messaging host.

.DESCRIPTION
    One command, nothing to unzip:

        irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex

    The script installs the host binary, writes the native messaging manifest, keeps the
    trusted extension id and registers the host for every Chromium browser it finds.
    Everything lives in %LOCALAPPDATA%\RustSentinelBridge and HKCU: no administrator
    rights, no PATH changes, no service, no port.

    When rust-sentinel-bridge.exe is not sitting next to the script - which is what always
    happens with the one-line form above - the binary is downloaded from the latest GitHub
    release and checked against the published SHA-256 before anything is written. When no
    Rust Sentinel Bridge extension is loaded yet, the unpacked extension is downloaded and
    extracted too, so the only manual step left is picking that folder once in
    chrome://extensions.

.PARAMETER ExtensionId
    The 32-character id shown by chrome://extensions. When omitted the script reuses a
    saved id, then looks for an unpacked Rust Sentinel Bridge extension in the Chrome,
    Edge, Brave, Vivaldi and Chromium profiles, and only then asks.

.PARAMETER ExePath
    Path to rust-sentinel-bridge.exe. Defaults to a copy next to this script, then to the
    cargo release output of this repository.

.PARAMETER InstallDir
    Target folder. Defaults to %LOCALAPPDATA%\RustSentinelBridge.

.PARAMETER Uninstall
    Removes the registry keys and the install folder. When uninstall.ps1 ships next to
    this script it is used instead, because it also cleans legacy layouts.

.PARAMETER DryRun
    Prepares the files and the manifest, but skips the HKCU registration (useful to check
    the layout before touching the real browser configuration).

.PARAMETER NoDownload
    Never download anything from GitHub; fail instead when a local binary is missing.

.NOTES
    Environment variables, handy because `irm ... | iex` cannot pass parameters:
    RUST_SENTINEL_EXTENSION_ID (same as -ExtensionId) and RUST_SENTINEL_INSTALL_DIR
    (same as -InstallDir).

.EXAMPLE
    irm https://github.com/sovith3/rust-sentinel-bridge/releases/latest/download/install.ps1 | iex

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\install.ps1 -ExtensionId abcdefghijklmnopabcdefghijklmnop

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\install.ps1 -DryRun -NoDownload
#>
[CmdletBinding()]
param(
    [string]$ExtensionId,
    [string]$ExePath,
    [string]$InstallDir,
    [switch]$Uninstall,
    [switch]$DryRun,
    [switch]$NoDownload
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Piped into PowerShell (irm ... | iex) there is no script path, and `exit` would close
# the user's console window. Errors are reported the same way either way.
$script:FromFile = $false
try {
    $script:FromFile = [bool]$MyInvocation.MyCommand.Path
} catch {
    # Strict mode: a script block has no Path property at all.
    $script:FromFile = $false
}

$HostName = "com.rustsentinel.bridge"
$ExeName = "rust-sentinel-bridge.exe"
$ExtensionZip = "rust-sentinel-extension.zip"
$ProjectUrl = "https://github.com/sovith3/rust-sentinel-bridge"
$ReleaseBase = "$ProjectUrl/releases/latest/download"
$InstallUrl = "$ReleaseBase/install.ps1"
$script:PromptedForExtension = $false
$script:DownloadedExe = $null
$UninstallUrl = "$ReleaseBase/uninstall.ps1"

# Windows PowerShell 5.1 may still default to TLS 1.0, which GitHub rejects.
try {
    $previousTls = [Net.ServicePointManager]::SecurityProtocol
    [Net.ServicePointManager]::SecurityProtocol = $previousTls -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Older runtimes ignore this; the download then fails with a clear message.
}

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
    if ($env:RUST_SENTINEL_INSTALL_DIR) {
        return $env:RUST_SENTINEL_INSTALL_DIR
    }
    $base = $env:LOCALAPPDATA
    if (-not $base) {
        $base = Join-Path $env:USERPROFILE "AppData\Local"
    }
    return (Join-Path $base "RustSentinelBridge")
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

# ------------------------------------------------------------ release download --

$script:SumsLoaded = $false
$script:Sums = @{}

function Get-PublishedSums {
    if ($script:SumsLoaded) {
        return
    }
    $script:SumsLoaded = $true
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ("rsb-sums-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8) + ".txt")
    $previous = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri "$ReleaseBase/SHA256SUMS.txt" -OutFile $temp -UseBasicParsing -ErrorAction Stop
        $map = @{}
        foreach ($line in (Get-Content -LiteralPath $temp)) {
            if ($line -match "^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$") {
                $map[$Matches[2]] = $Matches[1].ToLowerInvariant()
            }
        }
        $script:Sums = $map
    } catch {
        $script:Sums = @{}
    } finally {
        $ProgressPreference = $previous
        Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    }
}

function Assert-Checksum {
    param([string]$Path, [string]$Name)
    Get-PublishedSums
    $expected = $null
    if ($script:Sums.ContainsKey($Name)) {
        $expected = $script:Sums[$Name]
    }
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expected) {
        if ($actual -ne $expected) {
            Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
            throw "SHA-256 mismatch for $Name (expected $expected, got $actual). The download was discarded; try again."
        }
        Write-Step "sha-256 verified ($($actual.Substring(0, 12))...) $Name"
    } else {
        Write-Host "  warning: could not verify $Name (SHA256SUMS.txt unavailable)" -ForegroundColor Yellow
    }
}

function Save-ReleaseAsset {
    param([string]$Name, [string]$Destination)
    if ($NoDownload) {
        throw "$Name is not available locally and -NoDownload was given."
    }
    $url = "$ReleaseBase/$Name"
    Write-Step "downloading $Name"
    $previous = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri $url -OutFile $Destination -UseBasicParsing -ErrorAction Stop
    } catch {
        throw ("Could not download $url. Check your connection, or install offline from the " +
            "release zip. Details: $($_.Exception.Message)")
    } finally {
        $ProgressPreference = $previous
    }
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        throw "The download of $Name produced no file."
    }
    Assert-Checksum -Path $Destination -Name $Name
}

function Save-LatestHost {
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ("rsb-host-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $temp | Out-Null
    $target = Join-Path $temp $ExeName
    Save-ReleaseAsset -Name $ExeName -Destination $target
    $script:DownloadedExe = $target
    return $target
}

function Install-ExtensionFolder {
    param([string]$Directory)
    $target = Join-Path $Directory "extension"
    if (Test-Path -LiteralPath (Join-Path $target "manifest.json") -PathType Leaf) {
        Write-Step "extension already extracted at $target"
        return $target
    }
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ("rsb-ext-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8) + ".zip")
    try {
        Save-ReleaseAsset -Name $ExtensionZip -Destination $temp
        if (Test-Path -LiteralPath $target) {
            Remove-Item -LiteralPath $target -Recurse -Force
        }
        New-Item -ItemType Directory -Force -Path $target | Out-Null
        $progress = $ProgressPreference
        $ProgressPreference = "SilentlyContinue"
        try {
            Expand-Archive -LiteralPath $temp -DestinationPath $target -Force
        } finally {
            $ProgressPreference = $progress
        }
        if (-not (Test-Path -LiteralPath (Join-Path $target "manifest.json") -PathType Leaf)) {
            throw "The extension package did not contain manifest.json."
        }
        Write-Step "extension extracted to $target"
    } finally {
        Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    }
    return $target
}

# --------------------------------------------------------- extension id ----

function Show-ExtensionInstructions {
    param([string]$Folder)
    $script:PromptedForExtension = $true
    Write-Host ""
    Write-Host "Step 1 - load the extension (one time only)" -ForegroundColor Cyan
    Write-Host "  1. Open  chrome://extensions      (Edge: edge://extensions)"
    Write-Host "  2. Turn on  Developer mode  (top-right switch)"
    Write-Host "  3. Click  Load unpacked  and select this folder:"
    Write-Host "       $Folder" -ForegroundColor White
    Write-Host "     Already loaded from another folder? Keep it and paste its id below."
    Write-Host "  4. Copy the 32-character id shown on the new card (letters a-p only)"
    Write-Host ""
}

function Resolve-ExtensionId {
    param([string]$Requested, [string]$Directory)

    if ($Requested) {
        $explicit = Normalize-ExtensionId $Requested
        if (-not $explicit) {
            throw ("Invalid extension id '$Requested'. Expected 32 characters in the range " +
                "a-p (paste the id or chrome-extension://<id>/).")
        }
        Write-Step "using the extension id from the command line"
        return $explicit
    }

    if ($env:RUST_SENTINEL_EXTENSION_ID) {
        $fromEnvironment = Normalize-ExtensionId $env:RUST_SENTINEL_EXTENSION_ID
        if (-not $fromEnvironment) {
            throw "RUST_SENTINEL_EXTENSION_ID is not a 32-character id (range a-p)."
        }
        Write-Step "using RUST_SENTINEL_EXTENSION_ID"
        return $fromEnvironment
    }

    $saved = Get-SavedExtensionId -Directory $Directory
    if ($saved) {
        Write-Step "reusing the id saved by the previous install ($saved)"
        return $saved
    }

    $folder = Install-ExtensionFolder -Directory $Directory
    Show-ExtensionInstructions -Folder $folder

    while ($true) {
        $answer = Read-Host "Paste the extension id (Enter to cancel)"
        if (-not $answer) {
            return $null
        }
        $normalized = Normalize-ExtensionId $answer
        if ($normalized) {
            return $normalized
        }
        Write-Host "  That is not a 32-character id (letters a-p only). Try again." -ForegroundColor Yellow
    }
}

function Resolve-HostExe {
    param([string]$Requested)

    if ($Requested) {
        if (Test-Path -LiteralPath $Requested -PathType Leaf) {
            return (Resolve-Path -LiteralPath $Requested).Path
        }
        throw "No binary at '$Requested' (-ExePath)."
    }

    $candidates = @()
    if ($PSScriptRoot) {
        $candidates += @(
            (Join-Path $PSScriptRoot $ExeName),
            (Join-Path $PSScriptRoot "native-host\target\release\$ExeName"),
            (Join-Path $PSScriptRoot "native-host\target\x86_64-pc-windows-msvc\release\$ExeName")
        )
    }
    $candidates += (Join-Path $PWD $ExeName)
    $candidates += (Join-Path $PWD "target\release\$ExeName")

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    Write-Step "no local binary next to the script, using the latest release"
    return (Save-LatestHost)
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

function Invoke-Uninstall {
    param([string]$Directory)

    # uninstall.ps1 knows every legacy layout and browser key, so it is preferred.
    # It runs as a separate process so its exit code cannot kill this session.
    if ($PSScriptRoot) {
        $standalone = Join-Path $PSScriptRoot "uninstall.ps1"
        if (Test-Path -LiteralPath $standalone -PathType Leaf) {
            $arguments = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $standalone, "-InstallDir", $Directory)
            if ($DryRun) {
                $arguments += "-DryRun"
            }
            & powershell.exe @arguments
            if ($LASTEXITCODE -ne 0) {
                throw "uninstall.ps1 reported exit code $LASTEXITCODE."
            }
            return
        }
    }

    Remove-Host -Directory $Directory
}

# ------------------------------------------------------------------- install --

function Invoke-Installer {

    if ($env:OS -ne "Windows_NT") {
        throw "This installer targets Windows 10/11."
    }

    $directory = Resolve-InstallDir -Requested $InstallDir

    if ($Uninstall) {
        Write-Host ""
        Write-Host "Rust Sentinel Bridge - uninstall" -ForegroundColor Cyan
        Invoke-Uninstall -Directory $directory
        return
    }

    Write-Host ""
    Write-Host "Rust Sentinel Bridge - installer" -ForegroundColor Cyan
    Write-Host "  no admin rights, no port, no service - files go to $directory" -ForegroundColor DarkGray
    Write-Host ""

    $sourceExe = Resolve-HostExe -Requested $ExePath
    $trustedId = Resolve-ExtensionId -Requested $ExtensionId -Directory $directory
    if (-not $trustedId) {
        Write-Host "Cancelled - nothing was changed." -ForegroundColor Yellow
        Write-Host "Load the extension, then run the same command again:" -ForegroundColor DarkGray
        Write-Host "  irm $InstallUrl | iex" -ForegroundColor DarkGray
        Write-Host ""
        return
    }

    Write-Host ""
    if ($script:PromptedForExtension) {
        Write-Host "Step 2 - register the native host" -ForegroundColor Cyan
    } else {
        Write-Host "Registering the native host" -ForegroundColor Cyan
    }

    New-Item -ItemType Directory -Force -Path $directory | Out-Null

    $targetExe = Join-Path $directory $ExeName
    $sourceFull = [System.IO.Path]::GetFullPath($sourceExe)
    $targetFull = [System.IO.Path]::GetFullPath($targetExe)
    if ($sourceFull -ne $targetFull) {
        try {
            Copy-Item -LiteralPath $sourceFull -Destination $targetFull -Force
        } catch {
            throw ("Cannot copy the host binary to $targetFull. Close every browser that uses " +
                "the bridge and run this script again. Details: $($_.Exception.Message)")
        }
        Write-Step "installed $targetFull"
    } else {
        Write-Step "binary already in place at $targetFull"
    }

    # The downloaded copy lives in %TEMP%; the install folder has the real one now.
    if ($script:DownloadedExe -and (Test-Path -LiteralPath $script:DownloadedExe)) {
        $downloadDir = Split-Path -Parent $script:DownloadedExe
        Remove-Item -LiteralPath $downloadDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    $manifestPath = Join-Path $directory "$HostName.json"
    $manifest = [ordered]@{
        name            = $HostName
        description     = "Rust Sentinel Bridge native messaging host (Apache-2.0)."
        path            = $targetFull
        type            = "stdio"
        allowed_origins = @("chrome-extension://$trustedId/")
    }
    Write-Utf8NoBom -Path $manifestPath -Content (($manifest | ConvertTo-Json -Depth 5))
    Write-Step "wrote $manifestPath"

    Write-Utf8NoBom -Path (Join-Path $directory "extension_id.txt") -Content "$trustedId`r`n"
    Write-Step "trusted extension id $trustedId"

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

    Write-Host ""
    Write-Host "Done. " -ForegroundColor Green -NoNewline
    Write-Host ("Host '{0}' is active for: {1}." -f $HostName, ($registered -join ", "))
    if ($DryRun) {
        Write-Host "Dry run: no HKCU key was written and no browser can see the host yet." -ForegroundColor Yellow
    }
    Write-Host ""
    Write-Host "Files" -ForegroundColor Cyan
    Write-Host "  $targetFull"
    Write-Host "  $manifestPath"
    Write-Host "  $(Join-Path $directory 'extension_id.txt')"
    Write-Host ""
    Write-Host "Next steps" -ForegroundColor Cyan
    Write-Host "  1. Click the reload arrow on the extension card in chrome://extensions."
    Write-Host "  2. Open the live demo: https://sovith3.github.io/rust-sentinel-bridge/"
    Write-Host "     The badge turns green when the host answers a PING."
    Write-Host "  3. If the badge stays amber, read $(Join-Path $directory 'bridge.log')."
    Write-Host ""
    Write-Host "Uninstall at any time" -ForegroundColor Cyan
    Write-Host "  irm $UninstallUrl | iex"
    Write-Host ""
    Write-Host "Docs: $ProjectUrl#quick-start" -ForegroundColor DarkGray
    Write-Host ""
}

# --------------------------------------------------------------- entry point --

$exitCode = 0
try {
    Invoke-Installer
} catch {
    Write-Host ""
    Write-Host "Installation failed: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "Docs: $ProjectUrl#quick-start" -ForegroundColor DarkGray
    Write-Host ""
    $exitCode = 1
}

if ($script:FromFile) {
    exit $exitCode
}
