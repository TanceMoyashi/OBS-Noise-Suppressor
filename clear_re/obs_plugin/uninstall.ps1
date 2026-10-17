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

    前回「管理者として」インストールしていた場合、ProgramData のファイルが
    Administrators 所有になっていて非昇格では消せません。そのときは自動で
    自分自身を管理者として起動し直します (-NoElevate で抑止)。

.PARAMETER Dest
    消す配置先。既定 %ProgramData%\obs-studio\plugins\obs_noise_suppressor。

.PARAMETER Obs
    追加で対象にする OBS のフォルダ (自動検出から漏れるとき)。

.PARAMETER Yes
    確認なしで削除する。

.PARAMETER NoDetect
    OBS の検出をしない (ProgramData と -Dest だけ消す)。

.PARAMETER NoElevate
    消せないときに管理者として起動し直さない (メッセージだけ出す)。

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
    [switch]$NoElevate,
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
# 権限まわり
# --------------------------------------------------------------------------
function Test-IsAdmin {
    try {
        $id = [Security.Principal.WindowsIdentity]::GetCurrent()
        return (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
            [Security.Principal.WindowsBuiltInRole]::Administrator)
    } catch { return $false }
}

# 消せるかどうかの目安。ProgramData のプラグインファイルは作成したユーザーに
# しか書き込み権をくれない (Users は RX だけ) ので、管理者が作ったファイルは
# 非昇格では消せない。
function Test-FileWritable([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $true }
    try {
        $fs = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
        $fs.Close()
        return $true
    } catch { return $false }
}

