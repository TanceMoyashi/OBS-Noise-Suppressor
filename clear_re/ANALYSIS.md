# VST3 プラグイン解析レポート

解析対象: `D:\Users\Syntac\Documents\OBS\VST3\Clear.vst3` と、同カテゴリの参考プラグイン群
作成日: 2026-10-17 / 目的: **類似プラグイン（de-noise / de-reverb）を自作するための参考資料**

---

## 1. 対象プラグインの素性

| 項目 | 値 |
| --- | --- |
| 製品 | **Supertone Clear 1.1.1** (FileDescription/ProductName = "Clear") |
| パス | `D:\Users\Syntac\Documents\OBS\VST3\Clear.vst3`（単体ファイル 25,308,728 bytes） |
| ファイル日付 | 2025-01-26 |
| インストーラ | `D:\Users\Syntac\Downloads\CLEAR_Win_x86_64_1.1.1.exe`（74.5 MB） |
| ライセンスファイル | `D:\Users\Syntac\Downloads\TAC_DESKTOP.clearlicense` |
| 機能（公開情報） | ボイス用 de-noise + de-reverb。3ノブ（**Ambience / Voice / Voice Reverb**）、Mono/Stereo、Eco モード、3ch 分離メーター表示、解析待ちなしのリアルタイム処理 |

> 補足: 形態素（セクション名・署名）から見て、このファイルは**保護ツールまたは事後加工で改変されている**可能性が高い（後述）。公式インストーラ由来かどうかに関わらず、バイナリ自体は「難読化済み」として扱うのが妥当。

## 2. Clear.vst3 の静的解析結果

PE ヘッダ（`clear_re\pe_info.py` の出力）:

```
PE32+ x64 / MSVC linker 14.29 / TimeDateStamp 2024-09-20 / imageBase 0x180000000
Exports: GetPluginFactory RVA 0xa03180, InitDll 0xa03310, ExitDll 0xa03300  (export name = Clear.dll)
Sections: .text .rdata .data .pdata _RDATA は Raw=0
          .Wy#(0x809573) .5<,(0x21a8) .]MP(0x181edb8) に実データ  ← 非標準名
SECURITY dir: RVA 0x1821c00 size 0x1238 だがファイル末尾 0x1821f38 を超過 → 署名破損
Resources: version info のみ（サイズ 0x4b6, CompanyName 空）
Imports: KERNEL32 USER32 GDI32 COMDLG32 ADVAPI32 SHELL32 ole32 OLEAUT32
         WININET WS2_32 SHLWAPI WINMM dbghelp IMM32 dxgi OPENGL32 ntdll bcrypt
```

可読文字列（ASCII 最小6文字 = 34,031 件 / UTF-16 = 9 件）:

- 意味のあるものは極少数: `Clear.dll`, `Supertone Inc.`(2), `CreateDXGIFactory`, `OPENGL32.dll`, DigiCert 証明書チェーン URL 群, `VS_VERSION_INFO` ほか
- **パラメータ名・VST3 クラス名・モデルファイル名は一切平文で存在しない**（UTF-16 に至っては 9 件すべてバージョン情報）
- 残りの大多数は 6〜10 文字のランダム列（`{%DSP*`, `~ZHDSP`, `5<,` など）で、暗号化/難読化されたデータ

### 結論（Clear の解析から得られるもの）
1. DSP やニューラルモデルの中身を静的解析で取り出すのは**非現実的**（暗号化＋難読化）。
2. imports から読み取れるのは「ネイティブ GUI（GDI/OpenGL/DXGI）」「ネットワーク（WININET/WS2_32 = 認証・テレメトリ等）」「暗号（bcrypt）」程度。
3. 参考にできるのは **公開されている機能・操作系（3ノブ + 分離表示 + Eco）** と、**他プラグインの実装スタック**。

## 3. 同カテゴリプラグインの実装スタック比較（実質的な参考資料）

| プラグイン | フレームワーク | 推論エンジン | モデル実体 | 判明した事実 |
| --- | --- | --- | --- | --- |
| **Clear 1.1.1** (Supertone) | 不明（難読化） | 不明 | 不明（3ch 分離） | 商用・保護済み。挙動の参考のみ |
| **Alt Denoiser 1.0.3** (Altinus) | **JUCE 8.0.12** | **Rust + tract-onnx 0.21.4**（純Rust推論） | `alt_denoiser_model.tar.gz` 内に `enc.onnx` / `erb_dec.onnx` / `df_dec.onnx` + `config.ini` + `version.txt` → **DeepFilterNet 系** | VST 3.8.0 SDK、CID `ABCDEF019182FAEB...`、`AltDenoiserProcessor` / `AltDenoiserEditor`、`.cargo\registry\...` と `C:\Users\runneradmin\...` → **GitHub Actions でビルド** |
| **rnnoise.vst3** | JUCE 7.0.1 | 自前 C（`DenoiseState`） | RNNoise | 軽量・BSD-3。JUCE ラッパー構成 |
| **voice-denoiser.vst3** | JUCE 8.0.12 | 不明 | パラメータ "Denoise" 1つ | 最小構成の参考例（`D:\Users\Syntac\Downloads\voice-denoiser-windows.vst3.zip`） |

→ **「JUCE + ONNX系推論 + DeepFilterNet 系モデル」がこのカテゴリの現実的な王道**で、Alt Denoiser はその完成形のサンプルになっています。

## 4. 類似プラグインの設計案

