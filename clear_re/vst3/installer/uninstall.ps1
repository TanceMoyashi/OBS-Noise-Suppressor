#requires -Version 5.1
<#
.SYNOPSIS
    DPDFNet Voice (VST3 版) を削除します。

.DESCRIPTION
    消すのは次のフォルダだけです（他のプラグインには触りません）:

        C:\Program Files\Common Files\VST3\DPDFNet Voice.vst3\   (既定)
        %APPDATA%\VST3\DPDFNet Voice.vst3\                      (-User で入れた場合)

    入れた場所が分からないときは、両方とも試します。-Dest で直接指定することも
    できます。

    コピーできないとき（Program Files を消そうとしたときなど）は、UAC で
    自分自身を管理者として起動し直します（-NoElevate で抑止）。

.PARAMETER Dest
    VST3 のフォルダを直接指定する（`<Dest>\DPDFNet Voice.vst3` を消す）。

.PARAMETER User
    ユーザー単位 (%APPDATA%\VST3) だけを見る。

.PARAMETER NoElevate
    管理者としての起動し直しをしない。

.EXAMPLE
    .\uninstall.ps1
    .\uninstall.ps1 -User
#>
[CmdletBinding()]
param(
    [string]$Dest = "",
    [switch]$User,
    [switch]$NoElevate
)

$ErrorActionPreference = "Stop"
$Bundle = "DPDFNet Voice.vst3"

function Test-IsAdmin {
    return ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

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
        return 1
    }
}

# --- 候補（入れた場所が分からないときは両方見る） ---
$roots = New-Object System.Collections.Generic.List[string]
if ($Dest) {
    $roots.Add($Dest)
} elseif ($User) {
    $roots.Add((Join-Path $env:APPDATA "VST3"))
} else {
    $roots.Add((Join-Path ${env:ProgramFiles} "Common Files\VST3"))
    $roots.Add((Join-Path $env:APPDATA "VST3"))
}

Write-Host ""
Write-Host "== DPDFNet Voice (VST3) を削除します" -ForegroundColor Cyan

$removed = 0
foreach ($r in $roots) {
    $target = Join-Path $r $Bundle
    if (-not (Test-Path -LiteralPath $target)) { continue }
    Write-Host "   削除: $target"
    try {
        Remove-Item -LiteralPath $target -Recurse -Force
        $removed++
        Write-Host "   [OK]" -ForegroundColor Green
    } catch {
        if (-not $NoElevate -and -not (Test-IsAdmin)) {
            Write-Host ""
            Write-Host "[!] 消せませんでした: $($_.Exception.Message)" -ForegroundColor Yellow
            Write-Host "    管理者として実行し直します..." -ForegroundColor Yellow
            exit (Invoke-ElevatedSelf)
        }
        Write-Host "[エラー] 消せませんでした: $($_.Exception.Message)" -ForegroundColor Red
        exit 1
    }
}

if ($removed -eq 0) {
    Write-Host "見つかりませんでした（すでに削除済みか、別の場所に入れています）。" -ForegroundColor Yellow
    Write-Host "  -Dest " + '"D:\VST3"' + " のように指定すると、その場所を見ます。"
} else {
    Write-Host ""
    Write-Host "完了しました（$removed 個）。DAW を起動し直すと一覧から消えます。" -ForegroundColor Green
}
