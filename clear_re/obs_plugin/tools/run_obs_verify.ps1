#requires -Version 5.1
<#
.SYNOPSIS
    OBS 実機で OBS Noise Suppressor を回して、遅延と DSP を数値で確認する。

.DESCRIPTION
    やること:

      1. 検証用のシーンコレクション / プロファイルを作る（`tools/make_verify_collection.py`）
      2. （必要なら）ビルドとインストール
      3. 起動中の OBS を閉じる（後片付けを確実にするため）
      4. `DPDFNET_DUMP` を設定して OBS を起動（メディアソース 1 つ + フィルタ 1 つ）
      5. ダンプが十分な長さになるまで待つ
      6. 画面をスクリーンショット（パネル UI の見た目確認用）
      7. ログからプラグインの状態行を出す
      8. `cargo test --release --lib live_dump` で
         「実機の出力 == 同じ入力をオフラインで回した出力」と「実機の遅延 = 2880」を検証
      9. OBS を閉じて `user.ini` を元に戻し、検証用コレクションを片付ける

    このマシンの `obs64.exe` には互換性設定 `RUNASADMIN` が付いているため OBS は
    管理者権限で動作する。UIPI のため非昇格プロセスからは OBS を閉じられないので、
    このスクリプトは**自分自身を管理者権限で起動し直す**（UAC が「通知しない」設定なら
    無言で昇格する。`ConsentPromptBehaviorAdmin = 0`）。昇格できなかった場合だけ、
    最後に手で OBS を閉じてもらう必要がある。

.PARAMETER Name
    検証用のシーンコレクション / プロファイル名（既定 `dpdfnet-verify`）。

.PARAMETER Seconds
    音を流す秒数（既定 25 秒）。

.PARAMETER SampleRate
    検証用プロファイルの OBS サンプルレート（既定 48000）。48 kHz 以外を試すと
    レート変換経路の実機確認になる（例: -SampleRate 96000）。

.PARAMETER Wav
    再生する WAV（既定は `plugin\test_signals\speech_tail_reverb_48k.wav`）。

.PARAMETER WorkDir
    ダンプ / スクリーンショット / user.ini の控えを置く場所。

.PARAMETER NoPanel
    パネルを自動で開かない。

.PARAMETER NoLaunch
    生成とログ表示だけ行い、OBS を起動しない。

.PARAMETER KeepObsOpen
    最後に OBS を閉じない（手で確認したいとき）。

.PARAMETER KeepCollection
    検証用コレクションを消さない。

.EXAMPLE
    .\tools\run_obs_verify.ps1 -SkipBuild -SkipInstall
#>
[CmdletBinding()]
param(
    [string]$Name = "dpdfnet-verify",
    [int]$Seconds = 25,
    [int]$SampleRate = 48000,
    [string]$Wav = "",
    [string]$WorkDir = (Join-Path $env:LOCALAPPDATA "Temp\opencode\live"),
    [switch]$NoPanel,
    [switch]$SkipBuild,
    [switch]$SkipInstall,
    [switch]$SkipTest,
    [switch]$NoLaunch,
    [switch]$KeepObsOpen,
    [switch]$KeepCollection,
    [switch]$Elevated   # 内部用: 昇格して再実行されたことを示す
)

$ErrorActionPreference = "Stop"

# --------------------------------------------------------------------------
# 昇格（OBS を閉じるのに必要）
# --------------------------------------------------------------------------

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not $Elevated -and -not (Test-Admin)) {
    New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
    $log = Join-Path $WorkDir "elevated.log"
    Remove-Item -LiteralPath $log -Force -ErrorAction SilentlyContinue

    $a = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", "`"$PSCommandPath`"",
        "-Elevated",
        "-Name", "`"$Name`"",
        "-Seconds", "$Seconds",
        "-SampleRate", "$SampleRate",
        "-WorkDir", "`"$WorkDir`""
    )
    if ($Wav) { $a += @("-Wav", "`"$Wav`"") }
    if ($NoPanel) { $a += "-NoPanel" }
    if ($SkipBuild) { $a += "-SkipBuild" }
    if ($SkipInstall) { $a += "-SkipInstall" }
    if ($SkipTest) { $a += "-SkipTest" }
    if ($NoLaunch) { $a += "-NoLaunch" }
    if ($KeepObsOpen) { $a += "-KeepObsOpen" }
    if ($KeepCollection) { $a += "-KeepCollection" }

    Write-Host "OBS を閉じるために管理者権限で再実行します…" -ForegroundColor Yellow
    try {
        $p = Start-Process -FilePath "powershell.exe" -ArgumentList $a -Verb RunAs `
            -PassThru -WindowStyle Minimized
    } catch {
        Write-Warning "昇格できませんでした: $_"
        Write-Warning "OBS を手で閉じたあとに、もう一度このスクリプトを実行してください。"
        exit 1
    }
    # 昇格側はトランスクリプトに出力する（こちらの標準出力には出せないため）。
    $null = $p.WaitForExit(1800000)
    if (Test-Path -LiteralPath $log) {
        Get-Content -LiteralPath $log
    } else {
        Write-Warning "昇格側のログがありません: $log"
    }
    exit $p.ExitCode
}

