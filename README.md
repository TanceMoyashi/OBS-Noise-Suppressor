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
| 推論デバイス | OBS プロパティで **CPU（既定）/ GPU (DirectML)** を選択。**GPU は実測で CPU より遅い**ため追加パック扱い（下記） |
| UI | 自前の Win32/GDI パネル（IN / OUT VU 2 本 + 縦フェーダー 3 本 + ミュート / バイパス + GR / 3 バンドメーター） |
| プリセット | 1〜8 のスロット（`presets.json` に保存） |
| VU 基準値 | −20 / −18 / −14 dBFS（既定 −18 = EBU）。目盛りが対応 dBFS ごと平行移動 |
| 数値表示 | フェーダーの dB と %、IN / OUT の現在値 / ピーク dBFS、GR の抑圧量 dB |
| インストーラー | レジストリ不使用 / 管理者権限不要 / アンインストーラー同梱 |
| VST3 版 | 同じ DSP を使う DAW 用プラグイン `DPDFNet Voice`（`clear_re/vst3/`）。OBS 版と DSP / パラメータ / 遅延が共通 |
| 音声スレッド | CPU はアロケーションなし（テンソル・バッファは事前確保し `IoBinding` で再利用）。GPU (DirectML) は毎 hop 束ねを作り直す（下記） |
| 堅牢性 | FFI 境界ごとに `catch_unwind`。落ちずにパススルーへ退避 |
| ログ | エラー / 警告 / 起動情報を専用ファイルに常時記録（OBS 標準ログにも同じ内容が出ます） |

## パネル

OBS のプロパティにある **「パネルを開く（フェーダー / メーター / メモリー）」** で開く、
自前描画のウィンドウです（画像素材やテーマファイルは使っていません）。

| セクション | 内容 |
| --- | --- |
| ヘッダ | タイトル + **メモリー 1〜8**（スロット押下 = 呼び出し / `SAVE` → スロット = 保存 / `Ctrl` + クリック = 消去） |
| IN / OUT | 入力（加工前）と出力（加工後）の**アナログ VU メーター**（実機準拠の弾道、300 ms で 99 %、行き過ぎなし）+ 現在値 / ピーク dBFS。ステレオは L / R の平均を 1 本の針で表示 |
| CONTROL | **縦フェーダー 3 本**（AMBIENCE / VOICE / REVERB）。ダブルクリックで既定値に戻ります。各フェーダーの下に **MUTE / BYP キー**（つまみの位置は保ったまま効きだけ切る 3 状態スイッチ） |
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

## VST3 版（DAW 用）

**同じ DSP を DAW でも**使えるようにした VST3 プラグイン（`DPDFNet Voice`）を
別パッケージで配ります。旧 VST3 版（JUCE、`clear_re/plugin/`）の復活ではなく、
現行の Rust DSP（`clear_re/dpdfnet/`）を共有した新しいシェルです。

