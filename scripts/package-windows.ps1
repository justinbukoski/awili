<#
.SYNOPSIS
  Builds Awili for Windows and packs Awili-<version>-windows-x64.zip.

.DESCRIPTION
  Run in the Windows build VM (or any x64 Windows host with the Rust MSVC
  toolchain, VS Build Tools with the Windows SDK, CMake, and LLVM 20 on
  LIBCLANG_PATH). From the repo root:

      powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1

  The zip holds Awili.exe (version resources and icon are embedded by
  crates\awili-app\build.rs) and README.txt (docs\windows\TESTER-README.txt).
  The zip and a .sha256 file are written to the output folder (default: dist).
  The build is unsigned.
#>
param(
    [string]$OutDir = "dist",
    [switch]$SkipBuild
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# Version: the workspace version (the same one build.rs embeds in the exe).
$cargo = Get-Content (Join-Path $repo "Cargo.toml") -Raw
if ($cargo -notmatch '(?ms)\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
    throw "could not read [workspace.package] version from Cargo.toml"
}
$version = $Matches[1]
Write-Host "Awili $version"

if (-not $SkipBuild) {
    if (-not $env:LIBCLANG_PATH) { Write-Warning "LIBCLANG_PATH is not set (LLVM 20 is needed for the speech build)" }
    # Strip build-machine paths (profile dir, checkout) from the release binary.
    $root = (Resolve-Path "$PSScriptRoot\..").Path
    # CARGO_ENCODED_RUSTFLAGS is 0x1f-separated, so paths with spaces stay whole;
    # it is set fresh and restored, so repeated runs don't accumulate flags.
    $saved = $env:CARGO_ENCODED_RUSTFLAGS
    $env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$($env:USERPROFILE)=~$([char]0x1f)--remap-path-prefix=$root=awili"
    try {
        cargo build --release --locked -p awili-app
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        $env:CARGO_ENCODED_RUSTFLAGS = $saved
    }
}

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" }
$exe = Join-Path $targetDir "release\awili-app.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }
$info = (Get-Item $exe).VersionInfo
if ($info.ProductName -ne "Awili" -or $info.FileVersion -ne $version) {
    throw "the exe's version resources are wrong: ProductName='$($info.ProductName)' FileVersion='$($info.FileVersion)' (expected Awili $version)"
}

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("awili-package-" + [guid]::NewGuid().ToString("N"))
$folder = Join-Path $stage "Awili-$version-windows-x64"
New-Item -ItemType Directory -Force $folder | Out-Null
try {
    Copy-Item $exe (Join-Path $folder "Awili.exe")
    # Notepad-friendly line endings for the README.
    $text = (Get-Content (Join-Path $repo "docs\windows\TESTER-README.txt") -Raw) -replace "`r?`n", "`r`n"
    [System.IO.File]::WriteAllText((Join-Path $folder "README.txt"), $text, (New-Object System.Text.UTF8Encoding($true)))

    New-Item -ItemType Directory -Force $OutDir | Out-Null
    $zip = Join-Path (Resolve-Path $OutDir) "Awili-$version-windows-x64.zip"
    if (Test-Path $zip) { Remove-Item $zip -Force }
    # Not Compress-Archive: Windows PowerShell 5.1 writes backslashes into entry
    # names, which other platforms' unzip tools warn about. Entries use "/".
    Add-Type -AssemblyName System.IO.Compression
    $fs = [System.IO.File]::Open($zip, [System.IO.FileMode]::Create)
    $archive = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($f in Get-ChildItem $folder -Recurse -File) {
            $name = $f.FullName.Substring($stage.Length + 1) -replace '\\', '/'
            $entry = $archive.CreateEntry($name, [System.IO.Compression.CompressionLevel]::Optimal)
            $out = $entry.Open()
            $in = [System.IO.File]::OpenRead($f.FullName)
            try { $in.CopyTo($out) } finally { $in.Dispose(); $out.Dispose() }
        }
    } finally { $archive.Dispose(); $fs.Dispose() }
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    Set-Content -Path "$zip.sha256" -Value "$hash  $(Split-Path $zip -Leaf)" -Encoding ascii
    Write-Host "wrote $zip"
    Write-Host "sha256 $hash"
} finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}
