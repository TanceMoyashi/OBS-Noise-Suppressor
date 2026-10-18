# OBS Noise Suppressor

**OBS Studio 用のネイティブ音声フィルタ（ノイズ抑圧 + 残響抑圧）** です。
ONNX Runtime と DPDFNet モデルを同梱した Windows x64 版で、追加のダウンロードは
要りません。プラグイン ABI・DSP・パネル UI まで**すべて Rust** で書いています。

![パネル（OBS 実機で音を流した状態）](https://github.com/TanceMoyashi/OBS-Noise-Suppressor/raw/main/docs/panel.png)

> 旧 VST3 版 **DPDFNet Voice** を OBS ネイティブに作り直したものです。フィルタ ID
> （`dpdfnet_voice_filter`）は据え置きなので、既存のシーンコレクションはそのまま動きます。

## AI による制作について

> [!IMPORTANT]
> このプロジェクトは **AI コーディング（AI がコードを生成）** を主体に制作されています。
> コードの生成と改修に使ったモデルは **DeepSeek V4.1 Flash Free** です
> （プロバイダ = `opencode` / モデル ID = `opencode/deepseek-v4.1-flash-free` / 実行環境 = OpenCode）。
> 設計方針の決定・仕様の指示・実機検証の判断は人間が行い、実装は AI エージェントが担当しています。

> [!WARNING]
> **その前提を理解したうえで利用してください。**
>
> - **無保証です。** ライセンス（GPL-2.0-or-later）の "NO WARRANTY" 条項どおり、
>   動作・品質・安全性について一切保証しません。**自己責任**で使ってください。
> - **AI の出力は鵜呑みにできません。** 誤り・見落とし・意図しない挙動が残っている
>   可能性があります。人間によるレビューと下記の実機検証は行っていますが、それは
>   「検証した範囲では正しい」という意味にすぎません。
> - **本番で使う前に自分で確かめてください。** 配信・録音・業務など重要な用途では、
>   事前に自分の環境でテストし、必要なら検証・修正してください。特に
>   **60 ms 固定の遅延**（＋レート変換ぶん数 ms）という前提が
>   用途に合うかを確認してください。
> - **権利関係も保証できません。** AI が生成したコードが第三者の著作権を侵害しない
>   ことを保証できません。再配布・商用利用の前に各自で確認してください。同梱の
>   ONNX Runtime と DPDFNet モデルには、それぞれ別のライセンスがかかります（後述）。
> - **音質は同梱モデル依存です。** 抑圧の効き方・残響の取れ方は DPDFNet モデルと
>   ONNX Runtime の推論結果そのもので、このプラグインが品質を保証するものではありません。

## 特徴

| 項目 | 内容 |
| --- | --- |
| サンプルレート | **44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz など**。48 kHz 以外は入出力でレート変換して内部 48 kHz で処理（変換段を作れない異常値のみパススルー） |
| チャンネル | モノラル → モノラル / ステレオ → ステレオ（OBS の音声設定に追従） |
| 内容遅延 | **2880 サンプル = 60 ms 固定**（`kLatencyHops = 6` × hop 480。VST3 版と同一）。48 kHz 以外ではレート変換ぶん（約 2〜5 ms）が加算 |
| 推論 | 同梱の ONNX Runtime 1.31.0（`dpdfnet_onnxruntime.dll`）+ DPDFNet モデル 4 つ |
| モデル選択 | OBS プロパティで **自動 / 高品質 (dp8) / 軽量 (dp2)** を選択 |
| UI | 自前の Win32/GDI パネル（入力 VU 2 本 + 縦フェーダー 3 本 + GR / 3 バンドメーター） |
| プリセット | 1〜8 のスロット（`presets.json` に保存） |
| VU 基準値 | −20 / −18 / −14 dBFS（既定 −18 = EBU）。目盛りが対応 dBFS ごと平行移動 |
| 数値表示 | フェーダーの dB と %、VU の現在値 / ピーク dBFS、GR の抑圧量 dB |
| インストーラー | レジストリ不使用 / 管理者権限不要 / アンインストーラー同梱 |
| 音声スレッド | アロケーションなし（テンソル・バッファは事前確保し `IoBinding` で再利用） |
| 堅牢性 | FFI 境界ごとに `catch_unwind`。落ちずにパススルーへ退避 |
| ログ | エラー / 警告 / 起動情報を専用ファイルに常時記録（OBS 標準ログにも同じ内容が出ます） |

## パネル

OBS のプロパティにある **「パネルを開く（フェーダー / メーター / メモリー）」** で開く、
自前描画のウィンドウです（画像素材やテーマファイルは使っていません）。

| セクション | 内容 |
| --- | --- |
| ヘッダ | タイトル + **メモリー 1〜8**（スロット押下 = 呼び出し / `SAVE` → スロット = 保存 / `Ctrl` + クリック = 消去） |
| INPUT | 入力 L / R の**アナログ VU メーター**（実機準拠の弾道、300 ms で 99 %、行き過ぎなし）と現在値 / ピーク dBFS |
| CONTROL | **縦フェーダー 3 本**（AMBIENCE / VOICE / REVERB）。ダブルクリックで既定値に戻ります |
| EFFECT | **GR メーター**（抑圧量 dB）と出力 **3 バンド（L / M / H）** メーター |
| フッタ | ステータス表示、モデル刻印、**BYPASS**（OBS のフィルタ有効/無効とそのまま同期） |

見た目は実機の業務用機器に寄せています（黒アルマイトの面板 + ヘアライン + 十字ネジ、
アンバーに灯る VU 盤面 + 0 〜 +3 の赤いオーバーロード帯、フェーダーの金属レール）。

## インストール

1. [Releases](https://github.com/TanceMoyashi/OBS-Noise-Suppressor/releases) から
   `OBS-Noise-Suppressor-x.y.z-win64.zip` をダウンロードして展開します。
2. **OBS を終了**して `install.bat` をダブルクリックします（中身は `install.ps1`）。
3. OBS を起動し、音声ソースの「フィルタ」→「OBS Noise Suppressor」を追加します。
4. プロパティの「パネルを開く」でパネルが開きます。

配置先は `%ProgramData%\obs-studio\plugins\obs_noise_suppressor\`
（通常インストーラー版 / Steam 版 / Microsoft Store 版で共通）。ポータブル版の OBS を
検出できた場合は `<OBS>\obs-plugins\64bit\` にも自動でコピーします。
削除は `uninstall.bat` です。

## 動作要件

- OBS Studio 28 以降 / Windows x64（検証は 32.2.2）
- OBS の音声設定は **44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz** など（48 kHz 以外は
  内部で 48 kHz に変換して処理します。変換段を作れない異常値のときだけパススルー）

## ビルド

```powershell
cd clear_re\obs_plugin
.\build.ps1                 # cargo build --release（配布する DLL）
.\build.ps1 -Tests          # cargo test --release --lib（62 テスト）
.\tools\make_package.ps1    # 配布パッケージ (dist\) と ZIP を作る
.\tools\run_obs_verify.ps1  # OBS を実際に回して遅延と DSP を数値で確認する
```

必要なもの:

- Rust（MSVC ツールチェーン）+ Visual Studio の C++ ビルドツール
  （`build.ps1` が `vcvars64.bat` を探します）
- OBS Studio が `C:\Program Files\obs-studio` にあること（`obs.lib` の参照のため）

ONNX Runtime とモデルは `vendor/` と `clear_re/plugin/models/` に入っているので、
クローン直後でもビルドと配置ができます。

## リポジトリ構成

| パス | 内容 |
| --- | --- |
| `clear_re/obs_plugin/` | **現行版**。Rust の OBS プラグイン本体 + ビルド / インストール / 検証ツール |
| `clear_re/obs_plugin/src/` | `filter.rs`（OBS フィルタ）/ `ui.rs`（パネル）/ `dsp/`（DSP と ONNX）/ `meter.rs` / `preset.rs` / `paths.rs` / `ffi.rs` |
| `clear_re/plugin/` | 旧 VST3 版（DPDFNet Voice）。**保守対象外**。DSP の移植元 + モデルの置き場 |
| `clear_re/ANALYSIS.md` | 旧 VST3 版の設計経緯・確定事項 |
| `docs/` | この README 用の画像 |
| `dist/`（未追跡） | `tools/make_package.ps1` が生成する配布パッケージ |

## ドキュメント

| ファイル | 内容 |
| --- | --- |
| `clear_re/obs_plugin/README.md` | 機能・プロパティ・パネル・ファイル構成の詳細 |
| `clear_re/obs_plugin/OBS_VERIFY.md` | 実機検証の手順と結果、トラブルシュート |
| `clear_re/ANALYSIS.md` | 旧 VST3 版の解析（DSP をどう移植したか） |

## 動作確認済みの環境

- OBS Studio 32.2.2 / Windows x64 / ONNX Runtime 1.31.0（`ORT_API_VERSION = 17`）
- 実機 vs オフライン参照が**ビット一致**（1,260,928 frames × 2 ch / 最大差 `0e0`）
- 遅延 `2880 samples (6.000 hop, 60.0 ms)` 固定（48 kHz）
- 48 kHz 以外も**レート変換して処理**（オフライン検証: 96 kHz で出力レベル差 0.05 %、
  遅延 5989 サンプル = 62.4 ms = 60 ms + 変換 2.4 ms。`offline_rate_conversion_96k`）
- レート変換段の単体検証: 22.05 / 44.1 / 88.2 / 96 / 192 kHz と 6 種類のブロック長
  （1 / 7 / 480 / 1023 / 1024 / 4096）で入出力のサンプル数が一致し、定常で直流が
  そのまま通る（`rack_keeps_sample_counts_and_level`）
- `cargo test --release --lib` → **62 passed / 0 failed**、ビルド警告 0 件

## ログ

不具合を後から追えるように、専用のログファイルを**常に**書きます（設定はありません）。
OBS 標準のログ（`%APPDATA%\obs-studio\logs\*.txt`）にも同じ内容が出ます。

```text
%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\obs_noise_suppressor.log
```

```text
2026-10-18 04:23:09.949 [INFO] OBS Noise Suppressor v0.2.1 を読み込みました
2026-10-18 04:23:09.949 [INFO] dll: C:\ProgramData\obs-studio\plugins\obs_noise_suppressor\bin\64bit
2026-10-18 04:23:10.066 [INFO] filter created (ambience 0.50, voice 0.00 dB, reverb 0.00)
2026-10-18 04:23:10.365 [INFO] status: 48 kHz / ch=2 / latency 2880 samples (60 ms) / dpdfnet8_48khz_hr_2out.onnx
2026-10-18 04:23:36.659 [INFO] filter destroyed
```

OBS の音声を 48 kHz 以外にしているときは、実レートと変換の遅延が出ます
（例: `status: 96 kHz / ch=2 / latency 2880 samples (60 ms) / …`。パネルの 1 行も
`2 ch · 96 kHz · 遅延 2880 samples (60 ms +SRC 2.4 ms) · …` になります）。

（上のログは v0.2.1 の実測そのままです。バージョンの行だけは、読み込んだ版によって
`OBS Noise Suppressor v0.3.0 を読み込みました` のように変わります）。

記録するのは `ERROR` / `WARNING` / `INFO` の 3 段（`LOG_DEBUG` は OBS 標準ログだけ）で、
起動時の構成（バージョン / ONNX Runtime の版とパス / 読み込んだ DLL / 使ったモデル /
サンプルレート / 遅延）も残ります。1 MB を超えたら開くときに `.1` へ退避して最新 2 世代
だけを残し、毎フレーム同じ警告が出るような場合も間引きます。書き込めない環境でも
音声処理は止めません。

## ライセンス

このリポジトリは **AI が生成したコードを含みます**（「AI による制作について」を参照）。**無保証**です。

- このリポジトリの自作コード: **GPL-2.0-or-later**（[`LICENSE`](LICENSE)）
- ONNX Runtime（`dpdfnet_onnxruntime.dll`）: MIT License（Microsoft）
- DPDFNet モデル（`*.onnx`）: 同梱モデルのライセンスに従います
- 旧 VST3 版の `clear_re/plugin/third_party/kissfft`: 同フォルダの `COPYING` を参照
