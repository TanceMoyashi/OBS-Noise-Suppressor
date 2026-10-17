# OBS Noise Suppressor — OBS Studio ネイティブ音声フィルタ

音声向けニューラル強調（ノイズ抑圧 + 残響抑圧）を OBS に直接組み込む、**Rust 実装の
OBS 音声フィルタ**です。旧構成（VST3 + atkAudio）を置き換えるもので、DSP は
JUCE 非依存だった VST3 版のコードをそのまま移植しています（`src/dsp/`）。
推論には DPDFNet モデルを使います。

- フィルタ名（OBS のフィルタ一覧）: **OBS Noise Suppressor**
- フィルタ ID: `dpdfnet_voice_filter`（**据え置き**。変えると既存シーンの
  フィルタが消えるため）
- 対応: OBS Studio 32.x / Windows x64

> 旧名は **DPDFNet Voice** でした。プロジェクト名・モジュール名・DLL 名・
> インストール先・表示名を `OBS Noise Suppressor` / `obs_noise_suppressor` に
> 変更していますが、**フィルタ ID は据え置き**なので既存のシーンコレクションは
> そのまま動きます。旧フォルダが残ると同じフィルタが二重に登録されるため、
> `install.ps1` / `install.bat` は旧名のフォルダを自動で削除します。

## 特徴

| 項目 | 内容 |
| --- | --- |
| サンプルレート | **48 kHz 専用**。それ以外のレートではパススルー（音は素通し） |
| チャンネル | OBS の音声設定に追従（モノラル → モノラル、ステレオ → ステレオ） |
| 内容遅延 | **2880 サンプル = 60 ms 固定**（`kLatencyHops = 6` × hop 480。VST3 版と同一） |
| 推論 | 同梱の ONNX Runtime 1.31.0（`dpdfnet_onnxruntime.dll`） |
| UI | 自前の Win32/GDI パネル（ノブ 3 個 + 3 バンドメーター + BYPASS）。VST3 版とほぼ同じ見た目 |
| 音声スレッド | アロケーションなし（テンソル・バッファは事前確保、`IoBinding` で再利用） |
| パニック | FFI 境界ごとに `catch_unwind`。落ちずにパススルーへ退避 |

## 動作要件

- OBS Studio 32.x（`obs64.exe`）
- OBS の音声設定が **48 kHz**（設定 → 音声 → サンプルレート）
- `dpdfnet_onnxruntime.dll`（同梱）と DPDFNet モデル（`*.onnx`）
- ビルドする場合のみ: Rust (MSVC ツールチェーン) + Visual Studio の C++ ビルドツール

モデルは次の優先順で探します（`src/paths.rs`）。

1. `dpdfnet8_48khz_hr_2out.onnx`（品質優先・Voice Reverb 可）
2. `dpdfnet2_48khz_hr_2out.onnx`（速度優先・Voice Reverb 可）
3. `dpdfnet8_48khz_hr.onnx`
4. `dpdfnet2_48khz_hr.onnx`

探索先（`DPDFNET_MODEL` で明示指定も可）:

- OBS のモジュールデータディレクトリ（＝ `data\`）とその `models\`
- `obs_noise_suppressor.dll` の隣、および 2 つ上（`<plugin root>\data` / `data\models`）
- `ドキュメント\DPDFNetVoice\models`
- `ドキュメント\OBS\VST3\models`（旧 VST3 版の配置をそのまま使える）

## ビルド

```powershell
.\build.ps1                      # cargo build --release
.\build.ps1 -Tests               # テストを回す
.\build.ps1 -Tests -TestFilter dsp::offline_tests -NoCapture
```

`build.ps1` は vcvars64 を探して読み込んでから cargo を呼びます。

## インストール

### 配布パッケージを使う（通常はこちら）

```powershell
.\tools\make_package.ps1         # dist\OBS-Noise-Suppressor\ と ZIP を作る
```

できた `dist\OBS-Noise-Suppressor\install.bat` をダブルクリックします
（中身は `install.ps1`）。削除は `uninstall.bat` です。

```text
dist\OBS-Noise-Suppressor\
  install.bat / install.ps1          インストーラー
  uninstall.bat / uninstall.ps1      アンインストーラー
  README.txt                         使い方（日本語）
  bin\64bit\obs_noise_suppressor.dll 本体
  data\dpdfnet_onnxruntime.dll       ONNX Runtime（同梱）
  data\models\*.onnx                 モデル
  data\manifest.json                 OBS のプラグイン情報