### 4.1 共通アーキテクチャ（どのルートでも必要）
- VST3: `IAudioProcessor` + `IEditController`（分割 or single component）、`setActive` / `process`、`getLatencySamples` でレイテンシ通知
- 48 kHz 学習モデル前提 → 44.1/96 kHz ホストでは**入力側リサンプリング**
- `process` 内は**アロケーション禁止**（FIFO + 事前確保バッファ、可変ブロック対応）
- 状態保存/復元（ノブ値、Mono/Stereo、Eco）、パラメータスムージング、バイパス
- UI: 3ノブ + 3ch（Voice / Ambience / Voice Reverb）レベルメーター。WebView2 かネイティブ（JUCE Component / VSTGUI）

### 4.2 3つの実装ルート

| ルート | 構成 | 長所 | 短所 / 追加要件 |
| --- | --- | --- | --- |
| **A: ONNX Runtime** | JUCE 8 + ONNX Runtime (C++ API, CPU / DirectML / CUDA EP) + DeepFilterNet3 の 3グラフ | 最短で高品質、Rust 不要、GPU も使える | 40 ms 前後のレイテンシ、ORT DLL 同梱でサイズ増 |
| **B: 軽量** | JUCE 8 + RNNoise（BSD-3） | 実装が単純、約10 ms、CPU 極小 | 品質は控えめ、de-reverb は別途必要 |
| **C: Alt Denoiser 同等** | JUCE 8 + Rust cdylib（tract-onnx）を FFI | ネイティブ DLL 不要・純Rustで軽量 | このPCに **cargo 未導入**（要 Rust 導入） |

### 4.3 de-reverb の作り方（Clear の 3ノブ目に相当）
1. DeepFilterNet は de-noise 専用なので、**残響抑圧は別段**（late reverb suppression / WPE + スペクトル減算）として実装する
2. 学習・ファインチューニングして denoise+dereverb のマルチタスクモデルにする（品質は出るが工数大）
3. OSS の dereverb レシピ（SpeechBrain / ESPnet 系）や公開モデルを ONNX 化して 2段目に置く
4. ノブのマッピング例: **Ambience** = ノイズ抑圧量 / **Voice** = ドライ成分の強調（EQ・ゲイン）/ **Voice Reverb** = 残響抑圧量。Eco = 軽量設定・短い窓

### 4.4 テスト
- `pluginval` による VST3 検証、主要 DAW（REAPER 等）での読み込み・オフライン書き出し確認
- 音質評価: PESQ / STOI を Python で回す、原音との null test、レイテンシ計測
- 参考: `voice-denoiser`・`Alt Denoiser` の出力と聴感比較

## 5. このPCのツールチェーン状況

| ツール | 状態 |
| --- | --- |
| CMake | ✅ `C:\Program Files\CMake\bin\cmake.exe` |
| Git | ✅ `C:\Program Files\Git\cmd\git.exe` |
| Visual Studio | ✅ VS 18 Community（C++ ツールあり、vswhere 確認済み） |
| Python | ✅ 3.12（numpy / torch / onnx / onnxruntime は**未導入**） |
| Rust / cargo | ❌ 未導入（ルートC を選ぶ場合のみ必要） |
| ONNX Runtime DLL | Adobe 製品同梱のものあり（流用せず公式配布物を推奨） |

## 6. 法務メモ

- Clear は商用製品。**機能・操作系の参考（公開情報ベース）は問題ない**が、UIアセット・モデル・コードの複製や保護の回避は行わない。
- 推奨モデルのライセンス: **RNNoise = BSD-3**、**DeepFilterNet = MIT / Apache-2.0 のデュアル** → 商用同梱可（実装前に最新のライセンス表記を確認）。
- 入手元が不明なバイナリは、解析より先にマルウェア検査と隔離を推奨。

## 7. 次のアクション候補

1. **ルートA のスケルトン作成**: JUCE 8 + CMake + ONNX Runtime で「3ノブ + 3chメータ」の VST3 をこのリポジトリに新規作成
2. **Python プロトタイプ**: DeepFilterNet3 の ONNX を実行し、手持ち音声で品質とレイテンシを測ってから C++ へ移植
3. **ルートB で最小の動くもの**: RNNoise を JUCE に組み込み、VST3 として DAW で鳴らすところまで
4. **Alt Denoiser の位置づけ整理**: 自作プラグインなら、Clear の操作系と突き合わせて改善点を整理・実装

---

# 追補（調査を進めた結果）

## 8. OBS / 96 kHz に関する制約

