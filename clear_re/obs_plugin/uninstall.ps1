#requires -Version 5.1
<#
.SYNOPSIS
    OBS Noise Suppressor を OBS Studio から削除する。

.DESCRIPTION
    消すもの:

      1. %ProgramData%\obs-studio\plugins\obs_noise_suppressor
         (および旧名 dpdfnet_voice が残っていればそれも)
      2. 検出した OBS の
           <OBS ルート>\obs-plugins\64bit\obs_noise_suppressor.dll
           <OBS ルート>\data\obs-plugins\obs_noise_suppressor
         (ポータブル版として追加配置したぶん)
      3. %APPDATA%\obs-studio\plugin_config\obs_noise_suppressor

    OBS 本体や、ほかのプラグインのファイルには一切触れません。
    レジストリも使いません。

.PARAMETER Dest
    消す配置先。既定 %ProgramData%\obs-studio\plugins\obs_noise_suppressor。

.PARAMETER Obs
    追加で対象にする OBS のフォルダ (自動検出から漏れるとき)。

.PARAMETER Yes
    確認なしで削除する。

.PARAMETER NoDetect
    OBS の検出をしない (ProgramData と -Dest だけ消す)。

.PARAMETER Force
    OBS が起動中でも強行する。

.EXAMPLE
    .\uninstall.ps1
    .\uninstall.ps1 -Yes