$started = Get-Date
Start-Transcript -Path (Join-Path $WorkDir "elevated.log") -Force | Out-Null

$root = Split-Path -Parent $PSScriptRoot          # obs_plugin/
$repo = Split-Path -Parent $root                   # clear_re/
$py = (Get-Command python -ErrorAction SilentlyContinue).Source
if (-not $py) { throw "python が見つかりません" }

# --- OBS のパス ---
$obsExe = "C:\Program Files\obs-studio\bin\64bit\obs64.exe"
if (-not (Test-Path -LiteralPath $obsExe)) { throw "obs64.exe が見つかりません: $obsExe" }
$obsBin = Split-Path -Parent $obsExe

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
$dumpPrefix = Join-Path $WorkDir "dump"
$userIni = Join-Path $env:APPDATA "obs-studio\user.ini"
$scenesDir = Join-Path $env:APPDATA "obs-studio\basic\scenes"
$collectionFile = Join-Path $scenesDir "$Name.json"
$profileDir = Join-Path $env:APPDATA "obs-studio\basic\profiles\$Name"
$logDir = Join-Path $env:APPDATA "obs-studio\logs"

# --------------------------------------------------------------------------
# OBS の起動 / 終了ヘルパ
# --------------------------------------------------------------------------

function Get-ObsProcess { @(Get-Process obs64 -ErrorAction SilentlyContinue) }

