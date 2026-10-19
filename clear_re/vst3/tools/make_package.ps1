#requires -Version 5.1
<#
.SYNOPSIS
    DPDFNet Voice (VST3 版) の配布パッケージ (ZIP) を作る。

.DESCRIPTION
    dist/DPDFNet-Voice-<version>-vst3/ に次の形で組み立てて ZIP にします。

        install.bat / install.ps1        入れる（VST3 の標準フォルダ）
        uninstall.bat / uninstall.ps1    消す
        README.txt                       使い方
        LICENSE                          GPL-2.0
        DPDFNet Voice.vst3\Contents\x86_64-win\
            DPDFNet Voice.vst3                      本体（cargo の出力をリネーム）
            dpdfnet_onnxruntime.dll                 ONNX Runtime (CPU)
            models\dpdfnet8_48khz_hr_2out.onnx      高品質モデル
            models\dpdfnet2_48khz_hr_2out.onnx      軽量モデル

    モデルと ONNX Runtime は OBS 版と同じものを使う（リポジトリに追跡済み）。

.PARAMETER Configuration
    Debug か Release（既定 Release）。

.PARAMETER OutDir
    出力先（既定 <リポジトリ>\dist）。

.PARAMETER SkipBuild
    ビルドを飛ばす（すでに target\ にある DLL を使う）。

.PARAMETER WithGpu
    GPU (DirectML) 用の DLL も入れる。**実測では GPU は CPU より遅い**ので
    既定では入れない（README.txt に注意書きを出す）。

.PARAMETER NoZip
    ZIP を作らずフォルダだけ作る。

.EXAMPLE
    .\make_package.ps1
    .\make_package.ps1 -WithGpu
#>
[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [string]$OutDir = "",
    [switch]$SkipBuild,
    [switch]$WithGpu,
    [switch]$NoZip
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot                       # clear_re\vst3\tools
$crate = Split-Path $root -Parent           # clear_re\vst3
$repo = Split-Path (Split-Path $crate -Parent) -Parent
$vendor = Join-Path $repo "clear_re\obs_plugin\vendor"
$models = Join-Path $repo "clear_re\plugin\models"
$license = Join-Path $repo "LICENSE"

if (-not $OutDir) { $OutDir = Join-Path $repo "dist" }

# --- バージョンは Cargo.toml から取る（1 箇所だけに書く） ---
$cargoToml = Get-Content -LiteralPath (Join-Path $crate "Cargo.toml") -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw "Cargo.toml から version を読めません"
}
$version = $Matches[1]

$pack = Join-Path $OutDir "DPDFNet-Voice-$version-vst3"
$bundle = Join-Path $pack "DPDFNet Voice.vst3"
$inner = Join-Path $bundle "Contents\x86_64-win"
$modDir = Join-Path $inner "models"
$bin = Join-Path $inner "DPDFNet Voice.vst3"

Write-Host "パッケージ先: $pack (v$version)" -ForegroundColor Cyan

# --- ビルド ---
if (-not $SkipBuild) {
    & (Join-Path $crate "build.ps1") -Configuration $Configuration
    if ($LASTEXITCODE -ne 0) { throw "ビルドに失敗しました" }
}

$confDir = $Configuration.ToLower()
$dll = Join-Path $crate "target\$confDir\dpdfnet_voice.dll"
if (-not (Test-Path -LiteralPath $dll)) {
    throw "DLL が見つかりません: $dll（先に build.ps1 を回してください）"
}

# -SkipBuild で古い DLL を梱包してしまう事故を防ぐ（src より古ければ作り直す）。
$dllTime = (Get-Item -LiteralPath $dll).LastWriteTime
$newestSrc = Get-ChildItem -Path (Join-Path $crate "src") -Recurse -File |
    Sort-Object LastWriteTime | Select-Object -Last 1
if ($newestSrc -and $dllTime -lt $newestSrc.LastWriteTime) {
    throw ("DLL がソースより古いです: $dll ($dllTime) < $($newestSrc.Name) ($($newestSrc.LastWriteTime))`n" +
        "-SkipBuild を外して作り直してください。")
}

