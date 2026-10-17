#requires -Version 5.1
<#
.SYNOPSIS
    OBS Noise Suppressor を OBS Studio にインストールする。

.DESCRIPTION
    OBS 32 が Windows でプラグインを探す場所は、通常インストーラー版・
    Steam 版・Microsoft Store 版のどれでも同じ 1 か所:

        %ProgramData%\obs-studio\plugins\<モジュール名>\bin\64bit\<モジュール名>.dll
        %ProgramData%\obs-studio\plugins\<モジュール名>\data\

    (obs-studio のソース frontend/widgets/OBSBasic.cpp の AddExtraModulePaths と
     libobs/obs-windows.c の add_default_module_paths で確認済み)

    だから既定のインストール先はエディションに依存せず、管理者権限も
    レジストリも使いません (Microsoft Store 版も非ポータブルなので同じ場所を
    読みます)。

    例外はポータブル版で、こちらは %ProgramData% を読みません。検出できた
    ポータブル版と -Obs で指定されたフォルダには、OBS の既定の相対パス

        <OBS ルート>\obs-plugins\64bit\obs_noise_suppressor.dll
        <OBS ルート>\data\obs-plugins\obs_noise_suppressor\

    にもコピーします。

    このスクリプトは 2 通りの使われ方をします:

      * 配布パッケージ … bin / data が隣にあるので、その中身をコピーするだけ
      * リポジトリ     … target / vendor / models があるのでビルドしてから配置

.PARAMETER Dest
    インストール先。既定 %ProgramData%\obs-studio\plugins\obs_noise_suppressor。
    フォルダ名は DLL 名と一致している必要があります (OBS が
    <フォルダ名>\bin\64bit\<フォルダ名>.dll を組み立てて読むため)。

.PARAMETER Configuration
    Debug か Release (既定 Release)。リポジトリから実行したときだけ使います。

.PARAMETER OrtDll
    同梱する dpdfnet_onnxruntime.dll の場所。省略時は $env:DPDFNET_ORT →
    <obs_plugin>\vendor → <obs_plugin> 直下 → VST3 版の同梱 DLL の順に探します。

.PARAMETER ModelSource
    モデルのコピー元ディレクトリ。既定 <repo>\plugin\models。

.PARAMETER Obs
    追加で配置する OBS のフォルダ (ポータブル版など)。複数指定できます。

.PARAMETER SkipBuild
    ビルドせず、既存の target\<Configuration> を使う。

.PARAMETER SkipModels
    モデルをコピーしない。

.PARAMETER NoDetect
    OBS の検出と、ポータブル版への追加配置をしない。

.PARAMETER Force
    OBS が起動中でも強行する。

.EXAMPLE
    .\install.ps1
    .\install.ps1 -SkipBuild -SkipModels
    .\install.ps1 -Obs "D:\obs-studio"
