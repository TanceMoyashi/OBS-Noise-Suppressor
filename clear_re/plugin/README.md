# DPDFNet Voice (作業名)

OBS (atkAudio 経由) で使う個人用 VST3。**Supertone Clear の公開機能仕様を参考にした自作プラグイン**であり、
Clear のバイナリやモデルには一切依存していない。

- 音声 (de-noise) + 残響 (de-reverb) のニューラル抑圧
- モデル: **DPDFNet**（Apache-2.0, `Ceva-IP/DPDFNet`）の **48 kHz HR** 版
- 推論: **ONNX Runtime C++**、STFT/iSTFT は自前実装（960 点 Vorbis 窓）
- 対象ホスト: OBS 専用 → **48 kHz のみ**（v0。44.1 kHz 用リサンプラは未実装で、48 kHz 以外ではパススルー）

設計の経緯・確定事項は `../ANALYSIS.md`（特に §13–14）を参照。

---

## 1. 前提ツール

| ツール | 確認済みの状態 |
| --- | --- |
| CMake | ✅ 4.2.0 |
| Visual Studio | ✅ 18 Community（MSVC 19.51、C++ ツール込み） |
| Git | ✅ インストール済み |
| ONNX Runtime C++ | ✅ **1.31.0** prebuilt（`C:\ort\onnxruntime-win-x64-1.31.0`） |
| Python 3.12 | ✅ numpy / onnxruntime 1.31.0 / onnx / `dpdfnet` 0.6.0 導入済み |

> **CMake の注意:** 4.2.0 はソース/作業パスに日本語が含まれると無言でクラッシュする
> （終了コード `0xC0000409`）。ASCII のみのジャンクションを作って**そこで cmake を実行する**:
>
> ```powershell
> cmd /c mklink /J C:\dev\dpdfnetvoice-plugin "<このフォルダの絶対パス>"
> cd C:\dev\dpdfnetvoice-plugin     # 以降のビルドはすべてここから
> ```

## 2. ONNX Runtime C++ の用意

prebuilt を使う（推奨・最短。vcpkg でも可）:

```powershell
# onnxruntime-win-x64-1.31.0.zip を C:\ort に展開して ONNXRUNTIME_ROOT に向ける
cmake -B build -G "Visual Studio 18 2026" -A x64 -DONNXRUNTIME_ROOT=C:\ort\onnxruntime-win-x64-1.31.0
```

> `Expand-Archive` が失敗する環境では `tar.exe -xf onnxruntime-win-x64-1.31.0.zip -C C:\ort` を使う。
>
> ORT 1.31 の C++ API 変更点（この実装で対応済み）:
> `LookupCustomMetadataMapAllocated(key, alloc)`（旧 `...AllocatedAllocator` は無い）、
> `AllocatorWithDefaultOptions` に `GetAllocator()` は無く `Base<T>` の暗黙変換で `OrtAllocator*` を取る。

## 3. モデルの取得

ONNX（Apache-2.0、`Ceva-IP/DPDFNet`）:

```powershell
cd clear_re\plugin
New-Item -ItemType Directory -Force models | Out-Null
# 速度優先 (推奨)
curl.exe -L -o models\dpdfnet2_48khz_hr.onnx `
  https://huggingface.co/Ceva-IP/DPDFNet/resolve/main/onnx/dpdfnet2_48khz_hr.onnx
# 品質優先 (2 倍重い)
curl.exe -L -o models\dpdfnet8_48khz_hr.onnx `
  https://huggingface.co/Ceva-IP/DPDFNet/resolve/main/onnx/dpdfnet8_48khz_hr.onnx
```

検証用の入力音声（sherpa-onnx のリリース。**配布物には含まれない**）:

```powershell
curl.exe -L -o test_signals\speech_with_noise.wav `
  https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/speech_with_noise.wav
```

重み（fine-tune 用、Apache-2.0）:

```powershell
curl.exe -L -o models\dpdfnet2_48khz_hr.pth `
  https://huggingface.co/Ceva-IP/DPDFNet/resolve/main/checkpoints/dpdfnet2_48khz_hr.pth
```

