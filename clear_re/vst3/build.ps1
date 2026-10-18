#requires -Version 5.1
<#
.SYNOPSIS
    DPDFNet Voice (VST3 版) をビルドする。

.DESCRIPTION
    OBS 版と同じく MSVC のリンカが要るので、先に vcvars64.bat を読んでから
    cargo を回す。初回は nih-plug (git) と vst3-sys を取ってくるため
    ネットワークが要る（2 回目以降はキャッシュされる）。

    出力は `target\<Configuration>\dpdfnet_voice.dll`。VST3 として使うには
    `<好きな名前>.vst3` にリネームして VST3 フォルダに置く（配布用は
    `installer\install-vst3.ps1` がモデルと ONNX Runtime ごと配置する）。

.PARAMETER Configuration
    Debug か Release（既定 Release）。

.PARAMETER Tests
    ビルドの代わりにテストを回す。モデルと ONNX Runtime を環境変数で指定すると
    オフライン検証も走る（ブロックサイズ非依存・モノ/ステレオ一致・遅延・
    起動時キャリブレーション）:
        $env:DPDFNET_ORT   = "..\obs_plugin\vendor"
        $env:DPDFNET_MODEL = "..\plugin\models\dpdfnet8_48khz_hr_2out.onnx"
    未設定ならそのテストはスキップして成功扱いになる。

.PARAMETER TestFilter
    `cargo test <filter>` のフィルタ。

.PARAMETER NoCapture
    テストの出力を表示する（`--nocapture`）。

.EXAMPLE
    .\build.ps1
    .\build.ps1 -Tests -NoCapture
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

if ($Tests) {
    $cargoArgs = @("test")
    if ($Configuration -eq "Release") { $cargoArgs += "--release" }
    $cargoArgs += @("--lib")
    if ($TestFilter) { $cargoArgs += $TestFilter }
    $cargoArgs += @("--")
    if ($NoCapture) { $cargoArgs += "--nocapture" }
} else {
    $cargoArgs = @("build")
    if ($Configuration -eq "Release") { $cargoArgs += "--release" }
}

$cargoLine = ($cargoArgs | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join " "
Write-Host "==> $cargo $cargoLine" -ForegroundColor Cyan

$cmd = "`"$vcvars`" >nul 2>&1 && cd /d `"$root`" && `"$cargo`" $cargoLine"
& cmd.exe /c $cmd
$code = $LASTEXITCODE

if ($code -ne 0) {
    throw "cargo が失敗しました (exit $code)"
}

if (-not $Tests) {
    $dll = Join-Path $root "target\$($Configuration.ToLower())\dpdfnet_voice.dll"
    if (Test-Path -LiteralPath $dll) {
        $len = (Get-Item -LiteralPath $dll).Length
        Write-Host "==> $dll ($len bytes)" -ForegroundColor Green
    }
}