- OBS のネイティブオーディオフィルタは **VST2 のみ**。VST3 を読むには **atkAudio**（無料、OBS 31.1.0+、JUCE 9 ベースのホスト、Win/macOS/Linux、追加レイテンシ約 40 ms）が必要。
- `D:\Users\Syntac\Documents\OBS\VST3\` に VST3 が並んでいる → すでに atkAudio を使っている可能性が高い。
- OBS のサンプルレートは **44.1 kHz / 48 kHz のどちらか** → OBS 経由では 96 kHz はプラグインに届かない。
- 96 kHz を扱いたいなら DAW 使用が前提。いずれにせよ **内部処理は 48 kHz にリサンプリング**するのが正解（DeepFilterNet/DPDFNet の native rate が 48 kHz、Krisp は内部 32 kHz / lite 16 kHz）。96 kHz で回すメリットはない。
- VST3 のホストルール（atkAudio/JUCE 準拠）:
  - レイテンシは `getLatencySamples()` で必ず通知（モデルの lookahead 分を正直に申告）
  - モデルロードは `setup()` / メインスレッド。`setActive()` 内では禁止
  - オーディオスレッドで **アロケーション / ファイル I/O / ネットワーク禁止**
  - `globalInit` はプロセスグローバル → 複数インスタンス対策に static な参照カウントガード

## 9. Krisp SDK（比較用・ルート K として検討し採用見送り）

- 呼び出し順: `globalInit` → `ModelInfo`(.kef) → `NcSessionConfig` → `Nc<SamplingFormat>::create` → `process(..., noiseSuppressionLevel 0-100, stats)`
- 10 ms フレーム固定、8–96 kHz 入力可、モデル内部は 32 kHz（lite 16 kHz）、モノラルのみ、メモリ約 100 MB、1.5–2 ms/frame、レイテンシ約 25 ms（lite 15 ms）
- モデル: `krisp-nc-o-pro-v1` 5.8 MB / `krisp-nc-o-med-v7` 5.7 MB / `krisp-nc-o-lite-v1` 2.8 MB
- **ランタイムライセンスキーが必要**（`KRISP_LICENSE_KEY` または element property）、オンライン認証の可能性あり（libcurl/OpenSSL 同梱）→ オフライン利用は失敗しうる
- **de-reverb 機能はない**（NC と BVC のみ）→ Clear の3つ目のノブを埋められない
- precedent: `krispai/gst-krisp-audio` は `/MT` CRT を要求（`-Db_vscrt=mt`）→ JUCE デフォルトの `/MD` と衝突
- VST3 ラッパー自体は約 200 行で書ける。実質的な障壁はライセンス → **個人プロトタイプではルート K を採用しない**

## 10. sherpa-onnx C API（推論層の選択肢）— 確認した内容

`c-api.h`（ sherpa-onnx ）を直接読んだ結果:

- `SherpaOnnxOfflineSpeechDenoiserModelConfig` は **offline / online で共有**され、`gtcrn` と **`dpdfnet`** の両モデルファミリーを持つ
- `SherpaOnnxOnlineSpeechDenoiserConfig` は同じ model config を再利用する
  → **DPDFNet をストリーミング（online）で使える**
- online API: `SherpaOnnxCreateOnlineSpeechDenoiser` / `Destroy` / `GetSampleRate` / `GetFrameShiftInSamples` / `Run` / `Flush` / `Reset`
- ドキュメント上、DPDFNet は 8/16/48 kHz で offline・online 両対応、入力はモデルの native rate にリサンプリングされる

VST3 に組み込む場合の注意点:

1. `Run` は **スレッドセーフでない**、かつ `SherpaOnnxDenoisedAudio` は **呼び出しごとにヒープ確保** → オーディオスレッドで直接呼ぶのはルール違反。
   → worker thread + ロックフリー ring buffer で処理するか、STFT/ERB フロントエンドを自前実装して ONNX Runtime C++ を直接使う。
2. `attenuation_limit_db` は DPDFNet config にあるが、doc の表記は **"Offline attenuation limit"**。online では効かない → **ノブ強度は blending で実装する**。
3. `Flush` は reset を兼ねる（発話終了扱い）→ 常時運転のプラグインでは使わない。

→ sherpa-onnx C API は「STFT / framing / state / resampling を肩代わり」してくれるので、**Python・CLI での品質検証**には最適。VST3 本体は ONNX Runtime C++ + 自前フロントエンド（または worker thread）で書くのが安全。

## 11. dereverb モデル調査（最終結論）

### 11.1 DPDFNet（`ceva-ip/DPDFNet`, **Apache-2.0**）
- 論文（arXiv 2512.16420）§3.1 に明示: 学習時に **"reverberation with early reflection targets"** augmentation を適用し、**late reverberation を抑圧して direct path + early reflections を残す**。
  → **DPDFNet は構造的に dereverb 能力を持つ**。これは Clear の "Voice Reverb" ノブの semantics と一致する。
- ただし評価ベンチは VoiceBank+DEMAND と DNS4（additive noise 中心、RIR/残響成分なし）→ dereverb 特化の評価は報告されていない。効果は「学習 augmentation 由来」であり、dereverb 単独の強さは保証されていない。
- 公式 ONNX export: `dpdfnet_baseline.onnx`, `dpdfnet2/4/8.onnx`（16 kHz）, `dpdfnet2_8khz`/`dpdfnet8_8khz`, `dpdfnet2_48khz_hr`/`dpdfnet8_48khz_hr`（48 kHz）。GitHub release `speech-enhancement-models` と HF `Ceva-IP/DPDFNet`。docs: `https://k2-fsa.github.io/sherpa/onnx/speech-enhancement/dpdfnet.html`
- 設計上: 20 ms window / 10 ms hop、DF order N=5、論文の look-ahead ℓ=2。
  **ただし実測の内容遅延は 4 フレーム（4 hop = 40 ms）**（§13.2）。論文の ℓ は計算グラフ上の
  look-ahead であり、マスク経路のフレームバッファ遅延を含めた「出力フレーム t が入力フレーム
  t−4 に対応する」という実測値とは別物。実装では後者を使う。

### 11.2 単一モデルで2ノブは独立にならない（重要な設計上の注意）
DPDFNet の target は「direct + early reflections」なので、出力は denoise と dereverb が**混ざった1本**になる。
- ノブ1（Ambience）= model output を noisy input と blending → 抑圧量
- ノブ3（Voice Reverb）= model output（dereverb 済み）と原音（残響込み）を blending → 残響量

この2つは同じモデル出力の blend なので**独立に動かない**。Clear のように3ノブを独立に動かすには、**NS 用モデルと dereverb 専用モデルの2段**が必要。

### 11.3 除外した候補

| 候補 | 理由 |
| --- | --- |
| `tomer-ros/deep-filter-net-dereverberation`（Rosenbaum et al., Sensors 2025, CC-BY） | DFN3 の dereverb 弱点を明示し、dereverb decoder（Dsd, M=10 taps, D=50 ms delay）を追加。ΔFWSegSNR 1.313 → 2.302、RTF 0.083。**リポジトリは README が "hello" のみ = code/weights なし** → 使用不可 |
| `sp-uhh/2sderev` | GitHub API で **license = null**（LICENSE ファイルなし = 権利留保）。かつ multi-channel（複数マイク）前提 → mono では使えない |
| `MixDirective/mix-directive-dereverb-onnx` | proprietary、offline、receptive field 約 10.9 s、hop 256、n_fft 1024 → real-time 不可 |
| `DeRTV-Net` | repo / ONNX export が見つからない |
| `STSubNet`（Alibaba） | joint denoise+dereverb、real-time、0.36 M params と小さいが**論文のみ、weights なし** |
| `voicefixer`（MIT, 415 MB, 44.1 kHz） | noise+reverb+clipping+band loss を扱うが offline / generative |