```

### 配置先

既定は `%ProgramData%\obs-studio\plugins\obs_noise_suppressor` です。
**管理者権限もレジストリも使いません**（レジストリは読み書きしません）。

```text
%ProgramData%\obs-studio\plugins\obs_noise_suppressor\
  bin\64bit\obs_noise_suppressor.dll   本体
  data\dpdfnet_onnxruntime.dll         ONNX Runtime（同梱）
  data\models\*.onnx                   モデル
  data\manifest.json                   OBS のプラグイン情報
```

> **フォルダ名は DLL 名と一致させてください。**
> OBS は `<フォルダ名>\bin\64bit\<フォルダ名>.dll` を組み立てて読みます。

**インストール先は OBS のエディションに依存しません。** OBS 32 が Windows で
プラグインを探す場所は、通常インストーラー版・Steam 版・Microsoft Store 版の
どれでも次の 1 か所だからです（確認元: OBS ソース
`frontend/widgets/OBSBasic.cpp` の `AddExtraModulePaths()` と
`libobs/obs-windows.c` の `add_default_module_paths()`）。

**例外はポータブル版だけ**です。ポータブル版は `%ProgramData%` を読まないので、
`install.bat` はポータブル版を検出したとき、OBS 側の既定の相対パス

```text
<OBS ルート>\obs-plugins\64bit\obs_noise_suppressor.dll
<OBS ルート>\data\obs-plugins\obs_noise_suppressor\
```

にもコピーします。自動検出から漏れる場合は `install.bat -Obs "D:\obs"` のように
指定してください。

> **一度でも「管理者として実行」したときの注意**
> `%ProgramData%\obs-studio\plugins\` のファイルは、作成したユーザーにしか書き込み権を
> 与えません（`Users` は読み取り専用）。だから管理者として入れたファイルは、非昇格では
> 上書きも削除もできません（`アクセスが拒否されました`）。
> `install.ps1` / `uninstall.ps1` はこれを検出すると**自動で自分自身を管理者として
> 起動し直します**（UAC を出したくない場合は `-NoElevate`。そのときはメッセージに
> 従って手動で管理者実行してください）。逆に、非昇格で新しく入れたファイルは自分の
> 所有になるので、以後の更新も非昇格のまま通ります。

### リポジトリから直接（開発時）

```powershell
.\install.ps1                    # ビルド + 配置（OBS の検出もする）
.\install.ps1 -SkipBuild -SkipModels
```

`install.ps1` は、隣に `bin` / `data` があれば配布パッケージとして、なければ
リポジトリとして（ビルドしてから）配置します。

### インストーラーが BAT ではなく PowerShell な理由

`install.bat` / `uninstall.bat` は**ダブルクリック用の ASCII だけのランチャー**で、
実体は `install.ps1` / `uninstall.ps1` です。純バッチでも書けますが、**cmd.exe は
マルチバイト文字を含むバッチファイルで行を見失うことがあります**（ラベルと
`goto` が多いほど起きやすい）。実測では、日本語を直接書いた純バッチで

- UTF-8 + `chcp 65001`: 264 行中 5 行が壊れ、コメントの一部がコマンドとして実行された
- CP932（`chcp` なし）: 1 行が壊れた

という結果でした。PowerShell は UTF-8 (BOM) のスクリプトを正しく読むので、
日本語のメッセージはそちらに置いています。`-ExecutionPolicy Bypass` で起動するので
実行ポリシーの影響も受けません。

> **`onnxruntime.dll` という名前で置いてはいけません。**
> OBS のプロセスには他プラグイン（atkAudio 等）が System32 の ORT 1.17 を既に
> ロードしていることがあり、名前だけで解決するとそちらを掴んで即死します。
> 本プラグインは必ず絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH` で
> `dpdfnet_onnxruntime.dll` を読むので、この名前で同梱しています。

インストール後は OBS を再起動してください（OBS の起動中は DLL を上書きできません）。

## 使い方

1. フィルタをかけたいソース（マイク・メディアソースなど）を右クリック → **フィルタ**
2. `+` → **OBS Noise Suppressor** を追加
3. プロパティの **「パネルを開く（ノブ / メーター）」** ボタンで自前パネルが開きます

### プロパティ（OBS 標準 UI）

| 項目 | 範囲 | 既定 | 意味 |
| --- | --- | --- | --- |
| Ambience | 0 – 1 | 0.5 | 残す暗騒音。0 % = 最大抑圧 (−48 dB)、100 % = 最小抑圧 (−6 dB) |
| Voice | −12 – +12 dB | 0.0 | 声の presence（高域の色づけ）。0 dB で完全なパススルー |
| Voice Reverb | 0 – 1 | 0.0 | 残す残響。**2 出力モデルのときだけ有効** |