モデル探索順（プラグイン側、`resolveModelFile()`）: **2 出力版を優先**する。

1. 環境変数 `DPDFNET_MODEL`（ファイルでもディレクトリでもよい）
2. `<プラグイン DLL のフォルダ>\models\`
3. そこから 3 階層上までの各 `models\`（VST3 バンドルの `Contents\x86_64-win` から
   `…\VST3\models` に届く）
4. `Documents\DPDFNetVoice\models\`、`Documents\OBS\VST3\models\`

ファイル名の優先順:

```
dpdfnet8_48khz_hr_2out.onnx   ← 品質優先 + Voice Reverb 有効
dpdfnet2_48khz_hr_2out.onnx   ← 速度優先 + Voice Reverb 有効
dpdfnet8_48khz_hr.onnx        ← Voice Reverb はグレーアウト
dpdfnet2_48khz_hr.onnx
```

## 4. ビルド

```powershell
cd C:\dev\dpdfnetvoice-plugin      # ASCII のみのジャンクション (§1 の注意)
cmake -B build -G "Visual Studio 18 2026" -A x64 -DONNXRUNTIME_ROOT=C:\ort\onnxruntime-win-x64-1.31.0
cmake --build build --config Release --parallel 8
```

VST3 は `build\DPDFNetVoice_artefacts\Release\VST3\DPDFNet Voice.vst3` に出力される。
POST_BUILD で ONNX Runtime が `Contents\x86_64-win\dpdfnet_onnxruntime.dll` として入り、
`models\*.onnx` が `…\VST3\models\` にコピーされる（`resolveModelFile()` の探索順と一致）。

> **DLL 名が `onnxruntime.dll` ではない理由（重要）**
> VST3 のモジュールローダ（VST3 SDK `module_win32.cpp`、JUCE 経由）は素の `LoadLibraryW`
> を使うため、依存 DLL の探索順に「プラグイン自身のディレクトリ」が入らない。
> Windows 11 (24H2+) は `C:\Windows\System32\onnxruntime.dll`（Windows ML 同梱、
> **ONNX Runtime 1.17**）を持っており、`onnxruntime.dll` という名前で同梱すると
> そちらが先に読み込まれる。本プラグインは ORT **1.31** 前提（`ORT_API_VERSION = 31`）
> なので API 照合に失敗し、**OBS ごと即死**する（ホスト側にクラッシュログも残らない）。
> 対策として `ORT_API_MANUAL_INIT` で import table から外し、一意名の DLL を
> 絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH` で読み込んでいる（`src/OrtRuntime.cpp`）。
> 詳細は `../ANALYSIS.md` §16。