# --- 同梱物の確認 ---
$need = @(
    (Join-Path $vendor "dpdfnet_onnxruntime.dll"),
    (Join-Path $models "dpdfnet8_48khz_hr_2out.onnx"),
    (Join-Path $models "dpdfnet2_48khz_hr_2out.onnx")
)
if ($WithGpu) {
    $need += @(
        (Join-Path $vendor "dpdfnet_onnxruntime_dml.dll"),
        (Join-Path $vendor "DirectML.dll"),
        (Join-Path $vendor "onnxruntime_providers_shared.dll")
    )
}
foreach ($n in $need) {
    if (-not (Test-Path -LiteralPath $n)) { throw "同梱物がありません: $n" }
}

# --- 組み立て ---
if (Test-Path -LiteralPath $pack) { Remove-Item -LiteralPath $pack -Recurse -Force }
New-Item -ItemType Directory -Force -Path $inner, $modDir | Out-Null

Copy-Item -LiteralPath $dll -Destination $bin -Force
Copy-Item -LiteralPath (Join-Path $vendor "dpdfnet_onnxruntime.dll") -Destination $inner -Force
Copy-Item -LiteralPath (Join-Path $models "dpdfnet8_48khz_hr_2out.onnx") -Destination $modDir -Force
Copy-Item -LiteralPath (Join-Path $models "dpdfnet2_48khz_hr_2out.onnx") -Destination $modDir -Force
if ($WithGpu) {
    foreach ($n in @("dpdfnet_onnxruntime_dml.dll", "DirectML.dll", "onnxruntime_providers_shared.dll")) {
        Copy-Item -LiteralPath (Join-Path $vendor $n) -Destination $inner -Force
    }
}

foreach ($f in @("install.ps1", "install.bat", "uninstall.ps1", "uninstall.bat")) {
    Copy-Item -LiteralPath (Join-Path $crate "installer\$f") -Destination $pack -Force
}
Copy-Item -LiteralPath $license -Destination (Join-Path $pack "LICENSE") -Force