#>
[CmdletBinding()]
param(
    [Alias("Target")]
    [string]$Dest = "",
    [string[]]$Obs = @(),
    [switch]$Yes,
    [switch]$NoDetect,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$ModuleName = "obs_noise_suppressor"
$LegacyName = "dpdfnet_voice"
$AppName = "OBS Noise Suppressor"

function Write-Head([string]$t) { Write-Host ""; Write-Host "== $t" -ForegroundColor Cyan }
function Write-Ok([string]$t) { Write-Host "   [削除] $t" -ForegroundColor Green }
function Write-Skip([string]$t) { Write-Host "   [--] $t" -ForegroundColor DarkGray }
function Write-Warn2([string]$t) { Write-Host "   [!!] $t" -ForegroundColor Yellow }

# --------------------------------------------------------------------------
# OBS の検出 (install.ps1 と同じ)
# --------------------------------------------------------------------------
function Get-ObsCandidates {
    $pf = $env:ProgramFiles
    $pf86 = ${env:ProgramFiles(x86)}
    $lad = $env:LOCALAPPDATA
    $sys = $env:SystemDrive

    $c = New-Object System.Collections.Generic.List[object]
    if ($pf) { $c.Add((Join-Path $pf "obs-studio")) }
    if ($pf86) { $c.Add((Join-Path $pf86 "obs-studio")) }
    if ($lad) { $c.Add((Join-Path $lad "Programs\obs-studio")) }
    if ($pf86) { $c.Add((Join-Path $pf86 "Steam\steamapps\common\OBS Studio")) }
    if ($pf) { $c.Add((Join-Path $pf "Steam\steamapps\common\OBS Studio")) }
    if ($sys) { $c.Add((Join-Path $sys "SteamLibrary\steamapps\common\OBS Studio")) }

    # Steam の追加ライブラリ
    $vdfs = New-Object System.Collections.Generic.List[string]
    if ($pf86) { $vdfs.Add((Join-Path $pf86 "Steam\steamapps\libraryfolders.vdf")) }
    if ($pf) { $vdfs.Add((Join-Path $pf "Steam\steamapps\libraryfolders.vdf")) }
    foreach ($vdf in $vdfs) {
        if (-not (Test-Path -LiteralPath $vdf)) { continue }
        foreach ($line in (Get-Content -LiteralPath $vdf -ErrorAction SilentlyContinue)) {
            $m = [regex]::Match($line, '"path"\s+"([^"]+)"')
            if ($m.Success) {
                $lib = $m.Groups[1].Value.Replace('\\', '\')
                $c.Add((Join-Path $lib "steamapps\common\OBS Studio"))
            }
        }
    }

    foreach ($p in @(
            (Join-Path $env:USERPROFILE "Desktop\obs-studio"),
            (Join-Path $env:USERPROFILE "Downloads\obs-studio"),
            (Join-Path $sys "obs-studio"),
            (Join-Path $sys "OBS\obs-studio"))) {
        if ($p) { $c.Add($p) }
    }
    return $c
}

function Remove-Dir([string]$Path, [string]$Why) {
    if (-not $Path) { return $false }
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    try {
        Remove-Item -LiteralPath $Path -Recurse -Force
        Write-Ok "$Why : $Path"
        return $true
    } catch {
        Write-Warn2 "$Why を消せませんでした: $Path"
        Write-Warn2 "  OBS を閉じてから再実行するか、管理者として実行してください。"
        return $false
    }
}

function Remove-InPlace([string]$ObsRoot) {
    $removed = $false
    $dll = Join-Path $ObsRoot "obs-plugins\64bit\$ModuleName.dll"
    $data = Join-Path $ObsRoot "data\obs-plugins\$ModuleName"
    if (Test-Path -LiteralPath $dll) {
        try {
            Remove-Item -LiteralPath $dll -Force
            Write-Ok "OBS : $dll"
            $removed = $true
        } catch {
            Write-Warn2 "消せませんでした: $dll ($_)"
        }
    }
    if (Test-Path -LiteralPath $data) {
        if (Remove-Dir $data "OBS") { $removed = $true }
    }
    return $removed
}

# --------------------------------------------------------------------------
# 0) OBS の起動チェック
# --------------------------------------------------------------------------
Write-Head "$AppName アンインストーラー"

$running = @(Get-Process -Name "obs64" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    if (-not $Force) {
        throw "OBS (obs64) が起動中です。終了してから実行してください。(強行するなら -Force)"
    }
    Write-Warn2 "OBS が起動中ですが -Force が指定されたので強行します。"
}

# --------------------------------------------------------------------------
# 1) 確認
# --------------------------------------------------------------------------
if (-not $Dest) { $Dest = Join-Path $env:ProgramData "obs-studio\plugins\$ModuleName" }

if (-not $Yes) {
    Write-Host ""
    Write-Host "   次を削除します:"
    Write-Host "     $Dest"
    Write-Host "     %APPDATA%\obs-studio\plugin_config\$ModuleName"
    if (-not $NoDetect) {
        Write-Host "     (ポータブル版として入れていれば) 検出した OBS の"
        Write-Host "       obs-plugins\64bit\$ModuleName.dll"
        Write-Host "       data\obs-plugins\$ModuleName"
    }
    Write-Host ""
    $ans = Read-Host "   よろしいですか? [y/N]"
    if ($ans -notmatch '^(?i)y(es)?$') {
        Write-Host "中止しました。"
        return
    }
}

# --------------------------------------------------------------------------
# 2) 配置を消す
# --------------------------------------------------------------------------
$count = 0
Write-Head "削除"

if (Remove-Dir $Dest "配置先") { $count++ }
if (Remove-Dir (Join-Path $env:ProgramData "obs-studio\plugins\$LegacyName") "旧名 (ProgramData)") { $count++ }

# --------------------------------------------------------------------------
# 3) 検出した OBS の obs-plugins / data
# --------------------------------------------------------------------------
if ($NoDetect) {
    Write-Skip "OBS の検出は -NoDetect で省略されました。"
} else {
    $targets = New-Object System.Collections.Generic.List[string]
    $seen = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)
    foreach ($c in (Get-ObsCandidates)) {
        if (-not $c) { continue }
        if (-not (Test-Path -LiteralPath (Join-Path $c "obs-plugins\64bit\$ModuleName.dll")) -and
            -not (Test-Path -LiteralPath (Join-Path $c "data\obs-plugins\$ModuleName"))) { continue }
        $real = (Resolve-Path -LiteralPath $c).Path
        if ($seen.Add($real)) { $targets.Add($real) }
    }
    foreach ($d in $Obs) {
        if (-not $d) { continue }
        if (-not (Test-Path -LiteralPath $d)) {
            Write-Warn2 "-Obs で指定されたフォルダがありません: $d"
            continue
        }
        $real = (Resolve-Path -LiteralPath $d).Path
        if ($seen.Add($real)) { $targets.Add($real) }
    }
    foreach ($t in $targets) {
        if (Remove-InPlace $t) { $count++ }
    }
}

# --------------------------------------------------------------------------
# 4) プラグイン設定
# --------------------------------------------------------------------------
if (Remove-Dir (Join-Path $env:APPDATA "obs-studio\plugin_config\$ModuleName") "plugin_config") { $count++ }
if (Remove-Dir (Join-Path $env:APPDATA "obs-studio\plugin_config\$LegacyName") "plugin_config (旧名)") { $count++ }

# --------------------------------------------------------------------------
# 5) 結果
# --------------------------------------------------------------------------
Write-Head "完了"
if ($count -eq 0) {
    Write-Host "   削除対象は見つかりませんでした (すでにアンインストール済みです)。"
    Write-Host "   別の場所に入れている場合は -Dest (または -Obs) で指定してください。"
} else {
    Write-Host "   $count 個の配置を削除しました。" -ForegroundColor Cyan
    Write-Host "   OBS を起動して、フィルタ一覧に「OBS Noise Suppressor」が" -ForegroundColor Cyan
    Write-Host "   残っていないことを確認してください。" -ForegroundColor Cyan
}
