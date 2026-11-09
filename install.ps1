# Sincra installer for Windows (PowerShell).
#
#   irm https://raw.githubusercontent.com/alexclaz/sincra/main/install.ps1 | iex
#
# Installs to %LOCALAPPDATA%\Programs\Sincra (GUI, CLI and ffmpeg), adds it to the user PATH
# and creates a Start Menu shortcut. No admin rights needed. Files downloaded this way carry no
# "Mark of the Web", so SmartScreen does not block the unsigned executables.
#
# Environment variables:
#   $env:SINCRA_VERSION = "v0.2.0"   install a specific release instead of the latest
#   $env:SINCRA_UNINSTALL = "1"      remove everything

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is very slow with the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = if ($env:SINCRA_REPO) { $env:SINCRA_REPO } else { "alexclaz/sincra" }
$Dest = Join-Path $env:LOCALAPPDATA "Programs\Sincra"
$Shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "Sincra.lnk"

function Set-UserPath([string]$dir, [bool]$add) {
    $cur = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @(if ($cur) { $cur -split ";" | Where-Object { $_ -and ($_.TrimEnd("\") -ne $dir.TrimEnd("\")) } })
    if ($add) { $parts += $dir }
    [Environment]::SetEnvironmentVariable("Path", ($parts -join ";"), "User")
}

if ($env:SINCRA_UNINSTALL -eq "1") {
    Get-Process sincra-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
    if (Test-Path $Shortcut) { Remove-Item -Force $Shortcut }
    Set-UserPath $Dest $false
    Write-Host "Sincra removed. (audio cache: $env:LOCALAPPDATA\sincra)"
    return
}

if (-not [Environment]::Is64BitOperatingSystem) { throw "Sincra requires 64-bit Windows." }

$Asset = "sincra-windows-x64.zip"
$Url = if ($env:SINCRA_VERSION) { "https://github.com/$Repo/releases/download/$($env:SINCRA_VERSION)/$Asset" }
       else { "https://github.com/$Repo/releases/latest/download/$Asset" }

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("sincra-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Force $Tmp | Out-Null
try {
    if ($env:SINCRA_PACKAGE) {   # local zip (testing)
        Copy-Item $env:SINCRA_PACKAGE (Join-Path $Tmp $Asset)
    } else {
        Write-Host "Downloading $Asset..."
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile (Join-Path $Tmp $Asset)
    }
    Get-Process sincra-gui -ErrorAction SilentlyContinue | Stop-Process -Force
    if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
    New-Item -ItemType Directory -Force $Dest | Out-Null
    Expand-Archive -Path (Join-Path $Tmp $Asset) -DestinationPath $Dest -Force
    Get-ChildItem $Dest -Recurse | Unblock-File
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

$Gui = Join-Path $Dest "sincra-gui.exe"
if (-not (Test-Path $Gui)) { throw "invalid package: $Gui not found" }

$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut($Shortcut)
$lnk.TargetPath = $Gui
$lnk.WorkingDirectory = $Dest
$lnk.Description = "Multicam sync by audio"
$lnk.Save()

Set-UserPath $Dest $true
$env:Path = "$env:Path;$Dest"

Write-Host ""
Write-Host "Installed in $Dest" -ForegroundColor Green
Write-Host "  Interface:     Start menu > Sincra"
Write-Host "  Command line:  sincra `"Timeline 1.xml`"   (open a new terminal)"
Write-Host "  Uninstall:     `$env:SINCRA_UNINSTALL=1; irm https://raw.githubusercontent.com/$Repo/main/install.ps1 | iex"