### 11.4 残っている唯一の「room reverb を除去すると明記された open real-time model」
`facebookresearch/denoiser` の causal Demucs（**CC-BY-NC-4.0**, 16 kHz, RTF 0.8 on 1 thread, Revecho / partial-dereverberation augmentation）。
ONNX export は HF `TigreGotico/audiosronnx-metadenoiser`（19–34 MB）にあるが **10 s 固定窓で trace 済み**（streaming ではない）→ stateful exportを作り直す必要がある。CC-BY-NC は個人プロトタイプなら可、商用配布なら不可。

### 11.5 推奨パイプライン（更新版）

- **Stage 1（NS ノブ）**: DPDFNet `dpdfnet2_48khz_hr.onnx`（Apache-2.0、streaming、約 10 MB）
- **Stage 2（dereverb）**, 優先順:
  1. **DPDFNet を RIR-augmented data で fine-tune**（同一アーキテクチャ / export tooling / C API がそのまま使える。torch が必要）← 最有力
  2. causal Demucs（metadenoiser）の **stateful / streaming ONNX export**（CC-BY-NC、16 kHz）
  3. 小さい streaming dereverb mask net を**自前学習**（torch が必要）
  4. neural でない **WPE / long-context spectral post-filter**（学習不要、fallback）

## 12. 次のアクション（当時の記録・すべて決着済み）

> 以下は設計初期のメモ。結論は §13〜§15 にある: 推論層 = ONNX Runtime C++ + 自前 STFT、
> Stage 2 = DPDFNet の RIR fine-tune（暫定はグラフ手術）、OBS 専用 = 48 kHz のみ。

1. Stage 2 の方式を確定（11.5 の 1〜4 のどれか）
2. 推論層を確定: ONNX Runtime C++ を直接使うか、sherpa-onnx C API を使うか。オーディオスレッドのアロケーション問題（worker thread + ring buffer / 自前 STFT・ERB）と、streaming で `attenuation_limit_db` が使えない問題をどう解決するか決める
3. OBS 専用か DAW も使うかを確認（OBS 専用なら 44.1/48 kHz + 内部 48 kHz リサンプリングで確定）
4. ONNX Runtime C++ を導入（prebuilt / vcpkg）→ JUCE 8 + CMake の VST3 スケルトン作成（3ノブ + 3chメータ、`getLatencySamples()`、48 kHz リサンプリング、モデルロードは `setup()`）
5. Python で DPDFNet の ONNX を回して品質・レイテンシを実測してから C++ に移植

---

# 確定設計（決定事項を反映）

ユーザー決定: **dereverb 段 = DPDFNet を RIR で fine-tune / 推論層 = ONNX Runtime C++ + 自前 STFT / 利用先 = OBS 専用**（→ 44.1/48 kHz、内部 48 kHz リサンプリング）

## 13. ONNX 入出力契約（公式スクリプトで確定）

`ceva-ip/DPDFNet/onnx_model/infer_dpdfnet_onnx.py`（公式のストリーミング推論リファレンス）を読んで確定した、実装に必要な全情報:

| 項目 | 値 |
| --- | --- |
| グラフ入力 | `spec` : float32 **[1, 1, F, 2]**（1 フレームのみ、実部/虚部を最後の次元に） |
| グラフ入力 | `state_in` : float32 **[state_size]**（フラット） |
| グラフ出力 | `spec_e` : float32 **[1, 1, F, 2]** |
| グラフ出力 | `state_out` : float32 **[state_size]** |
| モデル | `dpdfnet2_48khz_hr.onnx` / `dpdfnet8_48khz_hr.onnx`（48 kHz、F = 481） |
| metadata | `n_fft`=960 / `hop_length`=480 / `window_length`=960 / `window_type`="vorbis" / `normalized`=0 / `center`=1 / `pad_mode`="reflect" / `freq_bins`=481 / `erb_bins`=32 / `spec_bins`=96 / `state_size` / `erb_norm_state_size` / `spec_norm_state_size` / `erb_norm_init` / `spec_norm_init` |

**グラフが内部に持つもの**: ERB フィルタバンク、MagNorm48 / SpecNorm48 正規化、エンコーダ（DPRNN 含む）、ERB デコーダ、DF デコーダ、マスク適用、DF（deep filter）適用、および `*wnorm` / `*inv_wnorm`。**すべて internal state (`state_in`/`state_out`) で持ち回る**。

**グラフが内部に持たないもの**: STFT と iSTFT のみ。→ 自作するのはここだけ。

### 13.1 重要な確定事項（推測を排除）

