#requires -Version 5.1
<#
.SYNOPSIS
    GPU (DirectML) 用の追加 DLL を、インストール済みの OBS Noise Suppressor に入れる。

.DESCRIPTION
    先に本体 (OBS-Noise-Suppressor-x.y.z-win64.zip) を入れておいてください。
    この追加パックが入れるのは 3 つだけです。

        dpdfnet_onnxruntime_dml.dll       DirectML 入りの ONNX Runtime
        onnxruntime_providers_shared.dll
        DirectML.dll                      Windows 側の版が古いことがあるので同梱

    既定の入れ先 (本体の install.bat と同じ):

        %ProgramData%\obs-studio\plugins\obs_noise_suppressor\data\

    ポータブル版へ入れたときは -Obs "D:\obs-studio" のように指定してください。

    入れたあと、プロパティの「推論デバイス」で「GPU - DirectML」を選べます。

    **実測では GPU は CPU より遅いです** (このマシン / ステレオ / 1 ホップ
    10 ms の予算):

        dp2 (軽量)   CPU 2.51 ms (25 %)  GPU 4.66 ms (47 %)
        dp8 (高品質) CPU 6.68 ms (67 %)  GPU 12.73 ms (127 %)

    1 ホップが小さいため、DirectML の dispatch / sync の固定費が計算より大きく
    なります。CPU 使用率は下がるので、CPU が他の作業で埋まっているときの
    逃げ道として用意してあります。

.PARAMETER Obs
    ポータブル版などの OBS ルート。<Obs>\data\obs-plugins\obs_noise_suppressor\
    に入れます。

.PARAMETER Dest
    入れ先 (data フォルダ) を直接指定する。

.EXAMPLE
    .\install-gpu.ps1
    .\install-gpu.ps1 -Obs "D:\obs-studio"
#>
[CmdletBinding()]
param(
    [string]$Obs = "",
    [string]$Dest = ""
)

$ErrorActionPreference = "Stop"
$ModuleName = "obs_noise_suppressor"
$names = @("dpdfnet_onnxruntime_dml.dll", "DirectML.dll", "onnxruntime_providers_shared.dll")
$src = Join-Path $PSScriptRoot "data"

function Fail([string]$Message) {
    Write-Host ""
    Write-Host "[エラー] $Message" -ForegroundColor Red
    exit 1
}

foreach ($n in $names) {
    if (-not (Test-Path -LiteralPath (Join-Path $src $n))) {
        Fail "同梱の $n がありません: $src"
    }
}

# 入れ先 = 本体が入っている場所。目印は dpdfnet_onnxruntime.dll (CPU 版)。
$cands = New-Object System.Collections.Generic.List[string]
if ($Dest) { $cands.Add($Dest) }
if ($Obs) { $cands.Add((Join-Path $Obs "data\obs-plugins\$ModuleName")) }
$cands.Add((Join-Path $env:ProgramData "obs-studio\plugins\$ModuleName\data"))
$cands.Add((Join-Path $env:ProgramFiles "obs-studio\data\obs-plugins\$ModuleName"))

$dest = $null
foreach ($c in $cands) {
    if (Test-Path -LiteralPath (Join-Path $c "dpdfnet_onnxruntime.dll")) { $dest = $c; break }
}
if (-not $dest) {
    $tried = ($cands | ForEach-Object { "    $_" }) -join "`n"
    Fail ("OBS Noise Suppressor のインストール先が見つかりません。`n" +
        "本体 (OBS-Noise-Suppressor-x.y.z-win64.zip) を先に入れてください。`n" +
        "探した場所:`n$tried`n`n" +
        'ポータブル版へ入れた場合は -Obs "D:\obs-studio" を付けてください。')
}

Write-Host ""
Write-Host "== GPU (DirectML) の DLL を入れます" -ForegroundColor Cyan
Write-Host "   コピー先: $dest"
try {
    foreach ($n in $names) {
        Copy-Item -LiteralPath (Join-Path $src $n) -Destination (Join-Path $dest $n) -Force
        Write-Host "   [OK] $n" -ForegroundColor Green
    }
} catch {
    Fail ("コピーできませんでした: $($_.Exception.Message)`n" +
        "OBS を終了してから、もう一度実行してください。本体を「管理者として実行」で`n" +
        "入れた場合は、このスクリプトも管理者として実行してください。")
}

Write-Host ""
Write-Host "完了しました。" -ForegroundColor Green
Write-Host "1. OBS を起動し直す"
Write-Host "2. フィルタのプロパティ -> 「推論デバイス」で「GPU - DirectML」を選ぶ"
Write-Host ""
Write-Host "注意: 実測では GPU は CPU より遅いです (dp2 2.5 -> 4.7 ms / dp8 6.7 -> 12.7 ms、" -ForegroundColor Yellow
Write-Host "      このマシンでのステレオ)。CPU 使用率は下がります。" -ForegroundColor Yellow
