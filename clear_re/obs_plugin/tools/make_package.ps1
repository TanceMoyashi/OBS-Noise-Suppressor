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

.PARAMETER GpuPack
    本体ではなく **GPU (DirectML) 用の追加パック**を作る。本体は CPU のみで
    配り、GPU を使いたい人だけがこのパックを入れる（実測で GPU は CPU より
    遅く、同梱すると 36 MB 増えるため）。

.EXAMPLE
    .\tools\make_package.ps1
    .\tools\make_package.ps1 -SkipBuild -NoZip
    .\tools\make_package.ps1 -GpuPack
#>
[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [string]$OutDir = "",
    [switch]$SkipBuild,
    [switch]$SkipModels,
    [switch]$NoZip,
    [switch]$GpuPack
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

# --------------------------------------------------------------------------
# GPU (DirectML) 追加パック
# --------------------------------------------------------------------------
# 本体は CPU のみで配る。GPU は実測で CPU より遅く（1 ホップが小さいので
# DirectML の dispatch / sync の固定費が勝つ）、同梱すると 36 MB 増えるため、
# 使いたい人だけが入れる追加パックとして別に作る。
if ($GpuPack) {
    $pack = Join-Path $OutDir ("OBS-Noise-Suppressor-{0}-gpu-pack" -f $version)
    Write-Host "GPU 追加パック: $pack (v$version)" -ForegroundColor Cyan
    if (Test-Path -LiteralPath $pack) { Remove-Item -LiteralPath $pack -Recurse -Force }
    New-Item -ItemType Directory -Force -Path (Join-Path $pack "data") | Out-Null

    $vendor = Join-Path $root "vendor"
    $gpuNames = @("dpdfnet_onnxruntime_dml.dll", "DirectML.dll", "onnxruntime_providers_shared.dll")
    foreach ($n in $gpuNames) {
        $srcDll = Join-Path $vendor $n
        if (-not (Test-Path -LiteralPath $srcDll)) {
            throw "GPU 用の DLL がありません: $srcDll"
        }
        Copy-Item -LiteralPath $srcDll -Destination (Join-Path $pack "data\$n") -Force
    }
    foreach ($f in @("installer\install-gpu.ps1", "installer\install-gpu.bat")) {
        Copy-Item -LiteralPath (Join-Path $root $f) -Destination $pack -Force
    }
    $licenseSrc = Join-Path (Split-Path -Parent $repo) "LICENSE"
    if (Test-Path -LiteralPath $licenseSrc) {
        Copy-Item -LiteralPath $licenseSrc -Destination (Join-Path $pack "LICENSE") -Force
    } else {
        Write-Warning "LICENSE が見つかりません: $licenseSrc"
    }

    $readme = @"
OBS Noise Suppressor v$version - GPU (DirectML) 追加パック
========================================================

本体 (OBS-Noise-Suppressor-$version-win64.zip) を先にインストールしてある
場合だけ使えます。入るのは次の 3 つです。

    data\dpdfnet_onnxruntime_dml.dll   DirectML 入りの ONNX Runtime
    data\DirectML.dll                  DirectML 本体
    data\onnxruntime_providers_shared.dll

DPDFNet の推論を CPU ではなく GPU (DirectML) で回すための追加パックです。


インストール
------------
1. OBS を終了します (起動中だと DLL を上書きできません)。
2. install-gpu.bat をダブルクリックします。
   既定では本体と同じ場所へ入れます。管理者権限は不要です。

       %ProgramData%\obs-studio\plugins\obs_noise_suppressor\data\

   ポータブル版へ入れた場合は指定してください。

       install-gpu.bat -Obs "D:\obs-studio"

3. OBS を起動し、フィルタのプロパティの「推論デバイス」で
   「GPU - DirectML」を選びます (既定は CPU のままです)。


なぜ既定で入っていないのか
--------------------------
実測では **GPU は CPU より遅い**ためです (このマシン / ステレオ / 1 ホップ
10 ms の予算)。

    dp2 (軽量)   CPU 2.51 ms (25 %)  GPU 4.66 ms (47 %)
    dp8 (高品質) CPU 6.68 ms (67 %)  GPU 12.73 ms (127 %)

1 ホップが小さいため、DirectML の dispatch / sync の固定費が計算より大きく
なります。高品質モデルでは予算を超えるので、負荷が高いと自動的に軽量モデル
(dp2) へ切り替わります (そのときは理由がログに出ます)。CPU 使用率そのものは
下がるので、CPU が他の作業で埋まっているときの逃げ道として用意してあります。


GPU の性質
----------
* 出力は CPU とビット一致しません (丸めが違うため)。実機の比較では
  SNR 66 dB 程度で、聴感上の差はありません。
* 対応していない GPU ではセッション作成に失敗し、警告を出して CPU で
  動きます (無音にはなりません)。
* GPU をやめるときは、data\ の 3 つを手で削除してください (それだけで
  CPU に戻ります。プラグイン本体は uninstall.bat で消えます)。


ライセンス
----------
* ONNX Runtime (dpdfnet_onnxruntime_dml.dll): MIT License (Microsoft)
* DirectML (DirectML.dll): Microsoft のライセンスに従います
* この追加パックの構成物: GPL-2.0-or-later (同梱の LICENSE を参照)

詳しくはリポジトリの clear_re\obs_plugin\README.md と OBS_VERIFY.md を
参照してください。
"@
    $readme = $readme -replace "`r?`n", "`r`n"
    [System.IO.File]::WriteAllText((Join-Path $pack "README.txt"), $readme,
        (New-Object System.Text.UTF8Encoding($true)))

    Write-Host ""
    Get-ChildItem -LiteralPath $pack -Recurse -File | Sort-Object FullName |
        ForEach-Object {
            $rel = $_.FullName.Substring($pack.Length + 1)
            Write-Host ("  {0,-40} {1,12:N0}" -f $rel, $_.Length)
        }
    $totalMB = [math]::Round(((Get-ChildItem -LiteralPath $pack -Recurse -File |
        Measure-Object -Property Length -Sum).Sum / 1MB), 2)
    Write-Host ""
    Write-Host "合計 $totalMB MB" -ForegroundColor Green

    if (-not $NoZip) {
        $zip = Join-Path $OutDir ("OBS-Noise-Suppressor-{0}-gpu-pack.zip" -f $version)
        if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::CreateFromDirectory(
            $pack, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
        $zipMB = [math]::Round(((Get-Item -LiteralPath $zip).Length / 1MB), 2)
        Write-Host "ZIP: $zip ($zipMB MB)" -ForegroundColor Green
    }
    return
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


パネルの操作
------------
* フェーダー (AMBIENCE / VOICE / REVERB) をドラッグして調整します。
  ダブルクリックで既定値に戻ります。
* 各フェーダーの下の MUTE / BYP キーは、そのフェーダーだけを切ります。
  つまみの位置と保存値は変わらず、効きだけが切り替わります。
  MUTE = その機能の最小値、BYP = 掛けない中立値で、同じキーをもう一度
  押すと有効 (つまみの値) へ戻ります。MUTE と BYP は排他です。
* IN / OUT の 2 本の VU メーターで、加工前と加工後のレベルを見比べられます
  (ステレオは L / R の平均)。抑圧が掛かると OUT の針が IN より下がります。
* ヘッダ右の 1〜8 はプリセットです。SAVE を押してから番号を押すと保存し、
  番号を押すと呼び出します (Ctrl + クリックで消去)。


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
* 音が途切れる / 遅れて聞こえるときは、まず CPU を疑ってください。既定の
  「自動」で選ばれる高品質モデルは、速い CPU でもコア 1 本をほぼ使い切ります
  (10 ms の枠に 8 ms 前後)。ノート PC や古い CPU では間に合わなくなります。
  プロパティの「モデル」を「軽量 (dp2)」にすると負荷が約 1/3 になります。
  間に合っているかは、ログの
    音声処理の負荷: 1 ホップ平均 7.25 ms / 予算 10.00 ms (73 %)
  のような行で確認できます (1 回だけ出ます)。間に合わない状態が続いた場合は
  警告を残し、1 度だけ自動で軽量モデルへ切り替えます (設定は書き換えません)。
* GPU (DirectML) を使いたい場合は、別配布の追加パック
  (OBS-Noise-Suppressor-<版>-gpu-pack.zip) を入れると、プロパティの
  「推論デバイス」で GPU を選べます。ただし**実測では GPU の方が遅い**ので
  (このマシン / ステレオで dp8 6.7 -> 12.7 ms)、CPU に余裕があるなら
  CPU のままをおすすめします。
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