1. **`wnorm` は外から掛けてはいけない。** 公式スクリプトは `librosa.stft(...)` の**生の複素スペクトルをそのまま** `spec` に渡し、`librosa.istft(...)` で戻す（`* wnorm` も `/ wnorm` も無い）。`model/utils.py` の `get_wnorm(win_len, hop) = 2*hop/win_len²` はグラフ内部（エクスポート用ラッパー）で完結している。つまり **C++ 側は生 STFT → ONNX → 生 iSTFT** でレベルが合う。
2. **窓は Vorbis 窓**: `w[i] = sin(π/2 · sin²(π(i+0.5)/(win/2)))`、win = 960。
3. **初期 state はゼロではない**。`state[0 : erb_norm_state_size] = erb_norm_init`、`state[erb_norm_state_size : erb_norm_state_size+spec_norm_state_size] = spec_norm_init`、**残りはゼロ**。`erb_norm_init` / `spec_norm_init` は metadata にカンマ区切りの10進文字列で入っている。
4. **`win_len` は ONNX の入力 shape から復元できる**: `(freq_bins - 1) * 2` = 960。
5. **減衰リミット（attenuation limit）の実装はオフライン専用**。オンラインでは使えないので、**ノブ強度は C++ 側でスペクトルをブレンドして実装する**。整列は `ATTN_LIMIT_NOISY_FRAME_OFFSET = 4` フレーム（= §13.2 のモデル遅延）。C++ 側は 4 フレーム前の整列入力スペクトルを ring buffer で参照して混ぜる。
   （以前ここに「ストリーミングでは厳密に 2 フレーム」と書いていたが、`tools\sim_stream.py` の実測で **4 フレームが正しい**ことが確定した。ONNX グラフ内部の `/model/mask/spec_buffer` も整合している。）
6. **HF に公式チェックポイントがある**（これは大きい）: `Ceva-IP/DPDFNet` の `checkpoints/dpdfnet2_48khz_hr.pth` / `dpdfnet8_48khz_hr.pth`（Apache-2.0）。→ **2出力エクスポート**（NS 用と dereverb 用を分ける）と **RIR による fine-tune** の両方が、重みを一から学習せずに可能。

### 13.2 レイテンシ（実測: **6 hop = 2880 サンプル = 60 ms @48 kHz**）

**モデル自身の内容遅延は 4 フレーム（4 hop = 1920 = 40 ms）**。根拠は 4 つ:

- 公式 `dpdfnet/audio.py` の `ATTN_LIMIT_NOISY_FRAME_OFFSET = 4`
- 公式オフライン経路（`postprocess_spec`）が iSTFT 出力の先頭 `2·win_len`（= 4 hop）を捨てて遅延を消している
- `tools/diag_model.py` のスペクトル照合、および `tools/check_onnx_model.py` が 4 フレーム補正で lag 0 になること
- **ONNX グラフ内部**: `/model/mask/Mul_output_0` は `/model/mask/spec_buffer` 経由の整列済み spec にマスクを掛けており、DF 段のフレームバッファは 5 エントリ（= DF order N=5）。マスクのみを取り出すと `spec_e` より **2 フレーム先行**するので、DF 段バッファの index 2 を取ると同位相になる（§13.3）

しかしプラグインが PDC で申告すべき**内容遅延は 4 hop では終わらない**:

| 内訳 | hop | 根拠 |
| --- | --- | --- |
| モデル自身の内容遅延 | 4 | 出力フレーム t = 入力フレーム t−4 の内容 |
| WOLA の構造 | 1 | step s で確定する hop は入力 hop (s−5) の内容（2 フレームの OLA の重なり） |
| 出力位置 | 1 | 完成した hop を「次に読むサンプル」から書くため、出力グリッド上で 1 hop 後ろ |
| **合計** | **6** | **2880 サンプル = 60 ms** |

実測は `tools/sim_stream.py`。C++ の `VorbisStft` + `runHop` + `processBlock` を忠実に再現し、
**出力位置の規約まで含めて**入力と相互相関を取る:

| 実装 | 内容遅延 |
| --- | --- |
| `dpdfnet.enhance`（オフライン、4 hop 補正込み） | **0**（基準。ツール自体の健全性チェック） |
| 公式 `StreamEnhancer`（可変長を返しホストにゼロ埋めさせる） | 5 hop = 2400 = 50 ms |
| **本プラグイン**（固定長ブロック） | **6 hop = 2880 = 60 ms** |

**6 hop は「固定長ブロックを必ず返す」VST3 で実現できる最小値**である。合成をスキップする
フレーム数（`c.step >= 2`）を 0〜5 と変えても合計は 6 hop のままであることを実測で確認した
（スキップを増やすと起動過渡は減るが、その分だけ先頭の prime が増えて相殺される）。
公式の 5 hop は「ブロック長より短い返り値を許し、ホスト側にゼロ埋めさせる」sounddevice 前提の
実装であり、VST3 の `processBlock`（必ず numSamples を埋める）では真似できない。

> 途中経過のメモ: 一時期「3 hop = 1440」と結論していた。これは **放出した hop を詰めて並べた
> 仮想ストリーム**の相関を見ていたためで、実際に書かれる出力位置（`processBlock` が
> 「その hop をどのサンプル位置に書くか」）を再現していなかった。規約を忠実にした結果が 6 hop。

実機確認: 本プラグインを通したインパルス（またはクリック）を録音し、atkAudio の遅延補正を
切って入出力の先頭位置を比べる。申告値は `src/PluginProcessor.h` の `kLatencyHops`（= 6）。

### 13.3 ノブの実現方法（3ノブ独立の設計）

| ノブ | 実装 | 単一出力モデル（公式）での挙動 |
| --- | --- | --- |
| **Ambience** | 整列した入力スペクトルを混ぜる量。`Y = (1−α)·spec_e + α·spec_noisy_aligned`、`α = 10^(−L/20)`、`L ∈ [6, 48] dB`（ノブ反転: Ambience↑ = 抑圧弱） | **動く**（公式の attenuation limit と等価） |
| **Voice** | 出力のプレゼンス/ゲイン（`-12..+12 dB`、軽い高域シェルフ） | 動く（暫定実装） |
| **Voice Reverb** | `Y = (1−β)·spec_ns + β·spec_e`（β = ノブ）。`spec_ns` = マスク適用のみ、`spec_e` = マスク + DF | **効かない**（1 出力モデルには `spec_ns` が無い）→ UI でグレーアウト |

