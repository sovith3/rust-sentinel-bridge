<#
.SYNOPSIS
    Completely removes Rust Sentinel Bridge from this Windows account.

.DESCRIPTION
    Version-agnostic uninstaller. It does not care which release you installed:
    it stops any running host process, deletes every HKCU/HKLM native messaging
    registration that points at this project (including registrations written by
    older builds under a different folder or host name), and removes the install
    folder(s) of the current and legacy layouts.

    Nothing needs administrator rights. The HKLM branch is only touched when the
    script is started elevated, so a normal user run is a pure HKCU cleanup.

.PARAMETER InstallDir
    Extra folder to remove, on top of the known layouts. Useful when a previous
    version was installed somewhere custom.

.PARAMETER KeepLog
    Copy bridge.log to your Desktop before the install folder is deleted.

.PARAMETER DryRun
    Show what would be removed; touch nothing.

.PARAMETER Force
    Delete the install folder even when it does not look like a bridge folder.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\uninstall.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -DryRun

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -InstallDir "D:\Tools\RustSentinelBridge" -KeepLog
#>
[CmdletBinding()]
param(
    [string]$InstallDir,
    [switch]$KeepLog,
    [switch]$DryRun,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Show a single readable line instead of a PowerShell stack trace.
trap {
    Write-Host ""
    Write-Host "Uninstall failed: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "Docs: https://github.com/sovith3/rust-sentinel-bridge" -ForegroundColor DarkGray
    exit 1
}

$HostName = "com.rustsentinel.bridge"
$ExeName = "rust-sentinel-bridge.exe"
$ManifestName = "$HostName.json"
$ProjectUrl = "https://github.com/sovith3/rust-sentinel-bridge"

# Anything that looks like this project: current name, legacy folder names.
$ProjectMarker = "(?i)rust[-_ .]?sentinel[-_ .]?bridge|rustsentinel"

# Every Chromium family that honours the native messaging registry layout.
# The list stays valid for future versions: keys are discovered, not assumed.
$NativeMessagingRoots = @(
    "Software\Google\Chrome\NativeMessagingHosts",
    "Software\Google\Chrome Beta\NativeMessagingHosts",
    "Software\Google\Chrome Dev\NativeMessagingHosts",
    "Software\Chromium\NativeMessagingHosts",
    "Software\Microsoft\Edge\NativeMessagingHosts",
    "Software\Microsoft\Edge Beta\NativeMessagingHosts",
    "Software\Microsoft\Edge Dev\NativeMessagingHosts",
    "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
    "Software\Vivaldi\NativeMessagingHosts",
    "Software\Opera Software\Opera Stable\NativeMessagingHosts",
    "Software\Opera Software\Opera GX Stable\NativeMessagingHosts",
    "Software\Yandex\YandexBrowser\NativeMessagingHosts"
)

function Write-Step {
    param([string]$Message)
    Write-Host "  $Message" -ForegroundColor DarkGray
}

function Test-IsAdmin {
    try {
        $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
        return $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
    } catch {
        return $false
    }
}

# ------------------------------------------------------------------- processes --

function Stop-HostProcess {
    $stopped = 0
    foreach ($process in @(Get-Process -Name "rust-sentinel-bridge" -ErrorAction SilentlyContinue)) {
        if (-not $process) {
            continue
        }
        if ($DryRun) {
            Write-Step "would stop PID $($process.Id) ($ExeName)"
            continue
        }
        try {
            Stop-Process -Id $process.Id -Force -ErrorAction Stop
            $stopped += 1
            Write-Step "stopped PID $($process.Id) ($ExeName)"
        } catch {
            Write-Step "could not stop PID $($process.Id): $($_.Exception.Message)"
        }
    }
    if ($stopped -gt 0) {
        # Give Windows a moment to release the file handle on the .exe.
        Start-Sleep -Milliseconds 400
    }
    return $stopped
}

# --------------------------------------------------------------- registry keys --

function Remove-Registration {
    param([string]$Hive)

    $hiveRoot = "${Hive}:"
    if (-not (Test-Path -LiteralPath $hiveRoot)) {
        return 0
    }

    $removed = 0
    foreach ($root in $NativeMessagingRoots) {
        $base = "$hiveRoot\$root"
        if (-not (Test-Path -LiteralPath $base)) {
            continue
        }

        $candidates = @()
        foreach ($child in @(Get-ChildItem -LiteralPath $base -ErrorAction SilentlyContinue)) {
            if (-not $child) {
                continue
            }
            $defaultValue = $null
            try {
                $defaultValue = [string]$child.GetValue("")
            } catch {
                $defaultValue = $null
            }

            # Remove the key when the host name matches, when the value points at
            # this project, or when the key name itself looks like a legacy name.
            $isOurs = $false
            if ($child.PSChildName -eq $HostName) {
                $isOurs = $true
            } elseif ($defaultValue -and ($defaultValue -match $ProjectMarker)) {
                $isOurs = $true
            } elseif ($child.PSChildName -match $ProjectMarker) {
                $isOurs = $true
            }

            if ($isOurs) {
                $candidates += $child
            }
        }

        # A registration whose default value cannot be read still has to go:
        # the plain host name is always ours.
        $explicit = Join-Path $base $HostName
        if ($candidates.Count -eq 0 -and (Test-Path -LiteralPath $explicit)) {
            $candidates += Get-Item -LiteralPath $explicit
        }

        foreach ($key in $candidates) {
            $label = $key.Name
            if ($DryRun) {
                Write-Step "would remove registration $label"
                continue
            }
            try {
                Remove-Item -LiteralPath $key.PSPath -Recurse -Force
                $removed += 1
                Write-Step "removed registration $label"
            } catch {
                Write-Step "could not remove $label : $($_.Exception.Message)"
            }
        }
    }
    return $removed
}

# --------------------------------------------------------------------- folders --

function Get-InstallCandidates {
    $roots = @()
    if ($env:LOCALAPPDATA) {
        $roots += @(
            (Join-Path $env:LOCALAPPDATA "RustSentinelBridge"),
            (Join-Path $env:LOCALAPPDATA "rust-sentinel-bridge"),
            (Join-Path $env:LOCALAPPDATA "RustSentinel"),
            (Join-Path $env:LOCALAPPDATA "Programs\RustSentinelBridge")
        )
    }
    if ($env:APPDATA) {
        $roots += (Join-Path $env:APPDATA "RustSentinelBridge")
    }
    if ($env:USERPROFILE) {
        $roots += (Join-Path $env:USERPROFILE "RustSentinelBridge")
    }
    if ($env:ProgramData) {
        $roots += (Join-Path $env:ProgramData "RustSentinelBridge")
    }
    if ($env:ProgramFiles) {
        $roots += (Join-Path $env:ProgramFiles "RustSentinelBridge")
    }
    if (${env:ProgramFiles(x86)}) {
        $roots += (Join-Path ${env:ProgramFiles(x86)} "RustSentinelBridge")
    }
    if ($InstallDir) {
        $roots += $InstallDir
    }
    return $roots
}

function Test-LooksLikeBridgeFolder {
    param([string]$Path)
    foreach ($name in @($ExeName, $ManifestName, "extension_id.txt", "bridge.log")) {
        if (Test-Path -LiteralPath (Join-Path $Path $name)) {
            return $true
        }
    }
    return $false
}

Add-Type -Namespace RustSentinel -Name NativeMethods -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
public static extern bool MoveFileEx(string existingFileName, string newFileName, int flags);
'@ -ErrorAction SilentlyContinue

function Remove-Folder {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }
    if (-not ($Force -or (Test-LooksLikeBridgeFolder -Path $Path))) {
        Write-Step "skipped $Path (no bridge files inside; use -Force to delete anyway)"
        return $false
    }
    if ($DryRun) {
        Write-Step "would delete $Path"
        return $true
    }

    for ($attempt = 1; $attempt -le 4; $attempt += 1) {
        try {
            Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            Write-Step "deleted $Path"
            return $true
        } catch {
            # A lingering host process or an antivirus scan can hold the .exe.
            Stop-HostProcess | Out-Null
            Start-Sleep -Milliseconds (250 * $attempt)
        }
    }

    # Last resort: let Windows delete the folder during the next boot.
    try {
        [RustSentinel.NativeMethods]::MoveFileEx($Path, $null, 4) | Out-Null
        Write-Step "$Path is locked; scheduled for deletion on the next reboot"
    } catch {
        Write-Step "could not delete $Path : $($_.Exception.Message)"
    }
    return $false
}