function Invoke-ElevatedSelf {
    param([System.Collections.IDictionary]$Bound)
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", ('"{0}"' -f $PSCommandPath))
    foreach ($k in $Bound.Keys) {
        if ($k -eq "NoElevate") { continue }
        $v = $Bound[$k]
        if ($v -is [switch]) {
            if ($v.IsPresent) { $argList += "-$k" }
        } elseif ($v -is [System.Array]) {
            foreach ($x in $v) { $argList += @("-$k", ('"{0}"' -f $x)) }
        } else {
            $argList += @("-$k", ('"{0}"' -f $v))
        }
    }
    Write-Host ("   powershell.exe " + ($argList -join " ")) -ForegroundColor DarkGray
    try {
        $p = Start-Process -FilePath "powershell.exe" -ArgumentList $argList `
            -Verb RunAs -Wait -PassThru
        return $p.ExitCode
    } catch {
        Write-Warn2 "昇格できませんでした: $($_.Exception.Message)"
        Write-Warn2 "  管理者として実行し直すか、uninstall.bat を右クリック →"
        Write-Warn2 "  「管理者として実行」してください。"
        return 1
    }
}

# --------------------------------------------------------------------------
# OBS の検出 (install.ps1 と同じ)
# --------------------------------------------------------------------------
function Get-ObsCandidates {
    $pf = $env:ProgramFiles
    $pf86 = ${env:ProgramFiles(x86)}
    $lad = $env:LOCALAPPDATA
    $sys = $env:SystemDrive

    $c = New-Object System.Collections.Generic.List[object]
    if ($pf) { $c.Add([pscustomobject]@{ Kind = "通常インストーラー版 (Program Files)"; Root = (Join-Path $pf "obs-studio") }) }
    if ($pf86) { $c.Add([pscustomobject]@{ Kind = "通常インストーラー版 (Program Files x86)"; Root = (Join-Path $pf86 "obs-studio") }) }
    if ($lad) { $c.Add([pscustomobject]@{ Kind = "通常インストーラー版 (ユーザー別)"; Root = (Join-Path $lad "Programs\obs-studio") }) }

    $steamRoots = New-Object System.Collections.Generic.List[string]
    if ($pf86) {
        $steamRoots.Add((Join-Path $pf86 "Steam"))
        $c.Add([pscustomobject]@{ Kind = "Steam"; Root = (Join-Path $pf86 "Steam\steamapps\common\OBS Studio") })
    }
    if ($pf) {
        $steamRoots.Add((Join-Path $pf "Steam"))
        $c.Add([pscustomobject]@{ Kind = "Steam"; Root = (Join-Path $pf "Steam\steamapps\common\OBS Studio") })
    }
    if ($sys) { $c.Add([pscustomobject]@{ Kind = "Steam"; Root = (Join-Path $sys "SteamLibrary\steamapps\common\OBS Studio") }) }

    # Steam の追加ライブラリ (libraryfolders.vdf の中の絶対パス)
    foreach ($sr in $steamRoots) {
        $vdf = Join-Path $sr "steamapps\libraryfolders.vdf"
        if (-not (Test-Path -LiteralPath $vdf)) { continue }
        foreach ($line in (Get-Content -LiteralPath $vdf -ErrorAction SilentlyContinue)) {
            $m = [regex]::Match($line, '"path"\s+"([^"]+)"')
            if ($m.Success) {
                $lib = $m.Groups[1].Value.Replace('\\', '\')
                $c.Add([pscustomobject]@{ Kind = "Steam ライブラリ"; Root = (Join-Path $lib "steamapps\common\OBS Studio") })
            }
        }
    }

    # ポータブル版のありそうな場所
    foreach ($p in @(
            (Join-Path $env:USERPROFILE "Desktop\obs-studio"),
            (Join-Path $env:USERPROFILE "Downloads\obs-studio"),
            (Join-Path $sys "obs-studio"),
            (Join-Path $sys "OBS\obs-studio"))) {
        if ($p) { $c.Add([pscustomobject]@{ Kind = "ポータブル版"; Root = $p }) }
    }
    return $c
}

function Get-InPlaceTargets {
    $targets = New-Object System.Collections.Generic.List[string]
    $seen = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)
    foreach ($c in (Get-ObsCandidates)) {
        if (-not $c.Root) { continue }
        if (-not (Test-Path -LiteralPath (Join-Path $c.Root "obs-plugins\64bit\$ModuleName.dll")) -and
            -not (Test-Path -LiteralPath (Join-Path $c.Root "data\obs-plugins\$ModuleName"))) { continue }
        $real = (Resolve-Path -LiteralPath $c.Root).Path
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
    return $targets
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

# ==========================================================================
# 0) OBS の起動チェック
# ==========================================================================
Write-Head "$AppName アンインストーラー"

$running = @(Get-Process -Name "obs64" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    if (-not $Force) {
        throw "OBS (obs64) が起動中です。終了してから実行してください。(強行するなら -Force)"
    }
    Write-Warn2 "OBS が起動中ですが -Force が指定されたので強行します。"
}

# ==========================================================================
# 1) 消す対象を決める
# ==========================================================================
if (-not $Dest) { $Dest = Join-Path $env:ProgramData "obs-studio\plugins\$ModuleName" }

$legacyDest = Join-Path $env:ProgramData "obs-studio\plugins\$LegacyName"
$configDir = Join-Path $env:APPDATA "obs-studio\plugin_config\$ModuleName"
$legacyConfig = Join-Path $env:APPDATA "obs-studio\plugin_config\$LegacyName"

$inPlace = @()
if (-not $NoDetect) { $inPlace = @(Get-InPlaceTargets) }

# ==========================================================================
# 2) 消せるか確認する (必要なら昇格し直す)
# ==========================================================================
$needAdmin = @()
foreach ($p in @((Join-Path $Dest "bin\64bit\$ModuleName.dll"), $Dest)) {
    if ((Test-Path -LiteralPath $p) -and -not (Test-FileWritable $p)) { $needAdmin += $p }
}
foreach ($t in $inPlace) {
    $p = Join-Path $t "obs-plugins\64bit\$ModuleName.dll"
    if ((Test-Path -LiteralPath $p) -and -not (Test-FileWritable $p)) { $needAdmin += $p }
}

if ($needAdmin.Count -gt 0) {
    if (-not $NoElevate -and -not (Test-IsAdmin)) {
        Write-Warn2 "前回「管理者として」インストールしたファイルがあり、非昇格では消せません。"
        Write-Warn2 "管理者として実行し直します..."
        exit (Invoke-ElevatedSelf -Bound $PSBoundParameters)
    }
    Write-Warn2 "次のファイルは管理者権限がないと消せない可能性があります:"
    foreach ($p in $needAdmin) { Write-Warn2 "  $p" }
    Write-Warn2 "  (uninstall.bat を右クリック → 「管理者として実行」)"
}

# ==========================================================================
# 3) 確認
# ==========================================================================
if (-not $Yes) {
    Write-Host ""
    Write-Host "   次を削除します:"
    Write-Host "     $Dest"
    Write-Host "     %APPDATA%\obs-studio\plugin_config\$ModuleName"
    foreach ($t in $inPlace) {
        Write-Host "     (追加配置した OBS) $t"
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

# ==========================================================================
# 4) 配置を消す
# ==========================================================================
$count = 0
Write-Head "削除"

if (Remove-Dir $Dest "配置先") { $count++ }
if (Remove-Dir $legacyDest "旧名 (ProgramData)") { $count++ }

if ($NoDetect) {
    Write-Skip "OBS の検出は -NoDetect で省略されました。"
} else {
    foreach ($t in $inPlace) {
        if (Remove-InPlace $t) { $count++ }
    }
}

# ==========================================================================
# 5) プラグイン設定
# ==========================================================================
if (Remove-Dir $configDir "plugin_config") { $count++ }
if (Remove-Dir $legacyConfig "plugin_config (旧名)") { $count++ }

# ==========================================================================
# 6) 結果
# ==========================================================================
Write-Head "完了"
if ($count -eq 0) {
    Write-Host "   削除対象は見つかりませんでした (すでにアンインストール済みです)。"
    Write-Host "   別の場所に入れている場合は -Dest (または -Obs) で指定してください。"
} else {
    Write-Host "   $count 個の配置を削除しました。" -ForegroundColor Cyan
    Write-Host "   OBS を起動して、フィルタ一覧に「OBS Noise Suppressor」が" -ForegroundColor Cyan
    Write-Host "   残っていないことを確認してください。" -ForegroundColor Cyan
}