→ **`Voice Reverb` を効かせるには 2出力グラフが必要**。方法は2つ:

**(A) グラフ手術（再学習不要・実施済み）** — `tools/add_second_output.py`。
`spec_ns = Mul(Squeeze(Slice(/model/df_op/spec_buffer/Concat_output_0, index=2)), inv_wnorm)`
として `spec_ns` 出力を足す。重みは一切変更せず、`spec_e` / `state_out` はビット一致のまま
（`tools/verify_two_output.py` で検証: `max|diff| = 0.000e+00`）。

ポイント:

- **`inv_wnorm`（= 960.0）を掛ける**必要がある。グラフ内部は `spec_e = df_out · inv_wnorm` という
  正規化をしており、中間テンソルは正規化ドメインにある。掛けないとレベルが −60 dB になる。
- **`Squeeze(axis=2)`** で `[1,1,1,F,2]` → `[1,1,F,2]`（C++ は後者の shape で受ける）。
- **同位相にするのが最も難しい点**。マスク経路の素の出力（`/model/mask/Mul_output_0`）は
  `spec_e` より **2 フレーム先行**する（マスクは 2 フレーム遅延の spec に掛かるが、DF 段は
  さらに先の内容を出す）。DF 段のフレームバッファ（5 エントリ、古い順）から **index 2** を
  取ると同位相になる。これを間違えると `Voice Reverb` ノブが時間方向のブレンドになり、
  音がにじむ。
- 効き幅: `|spec_ns − spec_e|` は相対 **−10 dB 前後**。DF 段の寄与がそのまま残響抑圧の
  効き幅になる（`spec_ns` −4.1 dB / `spec_e` −3.5 dB、いずれも入力比）。
- 生成物: `models/dpdfnet2_48khz_hr_2out.onnx` / `models/dpdfnet8_48khz_hr_2out.onnx`。
  `resolveModelFile()` は `*_2out.onnx` を優先し、無ければ 1 出力版にフォールバックして
  `Voice Reverb` をグレーアウトする。

**(B) RIR fine-tune（本命・未着手）**: HF の `dpdfnet2_48khz_hr.pth` を起点に、DNS4 の
248 real / 60,000 synthetic RIR で「direct + early reflections」を target にして fine-tune。
そのうえで 2出力エクスポートすれば、(A) と同じ C++ 経路のまま残響除去が強くなる。

## 14. 実装スケルトン（`clear_re\plugin\`）

```
clear_re\plugin\
  CMakeLists.txt                 JUCE 8.0.12 (FetchContent) + KissFFT (vendored) + ONNX Runtime
  README.md                      ビルド手順 / モデル取得 / atkAudio / キャリブレーション
  src\Parameters.h               パラメータ ID とレンジ
  src\ModelMeta.h/.cpp           ONNX metadata → STFT/state 情報
  src\VorbisStft.h/.cpp          Vorbis 窓 STFT/iSTFT（960点 = 2^6·3·5 → 混合基数）
  src\DpdfNet.h/.cpp             ONNX Runtime ラッパ（IO binding / 状態ダブルバッファ / 3出力対応）
  src\PluginProcessor.h/.cpp     JUCE プロセッサ（48 kHz 内部 / 3ノブ / メータ / レイテンシ）
  src\PluginEditor.h/.cpp        3ノブ + 3ch メータ
  third_party\kissfft\           vendored KissFFT (tag 131.2.0, BSD-3)
  tools\check_onnx_model.py      契約検証（numpy のみで STFT/iSTFT を独立実装）
  tools\sim_stream.py            C++ のストリーミング処理を再現して内容遅延を実測
  tools\diag_model.py            レベル / フレーム遅延の診断
  tools\inspect_graph.py         ONNX 中間テンソルの producer/consumer 探索
  tools\add_second_output.py     グラフ手術で 2出力化（spec_ns を追加）
  tools\verify_two_output.py     2出力モデルの検証（spec_e/state_out が不変であること）
  tools\bench_model.py           1 フレーム推論時間とリアルタイム余裕（RTF）の測定
```

**注意**: CMake 4.2.0 はソース/作業パスに日本語が含まれると無言でクラッシュする
（終了コード `0xC0000409`）。ASCII のみのジャンクション
（`C:\dev\dpdfnetvoice-plugin` → 実体）を作り、**そこから cmake を実行する**。
リポジトリの実体は日本語パスのままでよい。ビルドログもジャンクション側に置く。

### 14.1 FFT の選定（重要）

`n_fft = 960 = 2^6 × 3 × 5` であり、**`juce::dsp::FFT` は 2 の冪しか扱えないため使えない**。→ **KissFFT**（BSD-3、`kiss_fftr` が任意の偶数サイズ＝混合基数に対応、sherpa-onnx も同系統を vendor している）を採用。CMake から FetchContent で取得する。

`kiss_fftr` は正規化なし（`X[k] = Σ x[n]·e^{−j2πkn/N}`）で numpy/torch と同じ。`kiss_fftri` は `N·ifft` を返すので **1/N を掛ける**。iSTFT は torch/librosa と同じ「窓包絡線で割る」WOLA（`env[m] = w[m]² + w[m+hop]²`、m = 0..hop−1）で実装する。Vorbis 窓は厳密な Princen-Bradley 窓ではないため、COLA 定数で割るのではなく包絡線で割るのが正しい。

### 14.2 オーディオスレッドのアロケーションゼロ