インストール（**バンドルだけでは動かない。`models\` も必要**）:

```powershell
$dst = "$env:USERPROFILE\Documents\OBS\VST3"
Copy-Item -Recurse -Force "build\DPDFNetVoice_artefacts\Release\VST3\DPDFNet Voice.vst3" $dst
Copy-Item -Recurse -Force "models" "$dst\models"      # *.onnx があればよい
```

## 5. OBS への組み込み

1. **atkAudio** をインストール（OBS 31.1.0+、VST3 用。ネイティブの OBS フィルタは VST2 のみ）。
2. OBS の設定 → 音声 → サンプルレートを **48 kHz** にする。**48 kHz 以外では本プラグインはパススルー**（v0 の制限。エディタに警告が出る）。
3. マイクソース → フィルタ → atkAudio (VST3) を追加 → 本プラグインを選択。
4. レイテンシは `getLatencySamples()` で **2880 サンプル（60 ms @48 kHz）** と申告される。atkAudio 自体も約 40 ms 加算する点に注意。

> **実機確認の手順・数値の期待値・遅延の測り方・切り分けは `../plugin/OBS_VERIFY.md`** にまとめてある。
> テスト信号は `python tools\make_test_signals.py` で作る。

## 6. ノブ

| ノブ | 範囲 | 内容 | 単一出力モデル |
| --- | --- | --- | --- |
| **Ambience** | 0–1 | 残す環境ノイズ量。内部で `α = 10^(−L/20)`, `L = 48 − 42·knob` [dB] として整列入力スペクトルを混ぜる。右に回すほど抑圧が弱い | ✅ |
| **Voice** | ±12 dB | 出力のプレゼンス（軽い高域シェルフ）とゲイン。暫定実装 | ✅ |
| **Voice Reverb** | 0–1 | 残す残響量。`Y = (1−β)·spec_ns + β·spec_e`（β=1 で残響抑圧あり） | ✅（2出力モデル） |
| **Bypass** | on/off | 入力をそのまま通す。**0 遅延**（レイテンシ報告は 2880 のままなので ON/OFF で音の位置が 60 ms 飛ぶ） | — |

> **Bypass は遅延を維持しない。** `processBlock` が先頭で早期 return するため、Bypass 中の出力は
> 遅延 0 になる。プラグインのレイテンシ報告（2880）は変わらないので、ホスト側で 60 ms ぶん
> 補正され、ON/OFF のたびに音の位置が前後に飛ぶ（聞き比べの用途では問題ない）。

**`Voice Reverb` には 2出力モデル（`*_2out.onnx`）が必要。** `models/` に同梱済みで、
`resolveModelFile()` が `*_2out.onnx` を優先して読む（無ければ 1 出力版にフォールバックし、
その場合はノブがグレーアウトする）。

2出力化は `tools\add_second_output.py` による**グラフ手術**（再学習不要・重みは不変）:

```powershell
# 1) 候補を確認
python tools\add_second_output.py --model models\dpdfnet2_48khz_hr.onnx --list
# 2) spec_ns を追加 (マスク適用のみ。DF 経路のフレームバッファ index 2 が spec_e と同位相)
python tools\add_second_output.py --model models\dpdfnet2_48khz_hr.onnx `
    --from-buffer /model/df_op/spec_buffer/Concat_output_0 --buffer-index 2 `
    --out models\dpdfnet2_48khz_hr_2out.onnx
# 3) 検証 (spec_e / state_out が不変・spec_ns が同位相であること)
python tools\verify_two_output.py --base models\dpdfnet2_48khz_hr.onnx `
    --two models\dpdfnet2_48khz_hr_2out.onnx --wav test_signals\speech_with_noise.wav
```

`squeeze` して `inv_wnorm`（= 960.0）を掛けるのは、モデル内部が `spec_e = df_out · inv_wnorm`
という正規化をしているため。`spec_ns` は `mask × 遅延整列済み noisy spec` で、
`spec_e`（mask + deep filter）との差が **残響抑圧の効き幅**になる（実測で相対 −10 dB 程度）。

将来 HF のチェックポイントから RIR fine-tune して 2出力エクスポートすれば、同じ経路でより強い残響除去が得られる（`../ANALYSIS.md` §13.3）。

## 7. 検証・キャリブレーション

```powershell
python -m pip install numpy onnxruntime
python tools\check_onnx_model.py --model models\dpdfnet2_48khz_hr.onnx `
    --wav test_signals\speech_with_noise.wav --out out_check.wav
