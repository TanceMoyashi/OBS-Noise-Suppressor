#requires -Version 5.1
<#
.SYNOPSIS
    OBS Noise Suppressor (OBS 版) をビルドする。

.DESCRIPTION
    Rust 側は MSVC のリンカが要るので、先に vcvars64.bat を読んでから cargo を回す。
    `-Tests` を付けると `cargo test --lib` を回す。テスト実行ファイルは obs.dll を
    インポートしているので、PATH に OBS の bin\64bit を通しておく。

.PARAMETER Configuration
    Debug か Release（既定 Release）。

.PARAMETER Tests
    ビルドの代わりにテストを回す。

.PARAMETER TestFilter
    `cargo test <filter>` のフィルタ（例: dsp::offline_tests / ui::）。

.PARAMETER NoCapture
    テストの出力を表示する（`--nocapture`）。

.EXAMPLE
    .\build.ps1
    .\build.ps1 -Tests -TestFilter dsp::offline_tests -NoCapture
#>
[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [switch]$Tests,
    [string]$TestFilter = "",
    [switch]$NoCapture
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot

function Find-VcVars {
    $candidates = New-Object System.Collections.Generic.List[string]

    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere) {
        $vs = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        if ($vs) {
            $candidates.Add((Join-Path $vs "VC\Auxiliary\Build\vcvars64.bat"))
        }
    }

    foreach ($v in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if (-not $v) { continue }
        Get-ChildItem -Path (Join-Path $v "Microsoft Visual Studio") -Filter "vcvars64.bat" `
            -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\VC\\Auxiliary\\Build\\' } |
            ForEach-Object { $candidates.Add($_.FullName) }
    }

    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) { return $c }
    }
    return $null
}

function Find-Cargo {
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $guess = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (Test-Path -LiteralPath $guess) { return $guess }
    throw "cargo が見つかりません。Rust (MSVC ツールチェーン) を入れてください。"
}

$vcvars = Find-VcVars
if (-not $vcvars) {
    throw "vcvars64.bat が見つかりません。Visual Studio の C++ ビルドツールを入れてください。"
}
$cargo = Find-Cargo

# テスト実行ファイルは obs.dll をインポートするため、OBS の bin\64bit を PATH に足す。
$obsBin = "C:\Program Files\obs-studio\bin\64bit"
if (Test-Path -LiteralPath $obsBin) {
    $env:Path = "$obsBin;$env:Path"
}

if ($Tests) {
    # テスト用のプラグイン名は lib 名 + ".dll"。
    $cargoArgs = @("test")
    if ($Configuration -eq "Release") { $cargoArgs += "--release" }
    $cargoArgs += @("--lib")
    if ($TestFilter) { $cargoArgs += $TestFilter }
    $cargoArgs += @("--")
    if ($NoCapture) { $cargoArgs += "--nocapture" }

    $cargoLine = ($cargoArgs | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join " "

    # DSP コアは別クレート（VST3 版と共有）なので、そちらのテストも回す。
    $targets = @($root)
    $dspRoot = Join-Path (Split-Path -Parent $root) "dpdfnet"
    if (Test-Path -LiteralPath (Join-Path $dspRoot "Cargo.toml")) {
        $targets += $dspRoot
    }
    # パネル UI も別クレート（VST3 版と共有）。テストはそちらに移してある。
    $panelRoot = Join-Path (Split-Path -Parent $root) "panel_ui"
    if (Test-Path -LiteralPath (Join-Path $panelRoot "Cargo.toml")) {
        $targets += $panelRoot
    }

    foreach ($t in $targets) {
        Write-Host "==> $cargo $cargoLine" -ForegroundColor Cyan
        Write-Host "    (in $t)" -ForegroundColor DarkGray
        $cmd = "`"$vcvars`" >nul 2>&1 && cd /d `"$t`" && `"$cargo`" $cargoLine"
        & cmd.exe /c $cmd
        $code = $LASTEXITCODE
        if ($code -ne 0) {
            throw "cargo が失敗しました (exit $code): $t"
        }
    }
} else {
    $cargoArgs = @("build")
    if ($Configuration -eq "Release") { $cargoArgs += "--release" }

    $cargoLine = ($cargoArgs | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join " "
    Write-Host "==> $cargo $cargoLine" -ForegroundColor Cyan

    $cmd = "`"$vcvars`" >nul 2>&1 && cd /d `"$root`" && `"$cargo`" $cargoLine"
    & cmd.exe /c $cmd
    $code = $LASTEXITCODE

    if ($code -ne 0) {
        throw "cargo が失敗しました (exit $code)"
    }
}