1. `DPDFNet-Voice-x.y.z-vst3.zip` を展開して `install.bat`（中身は `install.ps1`）。
2. 既定で `C:\Program Files\Common Files\VST3\` に入ります（管理者が要ります）。
   管理者なしでよければ `install.bat -User` で `%APPDATA%\VST3\` へ。
3. DAW を起動し直して、モデルと ONNX Runtime ごと同梱された
   `DPDFNet Voice`（モノラル / ステレオ）をトラックに挿します。

| 項目 | 内容 |
| --- | --- |
| パラメータ | Ambience / Voice (±12 dB) / Voice Reverb + 各ミュート / バイパス + 全体の Bypass。範囲も既定値も OBS 版と同じ |
| モデル / 推論デバイス | パネルの MODEL / DEV キー、またはホストの汎用パラメータ表示から変更（自動化には載りません）。**変更すると一瞬音が途切れます**（その場で組み立て直すため） |
| 遅延 | 48 kHz で **2880 サンプル (60 ms)**。ホストに報告するので DAW 側で自動補正されます |
| GUI | OBS 版と同じ自前パネル（VU / フェーダー / MUTE / BYP / メモリー / MODEL / DEV）。ホストの子ウィンドウとして出ます。**固定サイズ** |
| ログ | DAW のログに出ます。`set NIH_LOG=%TEMP%\dpdfnet-vst3.log` でファイルにも |

詳しくは [`clear_re/vst3/README.md`](clear_re/vst3/README.md) をどうぞ。

## 動作要件

- OBS Studio 28 以降 / Windows x64（検証は 32.2.2）
- OBS の音声設定は **44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz** など（48 kHz 以外は
  内部で 48 kHz に変換して処理します。変換段を作れない異常値のときだけパススルー）
- CPU は**高品質モデル (`dpdfnet8`) でコア 1 本をほぼ使い切る**ため、ノート PC や
  古い CPU では**軽量モデル (`dpdfnet2`)** を選んでください（負荷は約 1/3。
  実測値と自動切り替えの説明は [`clear_re/obs_plugin/README.md`](clear_re/obs_plugin/README.md#cpu-負荷の目安音が途切れる--遅れて聞こえるとき)）
- GPU (DirectML) は**追加パック**（`OBS-Noise-Suppressor-<版>-gpu-pack.zip`）を
  入れたときだけ選べます。**実測では GPU は CPU より遅い**ので（このマシン /
  ステレオで `dp8` 6.68 → 12.73 ms、`dp2` 2.51 → 4.66 ms）、CPU に余裕があるなら
  CPU のままをおすすめします。詳細は
  [`clear_re/obs_plugin/OBS_VERIFY.md`](clear_re/obs_plugin/OBS_VERIFY.md#v030-追記-3推論デバイス-gpu-directml--2026-10-19)

## ビルド

```powershell
cd clear_re\obs_plugin
.\build.ps1                 # cargo build --release（配布する DLL）
.\build.ps1 -Tests          # cargo test --release --lib（OBS 版 41 + DSP コア 45 テスト）
.\tools\make_package.ps1    # 配布パッケージ (dist\) と ZIP を作る
.\tools\make_package.ps1 -GpuPack  # GPU (DirectML) の追加パックを作る
.\tools\run_obs_verify.ps1  # OBS を実際に回して遅延と DSP を数値で確認する

