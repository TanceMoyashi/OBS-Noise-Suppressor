#requires -Version 5.1
<#
.SYNOPSIS
    DPDFNet Voice (VST3 版) を DAW から使える場所へ入れます。

.DESCRIPTION
    入れる先は VST3 の標準フォルダです（DAW はここを既定で探します）。

        C:\Program Files\Common Files\VST3\DPDFNet Voice.vst3\   (既定。管理者が要る)

    管理者権限を使いたくないときは -User を付けると

        %APPDATA%\VST3\DPDFNet Voice.vst3\                      (ユーザー単位。管理者不要)

    へ入ります。DAW によってはこちらを探さないので、そのときは DAW の設定で
    VST3 のフォルダに `%APPDATA%\VST3` を足してください。

    bundle の中身は自己完結しています（モデルと ONNX Runtime を同梱）:

        DPDFNet Voice.vst3\Contents\x86_64-win\DPDFNet Voice.vst3   (本体)
                                                  \dpdfnet_onnxruntime.dll
                                                  \models\dpdfnet8_...onnx  (高品質)
                                                  \models\dpdfnet2_...onnx  (軽量)

    OBS 版が入っている必要はありません（DSP は同じものを使っていますが、
    別のファイルです）。

    コピーできないとき（Program Files に入れようとしたときなど）は、UAC で
    自分自身を管理者として起動し直します（-NoElevate で抑止）。

.PARAMETER Dest
    VST3 のフォルダを直接指定する（`<Dest>\DPDFNet Voice.vst3` を作る）。

.PARAMETER User
    ユーザー単位 (%APPDATA%\VST3) へ入れる（管理者不要）。

.PARAMETER NoElevate
    管理者としての起動し直しをしない。

.EXAMPLE
    .\install.ps1
    .\install.ps1 -User
    .\install.ps1 -Dest "D:\VST3"
#>
[CmdletBinding()]
param(
    [string]$Dest = "",
    [switch]$User,
    [switch]$NoElevate
)

$ErrorActionPreference = "Stop"
$Bundle = "DPDFNet Voice.vst3"
$src = Join-Path $PSScriptRoot $Bundle

function Fail([string]$Message) {
    Write-Host ""
    Write-Host "[エラー] $Message" -ForegroundColor Red
    exit 1
}

function Test-IsAdmin {
    return ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

# 自分自身を管理者として起動し直す。UAC が「通知しない」設定なら無言で昇格する。
function Invoke-ElevatedSelf {
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", ('"{0}"' -f $PSCommandPath))
    if ($Dest) { $argList += @("-Dest", ('"{0}"' -f $Dest)) }
    if ($User) { $argList += "-User" }
    Write-Host ("   powershell.exe " + ($argList -join " ")) -ForegroundColor DarkGray
    try {
        $p = Start-Process -FilePath "powershell.exe" -ArgumentList $argList `
            -Verb RunAs -Wait -PassThru
        return $p.ExitCode
    } catch {
        Write-Host "昇格できませんでした: $($_.Exception.Message)" -ForegroundColor Red
        Write-Host "  このスクリプトを右クリック → 「管理者として実行」してください。"
        Write-Host "  または -User を付けると管理者なしで入れられます。"
        return 1
    }
}

# --- 同梱物の確認 ---
if (-not (Test-Path -LiteralPath $src)) {
    Fail "同梱の $Bundle がありません: $src（ZIP を展開したフォルダで実行してください）"
}
$dll = Join-Path $src "Contents\x86_64-win\$Bundle"
if (-not (Test-Path -LiteralPath $dll)) {
    Fail "本体が見つかりません: $dll"
}
$ortNames = @("dpdfnet_onnxruntime.dll", "dpdfnet_onnxruntime_dml.dll")
$foundOrt = $false
foreach ($n in $ortNames) {
    if (Test-Path -LiteralPath (Join-Path (Split-Path $dll) $n)) { $foundOrt = $true }
}
if (-not $foundOrt) {
    Fail "同梱の ONNX Runtime が見つかりません: $(Split-Path $dll)"
}

# --- 入れ先を決める ---
if ($Dest) {
    $vst3Dir = $Dest
} elseif ($User) {
    $vst3Dir = Join-Path $env:APPDATA "VST3"
} else {
    $vst3Dir = Join-Path ${env:ProgramFiles} "Common Files\VST3"
}
$target = Join-Path $vst3Dir $Bundle

Write-Host ""
Write-Host "== DPDFNet Voice (VST3) を入れます" -ForegroundColor Cyan
Write-Host "   コピー元: $src"
Write-Host "   コピー先: $target"

if (-not (Test-Path -LiteralPath $vst3Dir)) {
    try {
        New-Item -ItemType Directory -Path $vst3Dir -Force | Out-Null
    } catch {
        if (-not $NoElevate -and -not (Test-IsAdmin)) {
            Write-Host ""
            Write-Host "[!] フォルダを作れませんでした: $($_.Exception.Message)" -ForegroundColor Yellow
            Write-Host "    管理者として実行し直します..." -ForegroundColor Yellow
            exit (Invoke-ElevatedSelf)
        }
        Fail "フォルダを作れませんでした: $($_.Exception.Message)"
    }
}

try {
    # 上書きインストール（古い版のファイルを消してから入れる）。
    if (Test-Path -LiteralPath $target) {
        Remove-Item -LiteralPath $target -Recurse -Force
    }
    Copy-Item -LiteralPath $src -Destination $target -Recurse -Force
} catch {
    if (-not $NoElevate -and -not (Test-IsAdmin)) {
        Write-Host ""
        Write-Host "[!] コピーできませんでした: $($_.Exception.Message)" -ForegroundColor Yellow
        Write-Host "    管理者として実行し直します..." -ForegroundColor Yellow
        exit (Invoke-ElevatedSelf)
    }
    Fail ("コピーできませんでした: $($_.Exception.Message)`n" +
        "DAW を終了してから、もう一度実行してください。")
}

$size = (Get-ChildItem -LiteralPath $target -Recurse -File | Measure-Object -Property Length -Sum).Sum
Write-Host ("   [OK] {0:N1} MB" -f ($size / 1MB)) -ForegroundColor Green

Write-Host ""
Write-Host "完了しました。" -ForegroundColor Green
Write-Host "1. DAW を起動し直す（プラグインを再スキャンさせる）"
Write-Host "2. DPDFNet Voice を挿す（モノラル / ステレオのトラックで使えます）"
Write-Host ""
Write-Host "設定: 既定はモデル「自動」/ CPU です。DAW の汎用パラメータ表示で" -ForegroundColor DarkGray
Write-Host "      「Model」「Inference Device」を変えられます（変更時に一瞬音が途切れます）。" -ForegroundColor DarkGray
if ($Dest -or $User) {
    Write-Host ""
    Write-Host "注意: このフォルダを DAW が探していない場合は、DAW の設定で VST3 の" -ForegroundColor Yellow
    Write-Host "      フォルダに $vst3Dir を追加してください。" -ForegroundColor Yellow
}
Write-Host ""
Write-Host "遅延は 48 kHz で 2880 サンプル (60 ms) です。DAW 側で自動補正されますが、" -ForegroundColor DarkGray
Write-Host "録音しながら聴くと 60 ms 遅れて聞こえます（OBS 版と同じ動作）。" -ForegroundColor DarkGray