# ---------------------------------------------------------------- entry point --

if ($env:OS -ne "Windows_NT") {
    throw "This uninstaller targets Windows 10/11."
}

$isAdmin = Test-IsAdmin

Write-Host ""
Write-Host "Rust Sentinel Bridge - uninstall (any version)" -ForegroundColor Cyan
Write-Host ""
if ($DryRun) {
    Write-Host "Dry run: nothing is deleted." -ForegroundColor Yellow
    Write-Host ""
}

$stopped = Stop-HostProcess

$removedKeys = 0
$removedKeys += Remove-Registration -Hive "HKCU"
if ($isAdmin) {
    $removedKeys += Remove-Registration -Hive "HKLM"
} else {
    Write-Step "HKLM skipped (start an elevated shell to clean machine-wide keys)"
}

$folders = @()
$seen = @{}
foreach ($candidate in Get-InstallCandidates) {
    if (-not $candidate) {
        continue
    }
    try {
        $full = [System.IO.Path]::GetFullPath($candidate)
    } catch {
        continue
    }
    $normalized = $full.TrimEnd('\').ToLowerInvariant()
    if ($seen.ContainsKey($normalized)) {
        continue
    }
    $seen[$normalized] = $true
    $folders += $full
}

$removedFolders = 0
foreach ($folder in $folders) {
    if (-not (Test-Path -LiteralPath $folder -PathType Container)) {
        continue
    }

    if ($KeepLog -and -not $DryRun) {
        $log = Join-Path $folder "bridge.log"
        if (Test-Path -LiteralPath $log -PathType Leaf) {
            $desktop = [Environment]::GetFolderPath("Desktop")
            if (-not $desktop) {
                $desktop = $env:TEMP
            }
            $target = Join-Path $desktop "rust-sentinel-bridge.log"
            Copy-Item -LiteralPath $log -Destination $target -Force
            Write-Step "kept the log at $target"
        }
    }

    if (Remove-Folder -Path $folder) {
        $removedFolders += 1
    }
}

# --------------------------------------------------------------------- recap ---

$leftovers = @()
foreach ($folder in $folders) {
    if (Test-Path -LiteralPath $folder) {
        $leftovers += $folder
    }
}
foreach ($root in $NativeMessagingRoots) {
    $key = "HKCU:\$root\$HostName"
    if (Test-Path -LiteralPath $key) {
        $leftovers += $key
    }
}

Write-Host ""
if ($DryRun) {
    Write-Host "Dry run finished. No registration and no folder was touched." -ForegroundColor Yellow
} else {
    Write-Host "Rust Sentinel Bridge removed." -ForegroundColor Green -NoNewline
    Write-Host (" ({0} registry key(s), {1} folder(s), {2} process(es))" -f $removedKeys, $removedFolders, $stopped)
}

if (-not $DryRun -and $leftovers.Count -gt 0) {
    Write-Host ""
    Write-Host "Still present (locked files or elevated keys - a reboot finishes the job):" -ForegroundColor Yellow
    foreach ($item in $leftovers) {
        Write-Host "  $item" -ForegroundColor DarkGray
    }
}

Write-Host ""
Write-Host "Final step (manual, two clicks):" -ForegroundColor Cyan
Write-Host "  1. Open chrome://extensions (or edge://extensions)."
Write-Host "  2. Click 'Remove' on the Rust Sentinel Bridge card."
Write-Host ""
Write-Host "Reinstall any time with install.ps1.  Docs: $ProjectUrl"
Write-Host ""

exit 0
