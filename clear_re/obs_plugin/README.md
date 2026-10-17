# DPDFNet Voice — OBS Studio ネイティブ音声フィルタ

音声向けニューラル強調（ノイズ抑圧 + 残響抑圧）を OBS に直接組み込む、**Rust 実装の
OBS 音声フィルタ**です。旧構成（VST3 + atkAudio）を置き換えるもので、DSP は
JUCE 非依存だった VST3 版のコードをそのまま移植しています（`src/dsp/`）。

- フィルタ名（OBS のフィルタ一覧）: **DPDFNet Voice**
- フィルタ ID: `dpdfnet_voice_filter`
- 対応: OBS Studio 32.x / Windows x64

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
- `dpdfnet_voice.dll` の隣、および 2 つ上（`<plugin root>\data` / `data\models`）
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

```powershell
.\install.ps1                    # ビルド + 配置
.\install.ps1 -SkipBuild -SkipModels
```

既定の配置先は `%ProgramData%\obs-studio\plugins\dpdfnet_voice`（管理者権限は不要）。

```text
%ProgramData%\obs-studio\plugins\dpdfnet_voice\
  bin\64bit\dpdfnet_voice.dll        本体
  data\dpdfnet_onnxruntime.dll       ONNX Runtime（同梱）
  data\models\*.onnx                 モデル
  data\manifest.json                 OBS 32 のプラグイン情報
```

> **`onnxruntime.dll` という名前で置いてはいけません。**
> OBS のプロセスには他プラグイン（atkAudio 等）が System32 の ORT 1.17 を既に
> ロードしていることがあり、名前だけで解決するとそちらを掴んで即死します。
> 本プラグインは必ず絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH` で
> `dpdfnet_onnxruntime.dll` を読むので、この名前で同梱しています。

インストール後は OBS を再起動してください（OBS の起動中は DLL を上書きできません）。

## 使い方

1. フィルタをかけたいソース（マイク・メディアソースなど）を右クリック → **フィルタ**
2. `+` → **DPDFNet Voice** を追加
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
  build.ps1 / install.ps1      ビルドとインストール
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
    make_verify_collection.py  実機検証用のシーンコレクション生成
    run_obs_verify.ps1         実機で回して数値で確認するハーネス
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
- 本リポジトリの自作コードのライセンスは現状**未設定**です（推奨: GPL-2.0-or-later）。