cd ..\vst3
.\build.ps1                 # VST3 版（cargo build --release）
.\build.ps1 -Tests          # VST3 版のテスト（モデルを指定するとオフライン検証も走る）
.\tools\make_package.ps1    # VST3 版の配布パッケージ (dist\) と ZIP を作る
```

必要なもの:

- Rust（MSVC ツールチェーン）+ Visual Studio の C++ ビルドツール
  （`build.ps1` が `vcvars64.bat` を探します）
- OBS Studio が `C:\Program Files\obs-studio` にあること（`obs.lib` の参照のため）
- VST3 版だけなら OBS は不要（代わりに初回ビルドで nih-plug と VST3 SDK を
  取ってくるためネットワークが要ります）

ONNX Runtime とモデルは `vendor/` と `clear_re/plugin/models/` に入っているので、
クローン直後でもビルドと配置ができます。

## リポジトリ構成

| パス | 内容 |
| --- | --- |
| `clear_re/obs_plugin/` | **現行版**。Rust の OBS プラグイン本体 + ビルド / インストール / 検証ツール |
| `clear_re/obs_plugin/src/` | `filter.rs`（OBS フィルタ）/ `ui.rs`（パネル）/ `meter.rs` / `preset.rs` / `paths.rs` / `ffi.rs`。DSP は下の `dpdfnet` を呼ぶだけ |
| `clear_re/dpdfnet/` | **共通 DSP コア**（ホスト非依存）。STFT / モデル / 遅延 / レート変換 / 負荷判定。OBS 版と VST3 版が共有する |
| `clear_re/vst3/` | **VST3 版**（DAW 用）。nih-plug のシェル + インストーラ / パッケージツール |
| `clear_re/plugin/` | 旧 VST3 版（DPDFNet Voice、JUCE）。**保守対象外**。DSP の移植元 + モデルの置き場 |
| `clear_re/ANALYSIS.md` | 旧 VST3 版の設計経緯・確定事項 |
| `docs/` | この README 用の画像 |
| `dist/`（未追跡） | `tools/make_package.ps1` が生成する配布パッケージ |

## ドキュメント

| ファイル | 内容 |
| --- | --- |
| `clear_re/obs_plugin/README.md` | 機能・プロパティ・パネル・ファイル構成の詳細 |
| `clear_re/obs_plugin/OBS_VERIFY.md` | 実機検証の手順と結果、トラブルシュート |
| `clear_re/vst3/README.md` | VST3 版のビルド・インストール・検証 |
| `clear_re/ANALYSIS.md` | 旧 VST3 版の解析（DSP をどう移植したか） |

## 重い / 音が途切れる・遅れて聞こえるとき

**症状**: 音が数秒遅れて聞こえる、ブツブツ切れる、声がロボットのようになる。

原因はほぼ **CPU が 1 ホップ = 480 サンプル（48 kHz で 10 ms）に追いついていない**
ことです。プラグイン自身が遅延を溜め込むことはありません（内容遅延は常に 2880
サンプル = 60 ms で固定）。追いつかないと OBS の音声スレッドが詰まり、**音が
遅れて聞こえる**ように見えます。

| 手順 | 内容 |
| --- | --- |
| 1 | フィルタのプロパティで **「モデル」を `軽量 (dp2)`** にする（負荷が約 1/3） |
| 2 | Windows の電源プランを **「高パフォーマンス」** にする（ノート PC をバッテリーで使っていると特に） |
| 3 | 録画 / 配信のエンコーダを **GPU（NVENC / QSV / AMF）** にする（x264 = CPU エンコードは 1 ホップの予算を奪う） |
| 4 | 他に重いフィルタ / プラグインがあれば外す。OBS の「統計」で `skipped frames` が増えていないか見る |
| 5 | 最新版に更新する（**起動時キャリブレーション**が入った版以降。「自動」でも最初から軽量モデルで始まる） |

効果はログで確認できます（`起動時の実測:` と `音声処理の負荷:` の行）。
「平均が予算の 95 % 超 / 予算超過のホップが 25 % 以上」が出ているときは、
そのまま上の手順を試してください。詳しくは
[`clear_re/obs_plugin/README.md` の「CPU 負荷の目安」](clear_re/obs_plugin/README.md#cpu-負荷の目安音が途切れる--遅れて聞こえるとき)。

### 相手の PC を切り分けるとき（そのまま送れる文面）

> 音が数秒遅れて途切れるのは、CPU が音声処理（10 ms ごとの AI 推論）に
> 追いついていないのが原因です。プラグインの遅延自体は 60 ms で固定なので、
> 遅れが積み上がることはありません。次の順で試してください。
>
> 1. フィルタのプロパティ →「モデル」を **軽量 (dp2)** に変更（いちばん効きます）
> 2. Windows の設定 → システム → 電源 → 電源モードを **最適なパフォーマンス** に
> 3. OBS の設定 → 出力 → エンコーダを **ハードウェア（NVENC / QSV / AMF）** に
>    （ソフトウェア x264 は CPU を 1 コアぶん持っていきます）
> 4. OBS の設定 → 詳細 → 音声バッファを確認（既定のままで OK）
> 5. 最新版に差し替える（起動時に重さを測って、最初から軽いモデルで始まる版）
>
> それでも直らないときは、OBS の標準ログを送ってください。
> OBS のメニュー「ヘルプ」→「ログファイル」→「現在のログファイルを表示」で開けます
> （場所は `%APPDATA%\obs-studio\logs\`）。見るのは次の 3 点です。
>
> - `CPU Name:` の行（CPU の型番）
> - `Audio buffering started (XXX ms)` と `ended` の行（音声バッファの詰まり）
> - `skipped` / `lagged frames` の行（映像 / 音声の取りこぼし）

## 動作確認済みの環境

- OBS Studio 32.2.2 / Windows x64 / ONNX Runtime 1.31.0（`ORT_API_VERSION = 17`）
- 実機（OBS）の 48 kHz / 96 kHz の両方で、**実機 vs オフライン参照がビット一致**
  （48 kHz: 1,260,928 frames × 2 ch、96 kHz: 2,505,440 frames × 2 ch、最大差 `0e0`）
- 遅延 `2880 samples (6.000 hop, 60.0 ms)`（48 kHz）。96 kHz は実測
  `5988 samples (62.4 ms)` = 60 ms + 変換 2.38 ms
- 48 kHz 以外も**レート変換して処理**（96 kHz で出力レベル差 0.05 %、
  `offline_rate_conversion_96k`）
- レート変換段の単体検証: 22.05 / 44.1 / 88.2 / 96 / 192 kHz と 6 種類のブロック長
  （1 / 7 / 480 / 1023 / 1024 / 4096）で入出力のサンプル数が一致し、定常で直流が
  そのまま通る（`rack_keeps_sample_counts_and_level`）
- フェーダーごとのミュート / バイパスは**既定が有効**なので音声経路は変わらない
  （実機のダンプ照合は新しい DLL でも `max diff = 0e0`）
- リアルタイム余裕の診断: 1 ホップ（10 ms）ごとの処理時間をアトミックに足し込み、
  監視スレッドが 1 秒ごとに判定。実機（48 kHz / ステレオ / 高品質）で
  `1 ホップ平均 7.2 – 8.6 ms / 予算 10.00 ms（72 – 86 %）`。間に合わない状態が
  続くと 1 度だけ軽量モデルへ自動で切り替えます（判定は `src/load.rs` の
  純関数 + 10 テスト）
- `cargo test --release --lib` → **82 passed / 0 failed**、ビルド警告 0 件

## ログ

不具合を後から追えるように、専用のログファイルを**常に**書きます（設定はありません）。
OBS 標準のログ（`%APPDATA%\obs-studio\logs\*.txt`）にも同じ内容が出ます。

```text
%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\obs_noise_suppressor.log
```

```text
2026-10-18 19:56:55.078 [INFO] OBS Noise Suppressor v0.3.0 を読み込みました
2026-10-18 19:56:55.078 [INFO] dll: C:\ProgramData\obs-studio\plugins\obs_noise_suppressor\bin\64bit
2026-10-18 19:56:55.183 [INFO] filter created (ambience 0.50, voice 0.00 dB, reverb 0.00)
2026-10-18 19:56:55.514 [INFO] status: 96 kHz / ch=2 / latency 2880 samples (60 ms) / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2
2026-10-18 19:56:56.520 [INFO] 音声処理の負荷: 1 ホップ平均 7.25 ms / 予算 10.00 ms (73 %)、最大 8.80 ms
2026-10-18 19:57:21.646 [INFO] filter destroyed
```

`status` の先頭は OBS の実レートです（48 kHz なら `48 kHz / … / latency 2880 samples (60 ms)`。
48 kHz 以外では上の例のように実レートが出て、パネルの 1 行にはレート変換ぶんの遅延も
付きます: `2 ch · 96 kHz · 遅延 2880 samples (60 ms +SRC 2.4 ms) · …`）。

記録するのは `ERROR` / `WARNING` / `INFO` の 3 段（`LOG_DEBUG` は OBS 標準ログだけ）で、
起動時の構成（バージョン / ONNX Runtime の版とパス / 読み込んだ DLL / 使ったモデル /
サンプルレート / 遅延）も残ります。1 MB を超えたら開くときに `.1` へ退避して最新 2 世代
だけを残し、毎フレーム同じ警告が出るような場合も間引きます。書き込めない環境でも
音声処理は止めません。

## 不具合報告・要望

**Issue は誰でも書けます**（GitHub アカウントがあれば、どなたでも）。テンプレートを
使わずに自由に書くこともできます。

- **不具合**: [不具合の報告を書く](https://github.com/TanceMoyashi/OBS-Noise-Suppressor/issues/new?template=bug_report.yml)
  （OBS のバージョン・サンプルレート・ログを書く欄があります）
- **機能の追加・改善の希望**: [要望を書く](https://github.com/TanceMoyashi/OBS-Noise-Suppressor/issues/new?template=feature_request.yml)
- 不具合かどうか分からない相談や使い方の質問: [Discussions](https://github.com/TanceMoyashi/OBS-Noise-Suppressor/discussions)

**このリポジトリのコードを直接変更できるのは、作者が承認した人だけです**（`main` は
保護されていて、直接の push や履歴の書き換えはできません）。それ以外の方は
リポジトリを fork して Pull Request を送ってください。対応は保証できません
（AI 生成の無保証ソフトです。「AI による制作について」を参照）。

## ライセンス

このリポジトリは **AI が生成したコードを含みます**（「AI による制作について」を参照）。**無保証**です。

- このリポジトリの自作コード: **GPL-2.0-or-later** — Copyright (C) 2026 TanceMoyashi。
  全文は [`LICENSE`](LICENSE)（GPL-2.0 の正規のテキストをそのまま置いています）
- ONNX Runtime（`dpdfnet_onnxruntime.dll`）: MIT License（Microsoft）
- DPDFNet モデル（`*.onnx`）: 同梱モデルのライセンスに従います
- 旧 VST3 版の `clear_re/plugin/third_party/kissfft`: 同フォルダの `COPYING` を参照