# --- README.txt（配布物の中に入る説明） ---
$gpuNote = if ($WithGpu) {
    @"
GPU (DirectML) について
-----------------------
このパッケージには GPU 用の DLL も入っています。使うには DAW の汎用パラメータ
表示で「Inference Device」を「GPU (DirectML)」にしてください。

**実測では GPU は CPU より遅いです** (開発機 / ステレオ / 1 ホップ 10 ms の予算):

    dp2 (軽量)   CPU 2.51 ms (25 %)  GPU 4.66 ms (47 %)
    dp8 (高品質) CPU 6.68 ms (67 %)  GPU 12.73 ms (127 %)

1 ホップが小さいため、DirectML の起動と同期の固定費が計算より大きくなります。
CPU 使用率は下がるので、CPU が他の作業で埋まっているときの逃げ道としてどうぞ。
"@
} else {
    @"
GPU (DirectML) について
-----------------------
このパッケージは CPU 用だけです。GPU を使いたい場合は同梱の DLL が足りないので、
OBS 版の GPU 追加パック (OBS-Noise-Suppressor-x.y.z-gpu-pack.zip) から次の 3 つを
`Contents\x86_64-win\` にコピーしてください。

    dpdfnet_onnxruntime_dml.dll
    DirectML.dll
    onnxruntime_providers_shared.dll

**実測では GPU は CPU より遅いです** (dp2 2.5 -> 4.7 ms / dp8 6.7 -> 12.7 ms)。
"@
}

$readme = @"
DPDFNet Voice $version (VST3 版)
===============================

OBS 用フィルタ (OBS Noise Suppressor) と同じ DSP を DAW でも使えるようにした
VST3 プラグインです。話し声から暗騒音と残響を落とします。

使い方
------
1. install.bat を実行する (ダブルクリックで OK)

     既定:        C:\Program Files\Common Files\VST3\   (管理者が要ります)
     管理者なし:  install.bat -User  ->  %APPDATA%\VST3\
     好きな場所:  install.bat -Dest "D:\VST3"

2. DAW を起動し直す (プラグインを再スキャンさせる)
3. DPDFNet Voice をトラックに挿す

消すときは uninstall.bat。

プラグインの内容
----------------
    DPDFNet Voice.vst3\Contents\x86_64-win\DPDFNet Voice.vst3   本体
                                          \dpdfnet_onnxruntime.dll
                                          \models\*.onnx       モデル
    install.ps1 / install.bat                入れる
    uninstall.ps1 / uninstall.bat            消す
    LICENSE                                  GPL-2.0

パラメータ
----------
    Ambience       残す暗騒音 (0 % = 最大抑圧 / 100 % = そのまま)
    Voice          presence (+-12 dB)
    Voice Reverb   残す残響 (0 % = 残さない / 100 % = そのまま)
    上の 3 つには ミュート / バイパス の切り替えが付いています
    Model          自動 / 高品質 (dp8) / 軽量 (dp2)
    Inference Device  CPU / GPU (DirectML)
    Bypass         全体のバイパス (オンのとき素通し)

「自動」は起動時に実測して、重ければ軽量モデルで始めます (音が乱れる前に決める)。
Model と Inference Device は自動化されない設定です。変えると一瞬音が途切れます。

知っておくこと
--------------
* 遅延は 48 kHz で 2880 サンプル (60 ms) です。DAW は自動で補正しますが、
  録音しながら聴くと 60 ms 遅れて聞こえます (OBS 版と同じ)。
* 48 kHz 以外のサンプルレートでも動きます (内部は 48 kHz のまま変換します)。
* GUI は OBS 版と同じ自前パネルです (VU / フェーダー / MUTE / BYP /
  メモリー / MODEL / DEV)。ホストの子ウィンドウとして出ます。
  固定サイズで、倍率はホストが伝えてきた DPI に従います。
  メモリー (1〜8) は %APPDATA%\DPDFNet Voice\presets.json に保存されます。
* ログは NIH_LOG でファイルに出せます:
      set NIH_LOG=%TEMP%\dpdfnet-vst3.log
* モノラルのトラックにもステレオのトラックにも挿せます。

$gpuNote

ライセンス
----------
GPL-2.0 (同梱の LICENSE)。DPDFNet のモデルと ONNX Runtime はそれぞれの
ライセンスに従います。
"@
[System.IO.File]::WriteAllText((Join-Path $pack "README.txt"), $readme,
    (New-Object System.Text.UTF8Encoding($true)))

# --- 一覧と合計 ---
Write-Host ""
Get-ChildItem -LiteralPath $pack -Recurse -File | Sort-Object FullName |
    ForEach-Object {
        $rel = $_.FullName.Substring($pack.Length + 1)
        Write-Host ("  {0,-52} {1,12:N0}" -f $rel, $_.Length)
    }
$totalMB = [math]::Round(((Get-ChildItem -LiteralPath $pack -Recurse -File |
    Measure-Object -Property Length -Sum).Sum / 1MB), 2)
Write-Host ""
Write-Host "合計 $totalMB MB" -ForegroundColor Green

if (-not $NoZip) {
    $zip = Join-Path $OutDir ("DPDFNet-Voice-{0}-vst3.zip" -f $version)
    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $pack, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
    $zipMB = [math]::Round(((Get-Item -LiteralPath $zip).Length / 1MB), 2)
    # Get-FileHash が無い環境があるので .NET で計算する。
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $fs = [System.IO.File]::OpenRead($zip)
    try {
        $hash = (($sha.ComputeHash($fs) | ForEach-Object { $_.ToString("x2") }) -join "")
    } finally {
        $fs.Dispose()
        $sha.Dispose()
    }
    Write-Host "ZIP: $zip ($zipMB MB)" -ForegroundColor Green
    Write-Host "SHA256: $hash"
}