- ORT は `Ort::IoBinding` に**事前確保した `Ort::Value`** を bind して `Run`。入出力バッファは `prepare()` で確保し、`state` はダブルバッファで入れ替える。
- ORT 内部のアリーナはロード時のウォームアップ（数フレーム）で成長させ、以後は再利用させる。
- `intra_op_num_threads = 1` / `inter_op_num_threads = 1` / `graph_optimization_level = ORT_ENABLE_ALL`（公式スクリプトと同じ）。

### 14.3 検証済みのポイント（ビルド後に確認した事実）

1. **遅延 = 6 hop = 2880 サンプル = 60 ms**（`tools\sim_stream.py` の実測。§13.2）。
   内容の整列は `tools\check_onnx_model.py` が 4 フレーム補正で lag 0 になることで確認する。
   OBS ではインパルスをループバックして最終確認する。
2. **ORT は 1.31.0 で API 差を吸収済み**:
   - `LookupCustomMetadataMapAllocated(key, OrtAllocator*)`（`...AllocatedAllocator` は無い）
   - `AllocatorWithDefaultOptions` に `GetAllocator()` は無く、`Base<T>` の暗黙変換で `OrtAllocator*` を取る
   - 既定アロケータは ORT 所有なので一時オブジェクトの破棄後も安全
3. **KissFFT は vendored**（FetchContent ではなく `third_party\kissfft`、tag `131.2.0`、BSD-3）。
   `FIXED_POINT` / `USE_SIMD` / `KISS_FFT_USE_ALLOCA` は `#ifdef` のみなので未定義のまま。
   `kiss_fftri` は `N·ifft` を返すので `1/N` を掛ける。DC/Nyquist の虚部は 0 にする。
4. **Vorbis 窓 STFT の実装上の注意**:
   - `VorbisStft` はユーザー定義デストラクタのせいで暗黙の move が消えるため、
     明示的な move ctor/assign が必要（`std::vector<ChannelDsp>::resize` で使う）。`DpdfNet` は
     `std::unique_ptr` で保持する（non-movable）。
   - `prepareToPlay` はモデルのロード後（`meta` が確定してから）フレーム長を決める。
     `setActive()` ではなく `prepareToPlay` でのみロードする。
5. **合成信号は契約検証にしか使えない**: モデルが「音声でない」と判断して −40 dB 以下に抑圧する。
   レベルと遅延の検証は `test_signals/speech_with_noise.wav` のような実音声で行う。
6. **2出力化のグラフ手術は `spec_e` / `state_out` をビット一致で保つ**
   （`tools\verify_two_output.py` の `max|diff| = 0.000e+00`）。
7. **モデル探索は `*_2out.onnx` を優先**し、無ければ 1 出力版にフォールバックして
   `Voice Reverb` をグレーアウトする（`dereverbAvailable`）。

---

# 実装状況（現時点）

## 15. 到達点と残タスク

### 完了（検証済み）

