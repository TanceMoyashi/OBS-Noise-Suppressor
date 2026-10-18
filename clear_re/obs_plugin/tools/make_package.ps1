#requires -Version 5.1
<#
.SYNOPSIS
    OBS Noise Suppressor の配布パッケージ (install.bat 同梱) を作る。

.DESCRIPTION
    既定では <project root>\dist\OBS-Noise-Suppressor\ に

        install.bat                         インストーラー (ダブルクリック)
        install.ps1                         その中身
        uninstall.bat                       アンインストーラー
        uninstall.ps1                       その中身
        README.txt                          使い方 (日本語)
        LICENSE                             ライセンス (GPL-2.0-or-later)
        bin\64bit\obs_noise_suppressor.dll  本体
        data\dpdfnet_onnxruntime.dll        ONNX Runtime (同梱)
        data\models\*.onnx                  モデル
        data\manifest.json                  OBS のプラグイン情報

    を組み立て、さらに -NoZip を付けない限り 1 つの ZIP にまとめる。

    bin / data の組み立ては install.ps1 に任せている (-Dest をパッケージ先に
    向けるだけ)。だから「開発用インストーラ」と「配布パッケージ」で配置が
    食い違うことがない。

.PARAMETER Configuration
    Debug か Release (既定 Release)。

.PARAMETER OutDir
    出力先。既定 <project root>\dist。

.PARAMETER SkipBuild
    ビルドせず既存の target\<Configuration> を使う。

.PARAMETER SkipModels
    モデルを入れない (軽いパッケージを作りたいとき)。

.PARAMETER NoZip
    ZIP を作らない。

.EXAMPLE
    .\tools\make_package.ps1
    .\tools\make_package.ps1 -SkipBuild -NoZip