プロパティの説明欄には、現在の状態（48 kHz / チャンネル数 / 遅延 / モデル名）が
そのまま出ます。困ったときはまずここを見てください。

### パネル（自前 Win32 UI）

- **AMBIENCE / VOICE / REVERB** の 3 ノブ（ドラッグで変更、ホイールでも可）
- **MID / HIGH / HIGHER** の 3 バンドメーター（ピークマーカー付き）
- 左下にステータスドット + 1 行の状態表示
- 右下の **BYPASS** ボタン

**Bypass は OBS 側の「フィルタを有効化」にそのまま対応します**（別物のバイパスを
持たないので、OBS のフィルタ一覧のチェックボックスと常に同期します）。
バイパス中は**完全な 0 遅延の素通し**です（遅延は維持しません。VST3 版と同じ）。

### 遅延について

内容遅延は **2880 サンプル（48 kHz で 60 ms）** で固定です。
OBS には音声フィルタ用の汎用遅延補正がないため、同じシーンでフィルタ無しの
ソースと混ぜると 60 ms ずれて聞こえます。マイク 1 本を通すだけの使い方
（自分の声を直接モニターしない）なら実害はありません。

## ファイル構成

```text
obs_plugin/
  build.ps1 / install.ps1        ビルドとインストール（開発時）
  uninstall.ps1                  アンインストーラー
  installer/
    install.bat / uninstall.bat  ダブルクリック用ランチャー（ASCII のみ）
  vendor/
    dpdfnet_onnxruntime.dll      同梱する ONNX Runtime（MIT）
  src/
    lib.rs                     obs_module_load など OBS モジュールのエクスポート
    ffi.rs                     手書きの libobs FFI（bindgen なし）
    filter.rs                  obs_source_info コールバック本体 / スレッドモデル
    ui.rs                      自前 Win32 パネル（GDI で全部描く）+ スナップショットテスト
    paths.rs                   自分の DLL / ORT / モデルの探索
    dump.rs                    開発用: 実機の入出力を WAV に落とす（環境変数で有効化）
    dsp/
      engine.rs                ホップスケジューラ + メーター + パラメータ適用
      net.rs                   ONNX Runtime セッション（IoBinding、事前確保バッファ）
      ort.rs                   同梱 ONNX Runtime の動的ロード
      stft.rs                  Vorbis 窓の STFT（完全再構成）
      meta.rs                  モデルメタデータの検証
      params.rs                パラメータ定義
      offline_tests.rs         OBS を起動しない検証（遅延・一致・決定性）
  tools/
    make_package.ps1           配布パッケージ（install.bat 同梱）を作る
    make_verify_collection.py  実機検証用のシーンコレクション生成
    run_obs_verify.ps1         実機で回して数値で確認するハーネス
    gen_obs_lib.ps1            vendor\obs.lib を obs.dll から作り直す
```

## 検証

実機での確認手順は [`OBS_VERIFY.md`](OBS_VERIFY.md) を参照してください。
OBS を起動せずに済む DSP の検証は `build.ps1 -Tests` で回せます。

## 旧 VST3 版からの移行

- VST3 版（`clear_re/plugin/`）はもう保守しません。OBS ではこのプラグインを使います。
- 旧 VST3 の同梱 ORT とモデルはそのまま流用できます（探索先に入れてあります）。
  atkAudio の VST3 検索パス設定は不要になります。

## ライセンス / クレジット

本プラグインが利用・同梱しているものは次のとおりです。**同梱物のライセンスは
それぞれの配布元の条項に従います。**

| コンポーネント | 用途 | ライセンス | 配布元 |
| --- | --- | --- | --- |
| DPDFNet ONNX モデル（`models/*.onnx`） | 音声強調の推論 | Apache-2.0 | `Ceva-IP/DPDFNet`（Hugging Face） |
| ONNX Runtime 1.31.0（`dpdfnet_onnxruntime.dll`） | 推論エンジン | MIT | Microsoft |
| OBS Studio / libobs | ホスト。`obs.dll` の C API を利用 | GPL-2.0-or-later | OBS Project |

- モデルを再配布する場合は Apache-2.0 の条件（著作権表示・ライセンス文の同梱、
  変更の明示）を満たしてください。
- OBS 本体は GPL-2.0-or-later です。libobs の API にリンクして配布するプラグインは、
  互換ライセンス（例: GPL-2.0-or-later）を付けるのが安全です。
- 本リポジトリの自作コード（Rust のプラグイン本体・PowerShell / Python の
  ツール類）は **GPL-2.0-or-later** です。全文はリポジトリ直下の [`LICENSE`](../../LICENSE)
  にあります。`install.bat` で入る配布パッケージにも `LICENSE` を同梱しています。

