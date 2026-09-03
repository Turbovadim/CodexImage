[CmdletBinding()]
param(
    [switch]$Install,
    [switch]$Open,
    [switch]$Archive
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$Metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) {
    throw "cargo metadata failed"
}
$Package = $Metadata.packages | Where-Object { $_.name -eq "codex-image" } | Select-Object -First 1
if (-not $Package) {
    throw "could not find the codex-image package"
}

cargo build --release --locked
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed"
}

$Architecture = "windows-x86_64"
$Stage = Join-Path $Root "dist/CodexImage-$Architecture"
if (Test-Path $Stage) {
    Remove-Item -Recurse -Force $Stage
}
New-Item -ItemType Directory -Force $Stage | Out-Null
Copy-Item (Join-Path $Root "target/release/codex-image.exe") (Join-Path $Stage "CodexImage.exe")
# Same-run conditioning shells out to this console companion; the app disables
# that step when the companion is missing beside CodexImage.exe.
Copy-Item (Join-Path $Root "target/release/codex-image-condition.exe") $Stage
Copy-Item (Join-Path $Root "README.md") $Stage

$Executable = Join-Path $Stage "CodexImage.exe"
if ($Install) {
    if (-not $env:LOCALAPPDATA) {
        throw "LOCALAPPDATA is unavailable"
    }
    $InstallDirectory = Join-Path $env:LOCALAPPDATA "Programs/CodexImage"
    New-Item -ItemType Directory -Force $InstallDirectory | Out-Null
    Copy-Item $Executable (Join-Path $InstallDirectory "CodexImage.exe") -Force
    Copy-Item (Join-Path $Stage "codex-image-condition.exe") $InstallDirectory -Force

    $StartMenu = Join-Path $env:APPDATA "Microsoft/Windows/Start Menu/Programs"
    $Shell = New-Object -ComObject WScript.Shell
    $Shortcut = $Shell.CreateShortcut((Join-Path $StartMenu "CodexImage.lnk"))
    $Shortcut.TargetPath = Join-Path $InstallDirectory "CodexImage.exe"
    $Shortcut.WorkingDirectory = $InstallDirectory
    $Shortcut.Save()
    $Executable = Join-Path $InstallDirectory "CodexImage.exe"
    Write-Output "Installed CodexImage for the current user at $InstallDirectory"
}

if ($Archive) {
    $ArchiveName = "CodexImage-$($Package.version)-$Architecture.zip"
    $ArchivePath = Join-Path (Join-Path $Root "dist") $ArchiveName
    if (Test-Path $ArchivePath) {
        Remove-Item -Force $ArchivePath
    }
    Compress-Archive -Path (Join-Path $Stage "*") -DestinationPath $ArchivePath -CompressionLevel Optimal
    $Hash = (Get-FileHash -Algorithm SHA256 $ArchivePath).Hash.ToLowerInvariant()
    Set-Content -Encoding ASCII "$ArchivePath.sha256" "$Hash  $ArchiveName"
    Write-Output $ArchivePath
    Write-Output "$ArchivePath.sha256"
} else {
    Write-Output $Stage
}

if ($Open) {
    Start-Process $Executable
}
