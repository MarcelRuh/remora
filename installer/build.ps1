# Baut die Release-EXE und daraus den Installer (target\installer\Remora-RDP-Setup-<Version>-x64.exe).
#
#   powershell -ExecutionPolicy Bypass -File installer\build.ps1 [-SkipBuild]
#
# Voraussetzungen: Rust/MSVC (siehe README), CMake im PATH (Audio), Inno Setup 6
# (winget install JRSoftware.InnoSetup).
param([switch]$SkipBuild)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if (-not $SkipBuild) {
    # CMake der VS Build Tools ergänzen, falls keins im PATH ist
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        $vsCmake = Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\*\*\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin" -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($vsCmake) { $env:PATH = "$($vsCmake.FullName);$env:PATH" }
    }
    Push-Location $root
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release fehlgeschlagen" }
    } finally { Pop-Location }
}

$iscc = @(
    (Get-Command iscc -ErrorAction SilentlyContinue).Source,
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 nicht gefunden (winget install JRSoftware.InnoSetup)" }

& $iscc /Q "$PSScriptRoot\remora.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC fehlgeschlagen ($LASTEXITCODE)" }
Get-ChildItem "$root\target\installer\*.exe" | Sort-Object LastWriteTime | Select-Object -Last 1 |
    ForEach-Object { "Installer: $($_.FullName) ($([math]::Round($_.Length / 1MB, 1)) MB)" }
