#requires -Version 5.1
<#
.SYNOPSIS
    インストール済み OBS の obs.dll から、Rust リンク用のインポートライブラリ
    vendor\obs.lib を生成する。

.DESCRIPTION
    OBS の C API は手書き FFI（src/ffi.rs）で呼ぶため、リンクに必要なのは
    obs.dll がエクスポートしているシンボルだけ。OBS SDK のヘッダ/.lib は要らない。

    dumpbin で obs.dll のエクスポートテーブルを取り出し、.def を経由して
    lib.exe（VC ツールチェーン）でインポートライブラリを作る。

    OBS 本体を更新したら再実行する。build.rs が vendor\obs.lib をリンクし、
    ファイルが無いとビルドが失敗する。

.PARAMETER ObsDll
    元にする obs.dll。既定は C:\Program Files\obs-studio\bin\64bit\obs.dll。

.PARAMETER Out
    出力先。既定は <obs_plugin>\vendor\obs.lib。

.EXAMPLE
    .\tools\gen_obs_lib.ps1
    .\tools\gen_obs_lib.ps1 -ObsDll "C:\OBS\bin\64bit\obs.dll"
#>
[CmdletBinding()]
param(
    [string]$ObsDll = "",
    [string]$Out = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot    # obs_plugin

function Find-VcVars {
    $candidates = New-Object System.Collections.Generic.List[string]

    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere) {
        $vs = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        if ($vs) {
            $candidates.Add((Join-Path $vs "VC\Auxiliary\Build\vcvars64.bat"))
        }
    }

    foreach ($v in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if (-not $v) { continue }
        Get-ChildItem -Path (Join-Path $v "Microsoft Visual Studio") -Filter "vcvars64.bat" `
            -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\VC\\Auxiliary\\Build\\' } |
            ForEach-Object { $candidates.Add($_.FullName) }
    }

    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) { return $c }
    }
    return $null
}

# --- 1) obs.dll を決める ---
if (-not $ObsDll) {
    $ObsDll = @(
        (Join-Path ${env:ProgramFiles} "obs-studio\bin\64bit\obs.dll")
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (-not $ObsDll -or -not (Test-Path -LiteralPath $ObsDll)) {
    throw "obs.dll が見つかりません。-ObsDll で指定してください。"
}
$ObsDll = (Resolve-Path -LiteralPath $ObsDll).Path

if (-not $Out) { $Out = Join-Path $root "vendor\obs.lib" }

# --- 2) VC ツールチェーン (dumpbin / lib) ---
$vcvars = Find-VcVars
if (-not $vcvars) {
    throw "vcvars64.bat が見つかりません。Visual Studio の C++ ビルドツールを入れてください。"
}
$envLines = & cmd.exe /c "`"$vcvars`" >nul 2>&1 && set"
foreach ($line in $envLines) {
    if ($line -match '^([^=]+)=(.*)$') {
        [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
    }
}
$dumpbin = (Get-Command dumpbin.exe -ErrorAction SilentlyContinue).Source
$libexe = (Get-Command lib.exe -ErrorAction SilentlyContinue).Source
if (-not $dumpbin -or -not $libexe) {
    throw "dumpbin / lib が PATH にありません（VC ツールチェーン）。"
}

# --- 3) エクスポートを取り出す ---
Write-Host "==> $ObsDll のエクスポートを読み取り中..." -ForegroundColor Cyan
$dump = & $dumpbin /nologo /exports $ObsDll
$names = New-Object System.Collections.Generic.List[string]
foreach ($line in $dump) {
    # "      1    0 0007ED90 array_output_serializer_free = array_output_serializer_free"
    if ($line -match '^\s+\d+\s+[0-9A-Fa-f]+\s+[0-9A-Fa-f]+\s+([^\s=]+)') {
        $names.Add($matches[1])
    }
}
if ($names.Count -eq 0) {
    throw "エクスポートを 1 つも取得できませんでした: $ObsDll"
}

# --- 4) .def を作って lib.exe にかける（.exp が散らからないよう一時ディレクトリで） ---
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("obs_lib_" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $work | Out-Null
try {
    $defPath = Join-Path $work "obs.def"
    $def = New-Object System.Collections.Generic.List[string]
    $def.Add("LIBRARY obs")
    $def.Add("EXPORTS")
    foreach ($n in $names) { $def.Add("    $n") }
    [System.IO.File]::WriteAllLines($defPath, $def, (New-Object System.Text.ASCIIEncoding))

    $built = Join-Path $work "obs.lib"
    & $libexe /nologo /def:"$defPath" /machine:x64 /out:"$built"
    if ($LASTEXITCODE -ne 0) {
        throw "lib.exe が失敗しました (exit $LASTEXITCODE)"
    }

    $outDir = Split-Path -Parent $Out
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    Copy-Item -LiteralPath $built -Destination $Out -Force
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "生成しました: $Out" -ForegroundColor Green
Write-Host "  シンボル数: $($names.Count)" -ForegroundColor Green
