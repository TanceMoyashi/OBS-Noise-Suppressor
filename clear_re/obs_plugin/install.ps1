#requires -Version 5.1
<#
.SYNOPSIS
    DPDFNet Voice (OBS 版) を OBS Studio にインストールする。

.DESCRIPTION
    成果物を OBS のプラグイン配置

        <Dest>\bin\64bit\dpdfnet_voice.dll   本体
        <Dest>\data\dpdfnet_onnxruntime.dll  ONNX Runtime (同梱)
        <Dest>\data\models\*.onnx            モデル
        <Dest>\data\manifest.json            OBS 32 のプラグイン情報

    にコピーする。管理者権限は不要（ProgramData は書き込み可）。

.PARAMETER Dest
    インストール先。既定 `%ProgramData%\obs-studio\plugins\dpdfnet_voice`。

.PARAMETER OrtDll
    同梱する `dpdfnet_onnxruntime.dll` の場所。省略時は
    `DPDFNET_ORT` → obs_plugin 直下 → VST3 版の同梱 DLL の順に探す。

.PARAMETER ModelSource
    モデルのコピー元ディレクトリ。既定 `<repo>\plugin\models`。

.PARAMETER SkipBuild
    ビルドせず、既存の `target\<Configuration>\dpdfnet_voice.dll` を使う。

.PARAMETER SkipModels
    モデルをコピーしない（既に `data\models` にある場合）。

.EXAMPLE
    .\install.ps1
    .\install.ps1 -SkipBuild -SkipModels
#>
[CmdletBinding()]
param(
    [string]$Dest = (Join-Path $env:ProgramData "obs-studio\plugins\dpdfnet_voice"),
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [string]$OrtDll = "",
    [string]$ModelSource = "",
    [switch]$SkipBuild,
    [switch]$SkipModels
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$repo = Split-Path -Parent $root

function Resolve-OrtDll {
    param([string]$Explicit)

    $names = @("dpdfnet_onnxruntime.dll", "onnxruntime.dll")
    $dirs = New-Object System.Collections.Generic.List[string]

    if ($Explicit) {
        if (Test-Path -LiteralPath $Explicit -PathType Leaf) { return (Resolve-Path -LiteralPath $Explicit).Path }
        $dirs.Add($Explicit)
    }
    if ($env:DPDFNET_ORT) {
        $p = $env:DPDFNET_ORT
        if (Test-Path -LiteralPath $p -PathType Leaf) { return (Resolve-Path -LiteralPath $p).Path }
        $dirs.Add($p)
    }
    $dirs.Add($root)
    $docs = [Environment]::GetFolderPath("MyDocuments")
    if ($docs) {
        $dirs.Add((Join-Path $docs "OBS\VST3\DPDFNet Voice.vst3\Contents\x86_64-win"))
    }
    $dirs.Add((Join-Path ${env:ProgramFiles} "obs-studio\obs-plugins\64bit"))

    foreach ($d in $dirs) {
        if (-not (Test-Path -LiteralPath $d -PathType Container)) { continue }
        foreach ($n in $names) {
            $f = Join-Path $d $n
            if (Test-Path -LiteralPath $f -PathType Leaf) { return (Resolve-Path -LiteralPath $f).Path }
        }
    }
    return $null
}

# --- 0) OBS が起動中だと DLL を上書きできない ---
$running = Get-Process -Name "obs64" -ErrorAction SilentlyContinue
if ($running) {
    throw "OBS (obs64) が起動中です。終了してから実行してください。"
}

# --- 1) ビルド ---
$built = Join-Path $root "target\$($Configuration.ToLower())\dpdfnet_voice.dll"
if (-not $SkipBuild) {
    & (Join-Path $root "build.ps1") -Configuration $Configuration
}
if (-not (Test-Path -LiteralPath $built)) {
    throw "ビルド成果物がありません: $built"
}

# --- 2) 配置先を用意 ---
$binDir = Join-Path $Dest "bin\64bit"
$dataDir = Join-Path $Dest "data"
$modelDir = Join-Path $dataDir "models"
New-Item -ItemType Directory -Force -Path $binDir, $dataDir, $modelDir | Out-Null

# --- 3) 本体 ---
Copy-Item -LiteralPath $built -Destination (Join-Path $binDir "dpdfnet_voice.dll") -Force
Write-Host "本体      : $(Join-Path $binDir 'dpdfnet_voice.dll')" -ForegroundColor Green

# --- 4) ONNX Runtime ---
$ort = Resolve-OrtDll -Explicit $OrtDll
if (-not $ort) {
    throw "dpdfnet_onnxruntime.dll が見つかりません。-OrtDll で指定してください。"
}
Copy-Item -LiteralPath $ort -Destination (Join-Path $dataDir "dpdfnet_onnxruntime.dll") -Force
Write-Host "ORT       : $ort -> $(Join-Path $dataDir 'dpdfnet_onnxruntime.dll')" -ForegroundColor Green

# --- 5) モデル ---
# paths.rs の MODEL_FILE_NAMES と同じ優先順。全部入れておくと差し替えが楽
# （コードは先頭から順に探すので dpdfnet8_*_2out が選ばれる）。
$modelNames = @(
    "dpdfnet8_48khz_hr_2out.onnx",
    "dpdfnet2_48khz_hr_2out.onnx",
    "dpdfnet8_48khz_hr.onnx",
    "dpdfnet2_48khz_hr.onnx"
)
if (-not $SkipModels) {
    if (-not $ModelSource) { $ModelSource = Join-Path $repo "plugin\models" }
    if (-not (Test-Path -LiteralPath $ModelSource -PathType Container)) {
        throw "モデルのコピー元がありません: $ModelSource"
    }
    $copied = 0
    foreach ($n in $modelNames) {
        $src = Join-Path $ModelSource $n
        if (Test-Path -LiteralPath $src -PathType Leaf) {
            Copy-Item -LiteralPath $src -Destination (Join-Path $modelDir $n) -Force
            $copied++
        }
    }
    if ($copied -eq 0) {
        throw "モデルが 1 つもコピーできませんでした (コピー元: $ModelSource)"
    }
    Write-Host "モデル    : $copied 個 -> $modelDir" -ForegroundColor Green
}

# --- 6) manifest.json (OBS 32 のプラグイン一覧に出す情報) ---
$manifest = [ordered]@{
    id               = "dpdfnet-voice"
    name             = "dpdfnet_voice"
    display_name     = "DPDFNet Voice"
    version          = "0.1.0"
    os_arch          = "win64"
    description      = "Neural speech enhancement (denoise / dereverb) audio filter"
    long_description = "DPDFNet Voice OBS native audio filter (Rust). ONNX Runtime based neural noise suppression and dereverberation for microphone sources, 48 kHz, 6-hop (2880 sample) algorithmic latency."
    urls             = [ordered]@{
        repository = ""
        website    = ""
        support    = ""
    }
    has_banner       = $false
    has_icon         = $false
}
$manifestPath = Join-Path $dataDir "manifest.json"
[System.IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "manifest  : $manifestPath" -ForegroundColor Green

Write-Host ""
Write-Host "インストール完了: $Dest" -ForegroundColor Cyan
Write-Host "OBS を起動してフィルタ「DPDFNet Voice」を追加し、プロパティの" -ForegroundColor Cyan
Write-Host "「パネルを開く（ノブ / メーター）」ボタンで UI を開けます。" -ForegroundColor Cyan