| 項目 | 状態 |
| --- | --- |
| ツールチェーン | CMake 4.2.0 / VS 18 / MSVC 19.51 / ORT 1.31.0 / JUCE 8.0.12 |
| ビルド | `cmake --build build --config Release` が警告 0・エラー 0 で成功 |
| 生成物 | `build\DPDFNetVoice_artefacts\Release\VST3\DPDFNet Voice.vst3`（5.4 MB + `dpdfnet_onnxruntime.dll` 16.5 MB） |
| C++ 実経路の初回実行 | `tools/ort_smoke_test.cpp`（GUI 不要の検証 EXE）。ORT 1.31 を同梱から読み、`DpdfNet::load` / `processFrame` / `VorbisStft` が動作。遅延 5 hop（モデル 4 + WOLA 1）を実測 = `kLatencyHops = 6` の内訳と一致 |
| **OBS 即死バグの修正** | 同梱 ORT が System32 の ORT 1.17 に置き換わっていた（§16）。`ORT_API_MANUAL_INIT` + 一意名 `dpdfnet_onnxruntime.dll` + 明示ロードで解決 |
| モデル同梱 | POST_BUILD で `models\*.onnx` を `…\VST3\models` へ（探索順と一致） |
| ONNX 契約 | `check_onnx_model.py`: レベル −3.5 dB、4 フレーム補正で lag 0 |
| ストリーミング DSP | `sim_stream.py`: C++ の再現と公式実装が相関 1.000 |
| レイテンシ | **6 hop = 2880 = 60 ms**（`kLatencyHops = 6`） |
| 2 出力化 | `spec_ns` を追加し `Voice Reverb` が機能。`spec_e`/`state_out` はビット不変 |
| Ambience 整列 | 4 フレーム ring（`kModelLookaheadHops`）で公式 attenuation limit と等価 |
| CPU（mono, 1 thread） | `dpdfnet2`: 0.83 ms/frame（RTF 0.08）/ `dpdfnet8`: 2.39 ms（RTF 0.24） |
| CPU（stereo = 2 inst.） | `dpdfnet2`: 1.66 ms（RTF 0.17）/ `dpdfnet8`: 4.79 ms（RTF 0.48） |
| 抑圧効果（低活動区間, 残響つき入力） | `dpdfnet2`: −28.3 → −49.9 dB / `dpdfnet8`: −36.6 → −60.6 dB |
| **de-reverb の効き幅**（Voice Reverb 0→1） | `dpdfnet2`: **−21.6 dB** / `dpdfnet8`: **−23.9 dB** |
| OBS 側 | atkAudio 導入済み（OBS 32.2.2）を確認。`Documents\OBS\VST3\` に配置済み |
| 実機確認の手順 | `plugin/OBS_VERIFY.md` に投入（テスト信号 / 期待値 / 遅延測定 / 切り分け） |

### 残タスク

1. **OBS で実機確認**（atkAudio 経由）。`Documents\OBS\VST3\` にバンドルと `models\` を置き、
   48 kHz で音を確認、インパルスで 2880 サンプルの遅延を実測。
   §16 のクラッシュ要因は修正済み。作業前に `build\Release\dpdfnet_ort_smoke.exe` を通すと、
   OBS を巻き込まずに ORT と DSP の健全性を確認できる。
2. **RIR fine-tune**（本命の dereverb）。torch + `checkpoints/dpdfnet2_48khz_hr.pth`、
   DNS4 の RIR で「direct + early reflections」を target に学習し、2 出力で再エクスポート。
   評価は PESQ/ESR と、残響のみの素材での late-reverb 抑圧量。
3. **44.1 kHz 対応**（任意）。リサンプラ（`juce::LagrangeInterpolator` の群遅延を
   `kLatencyHops` に加算する必要がある）を入れる。OBS 専用なら不要。
4. **CPU 最適化**（任意）。`dpdfnet2` で 1 インスタンス約 1 コア想定。ステレオは 2 インスタンス。
   必要なら `intra_op_num_threads` の調整や dpdfnet2 → さらに軽い派生の検討。

## 16. OBS 即死バグ（System32 の onnxruntime.dll 衝突）と修正

### 症状

atkAudio のフィルタで本プラグインを選んだ瞬間に OBS が落ちる。
- `%APPDATA%\obs-studio\crashes\` にクラッシュログが残らない
- Windows のイベントログに `Application Error` / WER レポートも出ない
- 次回起動時に `Crash or unclean shutdown detected`（= 強制終了扱い）

### 原因（実測で確定）

1. **VST3 のモジュールローダは素の `LoadLibraryW` を使う。**
   `juce_audio_processors_headless/format_types/VST3_SDK/public.sdk/source/vst/hosting/module_win32.cpp`
   の 165 / 183 行目が `LoadLibraryW (path)`（`LOAD_WITH_ALTERED_SEARCH_PATH` ではない）。
   この場合、依存 DLL の探索順は
   「exe のディレクトリ → System32 → Windows → カレント → PATH」で、
   **プラグイン自身のディレクトリは入らない**。

2. **Windows 11 (24H2 以降) は `C:\Windows\System32\onnxruntime.dll` を持つ**
   （Windows ML 同梱。このPCでは実測 **1.17.260613-0100.1.os-germanium**）。
   したがって同梱した `onnxruntime.dll`（1.31）ではなく System32 の 1.17 が読み込まれる。

3. **API バージョン照合に失敗すると即死する。**
   `OrtGetApiBase()->GetApi(ORT_API_VERSION)` は古いランタイムで `nullptr` を返し、
   C++ ラッパはそれをそのまま参照する。これは C++ の例外ではないので `catch` できず、
   アクセス違反でプロセスごと落ちる（OBS 側にログが残らないのはこのため）。

実測（同じプロセスでプラグイン DLL だけをロードし、どのモジュールが読み込まれるか比較）:

| ロード方法 | 読み込まれた onnxruntime |
| --- | --- |
| `LoadLibraryW(path)`（= ホストと同じ） | `C:\WINDOWS\SYSTEM32\onnxruntime.dll` **1.17** |
| `LoadLibraryExW(path, 0, LOAD_WITH_ALTERED_SEARCH_PATH)` | 同梱の `…\x86_64-win\onnxruntime.dll` **1.31.0** |

補足: atkAudio のプラグイン一覧で本プラグインが `numInputs="0" numOutputs="0"` と
表示されるのは無害。JUCE 8 のスキャンは `Contents\Resources\moduleinfo.json` の
高速パスを使うためバス情報を持たない（同じ 0/0 は Alt Denoiser / Orra など
moduleinfo.json を持つ他社製でも同様に記録されている）。

### 修正（`src/OrtRuntime.cpp` / `CMakeLists.txt`）

1. `ORT_API_MANUAL_INIT` を定義し、**onnxruntime を import table の依存から外す**。
   参照するのは `OrtApi` 構造体の関数ポインタ経由だけになるので、
   リンカが import を作らない（`dumpbin /dependents` に載らないことを確認済み）。
2. 同梱 DLL を **`dpdfnet_onnxruntime.dll`** という一意な名前にする。
   名前が衝突し得ないので、探索順に依存しない。
3. `OrtRuntime.cpp` が「自分のバイナリの隣（と 3 階層上）」から
   `LoadLibraryExW(..., LOAD_WITH_ALTERED_SEARCH_PATH)` で読み込み、
   `OrtGetApiBase()->GetApi(ORT_API_VERSION)` を検証してから `Ort::InitApi()` する。
   - 古いランタイムを掴んだ場合は**失敗として扱う**（例外を投げない）。
     エディタには `ONNX Runtime unavailable: …`（実際のバージョンとパス）が出る。
   - `DPDFNET_ORT` 環境変数でファイル/ディレクトリを上書きできる（検証用）。

### 検証（回帰テスト）

```powershell
cd C:\dev\dpdfnetvoice-plugin
cmake --build build --config Release --target dpdfnet_ort_smoke
.\build\Release\dpdfnet_ort_smoke.exe models\dpdfnet8_48khz_hr_2out.onnx
```

- `onnx runtime : ONNX Runtime 1.31.0 (…\dpdfnet_onnxruntime.dll)` と出ること
  （1.17 と出たら同梱 DLL の配置ミス）
- `A) level` / `B) lag` の後に `OK` で終了すること
- `B) lag (aligned noisy): lag 2400 samples (5.00 frames)`
  = モデル 4 hop + WOLA 1 hop。プラグインはこれに出力配置の 1 hop を足して
  `kLatencyHops = 6`（2880 サンプル）になる（§13.2 の内訳と一致）

`ort_smoke_test` は **プラグインの C++ 経路を初めて実行するテスト**でもある
（それまでは Python 側の再現（`sim_stream.py`）だけが通っていた）。