```

`check_onnx_model.py` は C++ とは**独立に** numpy で STFT/iSTFT を実装しており、次を検証する:

1. metadata の必須キーと `win_len` の復元（`(freq_bins−1)·2`）
2. 初期 state の構築（`erb_norm_init` / `spec_norm_init` + ゼロ埋め）
3. **入出力レベルが整合するか**（`wnorm` を外から掛けない契約の確認。±12 dB を超えたら警告）
4. 再構成の内容が入力と同位相で整列しているか（**`MODEL_FRAME_DELAY = 4` フレームを捨てて**ラグ ≈ 0）

**`--wav` に本物の音声を渡すこと。** 合成信号はモデルに「音声でない」と判断され
−40 dB 以下に抑圧されるため、レベルと遅延の検証には使えない。

### リアルタイム遅延の実測

遅延は「いつ出力できるか」の性質なので、内容の相関では測れない。**出力位置の規約まで含めて
シミュレーションする** `tools\sim_stream.py` が実測する:

```powershell
python tools\sim_stream.py --model models\dpdfnet2_48khz_hr.onnx --wav test_signals\speech_with_noise.wav
```

このツールは C++ の `VorbisStft` + `runHop` + `processBlock` を忠実に再現し、
公式 `StreamEnhancer` / `dpdfnet.enhance` と突き合わせる。結果（48 kHz）:

| 実装 | 内容遅延 |
| --- | --- |
| `dpdfnet.enhance`（オフライン、4 hop 補正込み） | 0（基準） |
| 公式 `StreamEnhancer`（可変長を返しホストにゼロ埋めさせる） | 5 hop = 2400 = 50 ms |
| **本プラグイン**（固定長ブロック） | **6 hop = 2880 = 60 ms** |

**60 ms の内訳**（`src\PluginProcessor.h` の `kLatencyHops` 付近に詳述）:

```
4 hop : モデル自身の内容遅延 (出力フレーム t は入力フレーム t-4)
1 hop : WOLA の構造 (step s で確定する hop は入力 hop s-5 の内容)
1 hop : 完成した hop を「次に読むサンプル」から書くため、出力グリッド上で 1 hop 後ろ
```

6 hop は**固定長ブロックを必ず返す VST3 で実現できる最小値**でもある（公式の 5 hop は、
ブロック長より短い返り値を許してホスト側にゼロ埋めさせる sounddevice 前提の実装）。
`runHop` の合成スキップ数（`c.step >= 2`）を変えても合計は 6 hop のままであることは実測済み。

実機で確認する場合は、本プラグインを通したインパルス（またはクリック）を録音し、
atkAudio の遅延補正を切って入出力の先頭位置を比べる。差が 2880 と違う場合は
`src\PluginProcessor.h` の `kLatencyHops` を合わせる。導出と測定方法は `../ANALYSIS.md` §13.2・§14。

## 9. 開発ツール (`tools\`)

C++ とは独立に Python でモデル/アルゴリズムを検証する。実装を変えたら、
音を聴く前にまずここを通す。

| ツール | 役割 |
| --- | --- |
| `check_onnx_model.py` | ONNX の契約検証（I/O 形状 / metadata / 初期 state / レベル / 4 フレーム遅延）。`--wav` 省略時は合成信号 |
| `sim_stream.py` | **C++ のストリーミング処理を忠実に再現**し、公式実装と突き合わせて内容遅延を実測する |
| `diag_model.py` | レベルとフレーム遅延の診断（モデル遅延 = 4 フレームの根拠） |
| `inspect_graph.py` | ONNX の中間テンソルを producer / consumer でたどる（`--producer-of spec_e --depth 8` など） |
| `add_second_output.py` | グラフ手術で `spec_ns`（マスク経路のみ）を追加し 2 出力化する |
| `verify_two_output.py` | 2 出力モデルの検証（`spec_e` / `state_out` が不変・`spec_ns` が同位相） |
| `bench_model.py` | 1 フレーム（= 10 ms 音声）の推論時間とリアルタイム余裕（RTF）を測る |
| `make_test_signals.py` | OBS 実機確認用のテスト信号（インパルス / 無音ギャップ入り音声 / 残響つき）を作る |
| `measure_effect.py` | テスト信号を 2 出力モデルで処理し、**プラグインが出すはずの数値**を出す |
| `analyze_obs_capture.py` | OBS の録音から遅延と抑圧量を測る（`--selftest` で測定器自体を検証） |
| `ort_smoke_test.cpp` | **C++ 側の検証**（GUI/JUCE 不要）。同梱 ORT が使われているか、`DpdfNet::load` / `processFrame` / STFT が動くか、遅延が 5 hop（モデル 4 + WOLA 1）かを確認する |
| `ui_snapshot.cpp` | **エディタの見た目確認**（OBS 不要）。エディタをヘッドレスで描画して PNG に出す。配色・レイアウト・文字を変えたときの確認用 |

```powershell
# C++ の経路（ORT のロード + 推論）を OBS を起動せずに確認する
cd C:\dev\dpdfnetvoice-plugin
cmake --build build --config Release --target dpdfnet_ort_smoke
.\build\Release\dpdfnet_ort_smoke.exe models\dpdfnet8_48khz_hr_2out.onnx
#   onnx runtime  : ONNX Runtime 1.31.0 (…\dpdfnet_onnxruntime.dll)   ← 1.17 なら異常
#   B) lag (aligned noisy): lag 2400 samples (5.00 frames)           ← 4 hop + WOLA 1 hop
```

インストール済みのファイルを検証したいときは `DPDFNET_ORT` でディレクトリを指定する:

```powershell
$env:DPDFNET_ORT = "D:\Users\Syntac\Documents\OBS\VST3\DPDFNet Voice.vst3\Contents\x86_64-win"
.\build\Release\dpdfnet_ort_smoke.exe "D:\Users\Syntac\Documents\OBS\VST3\models\dpdfnet8_48khz_hr_2out.onnx"
```

### エディタの見た目（PNG スナップショット）

```powershell
# 2 出力モデル（Voice Reverb あり）と 1 出力モデル（Reverb はグレーアウト）を描く
cmake --build build --config Release --target dpdfnet_ui_snapshot
.\build\dpdfnet_ui_snapshot_artefacts\Release\dpdfnet_ui_snapshot.exe ui.png
$env:DPDFNET_MODEL = "models\dpdfnet8_48khz_hr.onnx"
.\build\dpdfnet_ui_snapshot_artefacts\Release\dpdfnet_ui_snapshot.exe ui_1out.png
```

`DPDFNET_MODEL` / `DPDFNET_ORT` の探索はプラグイン本体と共通なので、実機と同じモデル・同じ ORT が読まれる。

### CPU（実測: 1 フレーム = 10 ms 音声、`intra_op=1 / inter_op=1`）

| モデル | mono | stereo（2 インスタンス） | RTF |
| --- | --- | --- | --- |
| `dpdfnet2_48khz_hr_2out` | 0.83 ms | 1.66 ms | 0.17 |
| `dpdfnet8_48khz_hr_2out` | 2.39 ms | 4.79 ms | 0.48 |

```powershell
python tools\bench_model.py --models models\dpdfnet2_48khz_hr_2out.onnx models\dpdfnet8_48khz_hr_2out.onnx
```

どちらも 1 コアで余裕がある（RTF < 0.5）。既定は品質優先で `dpdfnet8` を読む。
軽くしたい場合は `dpdfnet2_48khz_hr.onnx` を `models\` から外す（探索順が 2 に落ちる）。

```powershell
python tools\inspect_graph.py --model models\dpdfnet2_48khz_hr.onnx --producer-of spec_e --depth 8
python tools\sim_stream.py --model models\dpdfnet2_48khz_hr.onnx --wav test_signals\speech_with_noise.wav
```

## 10. 既知の制限 / 今後

- **48 kHz 専用**（v0）。44.1 kHz 対応はリサンプラ実装後。48 kHz 以外ではパススルーになり、エディタに警告が出る。
- **Mono/Stereo トグルと Eco モードは未実装**（Clear にはある）。ステレオは L/R 独立に 2 インスタンスで処理する（CPU は約 2 倍）。
- `Voice Reverb` の効き幅はモデル内蔵の DF 段の差ぶん（相対 −10 dB 程度）に限られる。
  より強い残響除去は RIR で fine-tune して再エクスポートする（`../ANALYSIS.md` §13.3）。
- `Voice` は暫定実装（学習された制御ではない）。
- 解析の初期履歴（1 hop 分）はゼロ埋め（訓練時は `pad_mode="reflect"`）。ストリーム先頭 20 ms のみ影響。
- ループバックや再利用時に `reset()` を呼ぶ（ホストのトランスポート停止では呼ばれない場合がある）。