#>
[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [string]$OutDir = "",
    [switch]$SkipBuild,
    [switch]$SkipModels,
    [switch]$NoZip
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot              # obs_plugin\tools
$root = Split-Path -Parent $root   # obs_plugin
$repo = Split-Path -Parent $root   # clear_re

if (-not $OutDir) { $OutDir = Join-Path (Split-Path -Parent $repo) "dist" }
$pkg = Join-Path $OutDir "OBS-Noise-Suppressor"

# --- バージョンは Cargo.toml から取る ---
$version = "0.0.0"
$cargo = Join-Path $root "Cargo.toml"
if (Test-Path -LiteralPath $cargo) {
    $m = Select-String -LiteralPath $cargo -Pattern '^\s*version\s*=\s*"([^"]+)"' |
        Select-Object -First 1
    if ($m) { $version = $m.Matches[0].Groups[1].Value }
}

Write-Host "パッケージ先: $pkg (v$version)" -ForegroundColor Cyan

if (Test-Path -LiteralPath $pkg) {
    Remove-Item -LiteralPath $pkg -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $pkg | Out-Null

# --- bin / data は install.ps1 に作らせる ---
& (Join-Path $root "install.ps1") -Dest $pkg -Configuration $Configuration `
    -SkipBuild:$SkipBuild -SkipModels:$SkipModels

# --- 安全確認: 梱包した DLL がこのバージョンで作られているか ---
# `build.ps1 -Tests` はテスト用の lib だけを作り、cdylib (この DLL) は作り直さない。
# そのためバージョンを上げたあとに cdylib をビルドし直さないと、古いバージョンの
# DLL をそのまま梱包してしまう (実際に v0.3.0 で起きた)。ここで中身を検査する。
$dll = Join-Path $pkg "bin\64bit\obs_noise_suppressor.dll"
if (-not (Test-Path -LiteralPath $dll)) {
    throw "DLL が見つかりません: $dll"
}
$bytes  = [System.IO.File]::ReadAllBytes($dll)
$needle = [System.Text.Encoding]::ASCII.GetBytes($version)
$found  = $false
for ($i = 0; $i -le $bytes.Length - $needle.Length; $i++) {
    if ($bytes[$i] -ne $needle[0]) { continue }
    $ok = $true
    for ($j = 1; $j -lt $needle.Length; $j++) {
        if ($bytes[$i + $j] -ne $needle[$j]) { $ok = $false; break }
    }
    if ($ok) { $found = $true; break }
}
if (-not $found) {
    throw (@(
        "DLL にバージョン '{0}' が埋まっていません。ビルドし直してください:"
        "  .\build.ps1 -Configuration Release   (または install.ps1)"
    ) -join "`n" -f $version)
}
Write-Host "DLL バージョン確認: $version" -ForegroundColor Green

# --- インストーラー / アンインストーラーを同梱 ---
foreach ($f in @("install.ps1", "uninstall.ps1", "installer\install.bat", "installer\uninstall.bat")) {
    Copy-Item -LiteralPath (Join-Path $root $f) -Destination $pkg -Force
}

# --- ライセンス (GPL はバイナリ配布時に全文の同梱を要求する) ---
$licenseSrc = Join-Path (Split-Path -Parent $repo) "LICENSE"
if (Test-Path -LiteralPath $licenseSrc) {
    Copy-Item -LiteralPath $licenseSrc -Destination (Join-Path $pkg "LICENSE") -Force
} else {
    Write-Warning "LICENSE が見つかりません: $licenseSrc"
}

# --- README.txt (UTF-8 BOM + CRLF。メモ帳で開く) ---
$readme = @"
OBS Noise Suppressor v$version (Windows 64bit / OBS Studio 28 以降)
====================================================================

OBS Studio 用の音声フィルタ (ノイズ抑制 / 残響除去) です。ONNX Runtime と
モデルを同梱しているので、追加のダウンロードは要りません。


インストール
------------
1. OBS を終了します (起動中だと DLL を上書きできません)。
2. install.bat をダブルクリックします。
   (中身は install.ps1 です。レジストリは使いません。)
3. OBS を起動し、音声ソースの「フィルタ」から
   「OBS Noise Suppressor」を追加してください。
   プロパティの「パネルを開く (フェーダー / メーター)」ボタンで UI が開きます。

install.bat は既定で次の場所に配置します。管理者権限は不要です。

    %ProgramData%\obs-studio\plugins\obs_noise_suppressor\

この場所は、通常インストーラー版・Steam 版・Microsoft Store 版の
どれでも共通で読まれます (OBS 32.2.2 のソースで確認済み)。
インストーラーはこれらのインストール先を検出して画面に表示します。

ポータブル版の OBS だけは %ProgramData% を読まないため、検出できた
ポータブル版には OBS 側の

    <OBS ルート>\obs-plugins\64bit\
    <OBS ルート>\data\obs-plugins\obs_noise_suppressor\

にも自動でコピーします。自動検出から漏れる場合は指定してください。

    install.bat -Obs "D:\obs-studio"


アンインストール
----------------
uninstall.bat をダブルクリックして y を押します。
消すのはこのプラグインだけです (OBS 本体やほかのプラグインには触れません)。

    uninstall.bat -Yes                  確認なしで削除
    uninstall.bat -Obs "D:\obs-studio"  ポータブル版も掃除


オプション (install.bat / uninstall.bat 共通)
---------------------------------------------
    -Obs "フォルダ"   ポータブル版など、追加で配置 / 削除する OBS
    -NoDetect         OBS の検出をしない
    -Force            OBS が起動中でも強行する
    -Dest "フォルダ"  配置先を変える (インストーラーのみ。別名 -Target)


困ったとき
----------
* OBS のログ (ヘルプ → ログファイル) に
    [obs-noise-suppressor]
  で始まる行が出ていれば読み込まれています。
* フィルタ一覧に 2 つ出る場合は旧名の配置が残っています。
  install.bat をもう一度実行すると旧名のフォルダを消します。
* サンプリングレートはどれでも構いません (44.1 / 48 / 88.2 / 96 / 176.4 /
  192 kHz など)。48 kHz 以外は内部で 48 kHz に変換して処理します。
* 内容遅延 (レイテンシ) は 2880 サンプル = 60 ms 固定です (48 kHz)。
  48 kHz 以外ではレート変換ぶん (約 2〜5 ms) が加算されます。
* プラグインのファイルは次の 4 つだけです。手で消しても構いません。
    bin\64bit\obs_noise_suppressor.dll   (本体)
    data\dpdfnet_onnxruntime.dll         (ONNX Runtime)
    data\models\*.onnx                   (モデル)
    data\manifest.json
* 前回「管理者として実行」してインストールしていた場合、ProgramData の
  ファイルが Administrators 所有になっていて上書きできません。
  install.bat / uninstall.bat はこれを検出すると自動で管理者として
  起動し直します (UAC の確認が出ます)。


ライセンス
----------
* このプラグイン本体 (Rust で書いた部分): GPL-2.0-or-later
  Copyright (C) 2026 TanceMoyashi
  全文は同梱の LICENSE を参照してください。
* ONNX Runtime (dpdfnet_onnxruntime.dll): MIT License (Microsoft)
* DPDFNet モデル (*.onnx): 同梱モデルのライセンスに従います

詳しくはリポジトリの clear_re\obs_plugin\README.md を参照してください。
"@
$readme = $readme -replace "`r?`n", "`r`n"
$readmePath = Join-Path $pkg "README.txt"
[System.IO.File]::WriteAllText($readmePath, $readme, (New-Object System.Text.UTF8Encoding($true)))

# --- 一覧 ---
Write-Host ""
Get-ChildItem -LiteralPath $pkg -Recurse -File |
    Sort-Object FullName |
    ForEach-Object {
        $rel = $_.FullName.Substring($pkg.Length + 1)
        Write-Host ("  {0,-46} {1,12:N0}" -f $rel, $_.Length)
    }

$totalMB = [math]::Round(((Get-ChildItem -LiteralPath $pkg -Recurse -File |
    Measure-Object -Property Length -Sum).Sum / 1MB), 2)
Write-Host ""
Write-Host "合計 $totalMB MB" -ForegroundColor Green

if ($NoZip) { return }

# --- ZIP (Compress-Archive ではなく .NET を使う: PS5.1 の 2GB 制限と
#     モジュール読み込みトラブルを避ける) ---
$zip = Join-Path $OutDir ("OBS-Noise-Suppressor-{0}-win64.zip" -f $version)
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $pkg, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)

$zipMB = [math]::Round(((Get-Item -LiteralPath $zip).Length / 1MB), 2)
Write-Host "ZIP: $zip ($zipMB MB)" -ForegroundColor Green