#>
[CmdletBinding()]
param(
    [Alias("Target")]
    [string]$Dest = "",
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [string]$OrtDll = "",
    [string]$ModelSource = "",
    [string[]]$Obs = @(),
    [switch]$SkipBuild,
    [switch]$SkipModels,
    [switch]$NoDetect,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$ModuleName = "obs_noise_suppressor"
$LegacyName = "dpdfnet_voice"
$AppName = "OBS Noise Suppressor"
$root = $PSScriptRoot
$repo = Split-Path -Parent $root

function Write-Head([string]$t) { Write-Host ""; Write-Host "== $t" -ForegroundColor Cyan }
function Write-Ok([string]$t) { Write-Host "   [OK] $t" -ForegroundColor Green }
function Write-Skip([string]$t) { Write-Host "   [--] $t" -ForegroundColor DarkGray }
function Write-Warn2([string]$t) { Write-Host "   [!!] $t" -ForegroundColor Yellow }

# --------------------------------------------------------------------------
# OBS の検出
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

function Test-PortableObs([string]$Root) {
    foreach ($m in @("portable_mode", "obs_portable_mode", "portable_mode.txt", "obs_portable_mode.txt")) {
        if (Test-Path -LiteralPath (Join-Path $Root $m)) { return $true }
        if (Test-Path -LiteralPath (Join-Path $Root "bin\64bit\$m")) { return $true }
    }
    return $false
}

# --------------------------------------------------------------------------
# ヘルパ
# --------------------------------------------------------------------------
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
    $dirs.Add((Join-Path $root "vendor"))
    $dirs.Add($root)
    $docs = [Environment]::GetFolderPath("MyDocuments")
    if ($docs) { $dirs.Add((Join-Path $docs "OBS\VST3\DPDFNet Voice.vst3\Contents\x86_64-win")) }
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

function Remove-OldInstall([string]$Path, [string]$Why) {
    if (-not $Path) { return }
    if (-not (Test-Path -LiteralPath $Path)) { return }
    try {
        Remove-Item -LiteralPath $Path -Recurse -Force
        Write-Ok "旧名のインストールを削除: $Path ($Why)"
    } catch {
        Write-Warn2 "旧名のフォルダを消せませんでした: $Path ($_)"
    }
}

function Install-InPlace([string]$ObsRoot) {
    $binDst = Join-Path $ObsRoot "obs-plugins\64bit"
    $dataDst = Join-Path $ObsRoot "data\obs-plugins\$ModuleName"
    try {
        New-Item -ItemType Directory -Force -Path $binDst, $dataDst | Out-Null
        Copy-Item -LiteralPath (Join-Path $Payload "$ModuleName.dll") `
            -Destination (Join-Path $binDst "$ModuleName.dll") -Force
        Copy-Item -Path (Join-Path $PayloadData "*") -Destination $dataDst -Recurse -Force
        Write-Ok "追加配置: $ObsRoot"
        return $true
    } catch {
        Write-Warn2 "追加配置に失敗: $ObsRoot ($_)"
        Write-Warn2 "  Program Files などに置く場合は管理者として実行してください。"
        return $false
    }
}

# --------------------------------------------------------------------------
# 0) 形態の判定と OBS の起動チェック
# --------------------------------------------------------------------------
$pkgDll = Join-Path $root "bin\64bit\$ModuleName.dll"
$Packaged = Test-Path -LiteralPath $pkgDll

Write-Head "$AppName インストーラー"
if ($Packaged) {
    Write-Host "   形態: 配布パッケージ ($root)"
} else {
    Write-Host "   形態: リポジトリ ($root)"
}

$running = @(Get-Process -Name "obs64" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    if (-not $Force) {
        throw "OBS (obs64) が起動中です。終了してから実行してください。(強行するなら -Force)"
    }
    Write-Warn2 "OBS が起動中ですが -Force が指定されたので強行します。"
}

# --------------------------------------------------------------------------
# 1) ペイロード (コピー元) を用意する
# --------------------------------------------------------------------------
$Payload = Join-Path $root "bin\64bit"    # dll のあるフォルダ
$PayloadData = Join-Path $root "data"     # models / ORT / manifest のあるフォルダ

if ($Packaged) {
    if (-not (Test-Path -LiteralPath (Join-Path $PayloadData "dpdfnet_onnxruntime.dll"))) {
        throw "同梱の ONNX Runtime がありません: $(Join-Path $PayloadData 'dpdfnet_onnxruntime.dll')"
    }
    if (-not (Test-Path -Path (Join-Path $PayloadData "models\*.onnx"))) {
        throw "同梱のモデルがありません: $(Join-Path $PayloadData 'models')"
    }
    Write-Host "   ペイロード: $root"
} else {
    Write-Head "ビルド"
    $built = Join-Path $root "target\$($Configuration.ToLower())\$ModuleName.dll"
    if (-not $SkipBuild) {
        & (Join-Path $root "build.ps1") -Configuration $Configuration
    }
    if (-not (Test-Path -LiteralPath $built)) {
        throw "ビルド成果物がありません: $built"
    }
    Write-Ok $built

    $Payload = Join-Path $root "payload_tmp\bin\64bit"
    $PayloadData = Join-Path $root "payload_tmp\data"
    if (Test-Path -LiteralPath (Join-Path $root "payload_tmp")) {
        Remove-Item -LiteralPath (Join-Path $root "payload_tmp") -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $Payload, (Join-Path $PayloadData "models") | Out-Null
    Copy-Item -LiteralPath $built -Destination (Join-Path $Payload "$ModuleName.dll") -Force

    $ort = Resolve-OrtDll -Explicit $OrtDll
    if (-not $ort) {
        throw "$ModuleName 用の dpdfnet_onnxruntime.dll が見つかりません。-OrtDll で指定してください。"
    }
    Copy-Item -LiteralPath $ort -Destination (Join-Path $PayloadData "dpdfnet_onnxruntime.dll") -Force
    Write-Ok "ONNX Runtime: $ort"

    if (-not $SkipModels) {
        if (-not $ModelSource) { $ModelSource = Join-Path $repo "plugin\models" }
        if (-not (Test-Path -LiteralPath $ModelSource -PathType Container)) {
            throw "モデルのコピー元がありません: $ModelSource"
        }
        $modelNames = @(
            "dpdfnet8_48khz_hr_2out.onnx",
            "dpdfnet2_48khz_hr_2out.onnx",
            "dpdfnet8_48khz_hr.onnx",
            "dpdfnet2_48khz_hr.onnx"
        )
        $copied = 0
        foreach ($n in $modelNames) {
            $src = Join-Path $ModelSource $n
            if (Test-Path -LiteralPath $src -PathType Leaf) {
                Copy-Item -LiteralPath $src -Destination (Join-Path $PayloadData "models\$n") -Force
                $copied++
            }
        }
        if ($copied -eq 0) { throw "モデルが 1 つもコピーできませんでした: $ModelSource" }
        Write-Ok "モデル: $copied 個"
    }

    # manifest.json (OBS のプラグイン一覧に出す情報)
    $manifest = [ordered]@{
        id               = "obs-noise-suppressor"
        name             = $ModuleName
        display_name     = $AppName
        version          = "0.1.0"
        os_arch          = "win64"
        description      = "Neural speech enhancement (denoise / dereverb) audio filter"
        long_description = "OBS Noise Suppressor - OBS native audio filter (Rust). ONNX Runtime based neural noise suppression and dereverberation (DPDFNet models) for microphone sources, 48 kHz, 6-hop (2880 sample) algorithmic latency."
        urls             = [ordered]@{ repository = ""; website = ""; support = "" }
        has_banner       = $false
        has_icon         = $false
    }
    $manifestPath = Join-Path $PayloadData "manifest.json"
    [System.IO.File]::WriteAllText($manifestPath,
        ($manifest | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
}

# --------------------------------------------------------------------------
# 2) 配置先
# --------------------------------------------------------------------------
if (-not $Dest) { $Dest = Join-Path $env:ProgramData "obs-studio\plugins\$ModuleName" }

Write-Head "配置"
Write-Host "   インストール先: $Dest"

$binDst = Join-Path $Dest "bin\64bit"
$dataDst = Join-Path $Dest "data"
New-Item -ItemType Directory -Force -Path $binDst, (Join-Path $dataDst "models") | Out-Null

Copy-Item -LiteralPath (Join-Path $Payload "$ModuleName.dll") `
    -Destination (Join-Path $binDst "$ModuleName.dll") -Force
Copy-Item -Path (Join-Path $PayloadData "*") -Destination $dataDst -Recurse -Force
Write-Ok "$binDst\$ModuleName.dll"
Write-Ok "$dataDst"

# OBS は <フォルダ名>\bin\64bit\<フォルダ名>.dll を組み立てて読むので、
# フォルダ名が DLL 名と違うと黙って読まれない。
if ((Split-Path -Leaf (Split-Path -Parent $Dest)) -eq "plugins" -and
    (Split-Path -Leaf $Dest) -ne $ModuleName) {
    Write-Warn2 "フォルダ名が DLL 名と違います: $Dest"
    Write-Warn2 "  OBS は読みません (bin\64bit\<フォルダ名>.dll を探すため)。"
}

# --------------------------------------------------------------------------
# 3) 旧名の掃除
# --------------------------------------------------------------------------
# フィルタ ID は据え置き (dpdfnet_voice_filter) なので、旧フォルダが残ると
# 同じフィルタが 2 つ登録され、一覧に 2 つ出てしまう。
Remove-OldInstall (Join-Path $env:ProgramData "obs-studio\plugins\$LegacyName") "ProgramData"
if ($Dest -ne (Join-Path $env:ProgramData "obs-studio\plugins\$ModuleName")) {
    $siblingOld = Join-Path (Split-Path -Parent $Dest) $LegacyName
    if ($siblingOld -ne (Join-Path $env:ProgramData "obs-studio\plugins\$LegacyName")) {
        Remove-OldInstall $siblingOld "配置先の隣"
    }
}

# --------------------------------------------------------------------------
# 4) OBS の検出と、ポータブル版への追加配置
# --------------------------------------------------------------------------
if ($NoDetect) {
    Write-Skip "OBS の検出は -NoDetect で省略されました。"
} else {
    Write-Head "検出した OBS"
    $found = 0
    $portable = New-Object System.Collections.Generic.List[string]
    $seen = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)

    foreach ($c in (Get-ObsCandidates)) {
        if (-not $c.Root) { continue }
        if (-not (Test-Path -LiteralPath (Join-Path $c.Root "bin\64bit\obs64.exe"))) { continue }
        $real = (Resolve-Path -LiteralPath $c.Root).Path
        if (-not $seen.Add($real)) { continue }
        $found++
        if (Test-PortableObs $real) {
            Write-Host "   [検出] $($c.Kind): $real  (ポータブル版)"
            $portable.Add($real)
        } else {
            Write-Host "   [検出] $($c.Kind): $real"
        }
    }
    if ($found -eq 0) { Write-Host "   (見つかりませんでした)" }

    # Microsoft Store 版 (情報のみ)
    try {
        $appx = @(Get-AppxPackage -Name "*OBS*" -ErrorAction SilentlyContinue)
        foreach ($a in $appx) {
            Write-Host "   [検出] Microsoft Store 版: $($a.Name) $($a.Version)"
        }
        if ($appx.Count -gt 0) {
            Write-Host "          (WindowsApps は保護領域なので書き込みません。"
            Write-Host "           非ポータブルなので上の ProgramData の場所を読みます。)"
        }
    } catch {
        Write-Skip "Microsoft Store 版の確認は省略しました (Get-AppxPackage が使えません)。"
    }

    foreach ($d in $Obs) {
        if (-not $d) { continue }
        if (-not (Test-Path -LiteralPath (Join-Path $d "bin\64bit\obs64.exe"))) {
            Write-Warn2 "-Obs で指定されたフォルダに obs64.exe がありません: $d"
            continue
        }
        $portable.Add((Resolve-Path -LiteralPath $d).Path)
    }

    if ($portable.Count -gt 0) {
        Write-Head "ポータブル版への追加配置"
        Write-Host "   ProgramData を読まないため、OBS 側にもコピーします。"
        foreach ($p in ($portable | Select-Object -Unique)) { Install-InPlace $p }
    }
}

# --------------------------------------------------------------------------
# 5) 後片付け
# --------------------------------------------------------------------------
$tmp = Join-Path $root "payload_tmp"
if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force }

Write-Head "完了"
Write-Host "   OBS を起動し、音声ソースの「フィルタ」から" -ForegroundColor Cyan
Write-Host "     OBS Noise Suppressor" -ForegroundColor Cyan
Write-Host "   を追加してください。プロパティの" -ForegroundColor Cyan
Write-Host "     「パネルを開く (ノブ / メーター)」" -ForegroundColor Cyan
Write-Host "   ボタンで UI が開きます。" -ForegroundColor Cyan