# OBS 本体も自前パネルも同じ obs64.exe のトップレベルウィンドウなので、
# `Process.MainWindowHandle`（Z オーダーの先頭を返す）だとパネルを掴んでしまう
# ことがある。可視のトップレベルウィンドウ全部に WM_CLOSE を投げる。
if (-not ("Dp.ObsCloser" -as [type])) {
    Add-Type -Namespace Dp -Name ObsCloser -MemberDefinition @'
[DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
[DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
delegate bool EnumProc(IntPtr h, IntPtr p);
static uint[] _ids;
static int _n;
public static int CloseVisible(uint[] ids)
{
    _ids = ids;
    _n = 0;
    EnumWindows(delegate(IntPtr h, IntPtr p) {
        uint pid;
        GetWindowThreadProcessId(h, out pid);
        if (System.Array.IndexOf(_ids, pid) >= 0 && IsWindowVisible(h)) {
            PostMessage(h, 0x0010 /* WM_CLOSE */, IntPtr.Zero, IntPtr.Zero);
            _n++;
        }
        return true;
    }, IntPtr.Zero);
    return _n;
}
'@
}

function Stop-Obs {
    param([int]$TimeoutSec = 20)

    $procs = Get-ObsProcess
    if (-not $procs) { return $true }

    Write-Host "起動中の OBS を閉じます (PID $($procs.Id -join ', '))…" -ForegroundColor Yellow
    # 注意: `[uint]` は Windows PowerShell 5.1 では型アクセラレータとして存在しない。
    $n = 0
    try {
        $n = [Dp.ObsCloser]::CloseVisible([System.UInt32[]]$procs.Id)
    } catch {
        Write-Warning "WM_CLOSE の送信に失敗: $_"
    }
    Write-Host "  WM_CLOSE を $n 個のウィンドウへ送信" -ForegroundColor Yellow
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if (-not (Get-ObsProcess)) { break }
    }

    $procs = Get-ObsProcess
    if ($procs) {
        Write-Host "応答が無いので強制終了します (PID $($procs.Id -join ', '))" -ForegroundColor Yellow
        $procs | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
    return (-not (Get-ObsProcess))
}

# user.ini をバイト等価のまま 1 箇所だけ書き換える（日本語のコレクション名を壊さない）。
function Set-IniValue {
    param([string]$Path, [string]$Pattern, [string]$Replacement)

    if (-not (Test-Path -LiteralPath $Path)) { return }
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)   # 1 バイト = 1 文字
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $text = $latin1.GetString($bytes)
    $new = $text -replace $Pattern, $Replacement
    if ($new -ne $text) {
        [System.IO.File]::WriteAllBytes($Path, $latin1.GetBytes($new))
    }
}

# --- 1) ビルド / インストール ---
if (-not $SkipBuild) {
    if ($SkipInstall) {
        & (Join-Path $root "build.ps1")
    } else {
        # install.ps1 がビルドもやる（OBS が起動中だと DLL を上書きできない）。
        & (Join-Path $root "install.ps1")
    }
}

# --- 2) 起動中の OBS を閉じる ---
if (-not (Stop-Obs)) {
    throw "OBS を終了できませんでした。手で閉じてから実行し直してください。"
}

# --- 3) シーンコレクション ---
$pyArgs = @((Join-Path $root "tools\make_verify_collection.py"), "--name", $Name,
    "--sample-rate", "$SampleRate")
if ($Wav) { $pyArgs += @("--wav", $Wav) }
& $py @pyArgs

if ($NoLaunch) {
    Write-Host "`n(NoLaunch) コレクション: $collectionFile ($SampleRate Hz)" -ForegroundColor Cyan
    Stop-Transcript | Out-Null
    return
}

# --- 4) user.ini を控えて、終了確認ダイアログを出さないようにする ---
Copy-Item -LiteralPath $userIni -Destination (Join-Path $WorkDir "user.ini.bak") -Force
Set-IniValue -Path $userIni -Pattern 'ConfirmOnExit=\w+' -Replacement 'ConfirmOnExit=false'

Remove-Item "$WorkDir\dump_*.wav" -Force -ErrorAction SilentlyContinue
# 前回の強制終了で残った sentinel があると「クラッシュが検出されました」ダイアログで止まる。
Remove-Item "$env:APPDATA\obs-studio\.sentinel\run_*" -Force -ErrorAction SilentlyContinue

$env:DPDFNET_DUMP = $dumpPrefix
if ($NoPanel) {
    Remove-Item Env:\DPDFNET_OPEN_PANEL_ON_START -ErrorAction SilentlyContinue
    Remove-Item Env:\DPDFNET_PANEL_SNAPSHOT -ErrorAction SilentlyContinue
} else {
    $env:DPDFNET_OPEN_PANEL_ON_START = "1"
    # パネル自身に自分の絵を書かせる（他のウィンドウに隠されていても取れる）。
    $env:DPDFNET_PANEL_SNAPSHOT = Join-Path $WorkDir "panel_live.bmp"
    Remove-Item (Join-Path $WorkDir "panel_live.bmp") -Force -ErrorAction SilentlyContinue
}

$logBefore = (Get-ChildItem "$logDir\*.txt" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1)

$exitCode = 0
try {
    # --- 5) OBS 起動 ---
    Write-Host "`nOBS 起動 (--multi --collection $Name / $SampleRate Hz)" -ForegroundColor Cyan
    Start-Process -FilePath $obsExe -WorkingDirectory $obsBin -ArgumentList @(
        "--multi", "--collection", $Name, "--profile", $Name,
        "--disable-missing-files-check", "--disable-updater"
    )

    # --- 6) ダンプが伸びるのを待つ ---
    $dumpOut = "${dumpPrefix}_out.wav"
    $deadline = (Get-Date).AddSeconds($Seconds)
    $lastLen = -1
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 2
        if (Test-Path -LiteralPath $dumpOut) {
            $len = (Get-Item -LiteralPath $dumpOut).Length
            if ($len -eq $lastLen -and $len -gt 200000) { break }
            $lastLen = $len
        }
    }
    if (Test-Path -LiteralPath $dumpOut) {
        Write-Host ("ダンプ: {0:N0} bytes ({1:N1} 秒ぶん)" -f `
            (Get-Item $dumpOut).Length, ((Get-Item $dumpOut).Length / 4 / 2 / $SampleRate)) -ForegroundColor Green
    } else {
        Write-Warning "ダンプができていません（フィルタが回っていない可能性）"
        $exitCode = 2
    }

    # --- 7) スクリーンショット ---
    $shot = Join-Path $WorkDir "obs_panel.png"
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
        $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
        $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        Write-Host "スクリーンショット: $shot" -ForegroundColor Green
    } catch {
        Write-Warning "スクリーンショットに失敗: $_"
    }

    # パネルが自分で書いた絵（隠されていても取れる）。BMP なので PNG に直す。
    $panelBmp = Join-Path $WorkDir "panel_live.bmp"
    if (Test-Path -LiteralPath $panelBmp) {
        try {
            Add-Type -AssemblyName System.Drawing
            $pb = [System.Drawing.Bitmap]::FromFile($panelBmp)
            $panelPng = Join-Path $WorkDir "panel_live.png"
            $pb.Save($panelPng, [System.Drawing.Imaging.ImageFormat]::Png)
            $pb.Dispose()
            Write-Host "パネル画像: $panelPng" -ForegroundColor Green
        } catch {
            Write-Warning "パネル画像の変換に失敗: $_"
        }
    } else {
        Write-Warning "パネル画像がありません（パネルが開かなかった？）"
    }

    # --- 8) ログ ---
    Start-Sleep -Seconds 2
    $log = Get-ChildItem "$logDir\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($logBefore -and $log.FullName -eq $logBefore.FullName) {
        Write-Warning "新しいログが見つかりません（OBS が起動していない？）"
    }
    Write-Host "`n=== $($log.Name): obs-noise-suppressor / dump / 音声まわり ===" -ForegroundColor Cyan
    Get-Content $log.FullName | Select-String -Pattern 'obs-noise-suppressor|noise-suppressor|dump:|status:|Audio|audio|error|Error|warning' |
        Select-Object -Last 50 | ForEach-Object { $_.Line }
} finally {
    # --- 9) OBS を閉じる（`destroy` が走って WAV ヘッダが確定する） ---
    if (-not $KeepObsOpen) {
        if (-not (Stop-Obs)) {
            Write-Warning "OBS を終了できませんでした。解析は WAV のファイル長から読むので続行できます。"
        }
    }
    # OBS は終了時に user.ini を書き換えるので、閉じたあとで戻す。
    $bak = Join-Path $WorkDir "user.ini.bak"
    if (Test-Path -LiteralPath $bak) {
        Copy-Item -LiteralPath $bak -Destination $userIni -Force
        Write-Host "user.ini を元に戻しました (SceneCollection は検証前の値)" -ForegroundColor Green
    }

    # --- 10) 実機 vs オフライン参照 ---
    if (-not $SkipTest -and -not $NoLaunch) {
        Write-Host "`n=== live_dump テスト ===" -ForegroundColor Cyan
        if (-not $env:DPDFNET_ORT) {
            $docs = [Environment]::GetFolderPath("MyDocuments")
            $cand = Join-Path $docs "OBS\VST3\DPDFNet Voice.vst3\Contents\x86_64-win"
            if (Test-Path -LiteralPath $cand) { $env:DPDFNET_ORT = $cand }
        }
        if (-not $env:DPDFNET_ORT -or -not (Test-Path -LiteralPath $env:DPDFNET_ORT)) {
            Write-Warning "DPDFNET_ORT を設定してください（dpdfnet_onnxruntime.dll のあるディレクトリ）"
        }
        if (-not $env:DPDFNET_MODEL) {
            $m = Join-Path $repo "plugin\models\dpdfnet8_48khz_hr_2out.onnx"
            if (Test-Path -LiteralPath $m) { $env:DPDFNET_MODEL = $m }
        }
        $env:DPDFNET_LIVE_DUMP = $dumpPrefix
        & (Join-Path $root "build.ps1") -Tests -TestFilter "live_dump" -NoCapture
    }

    if (-not $KeepCollection) {
        Remove-Item -LiteralPath $collectionFile -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $profileDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "検証用コレクション / プロファイルを消しました" -ForegroundColor Green
    }
}

$secs = ((Get-Date) - $started).TotalSeconds
Write-Host ("`n完了 ({0:N1} 秒)" -f $secs) -ForegroundColor Cyan
Stop-Transcript | Out-Null
exit $exitCode
