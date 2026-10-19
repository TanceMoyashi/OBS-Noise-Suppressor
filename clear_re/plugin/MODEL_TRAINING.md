# 中間モデル (dpdfnet4 相当) の作成計画と実測記録

このドキュメントは「dp8 (高品質・重い) と dp2 (軽量・品質低) の中間の 48 kHz モデル」を
新しく作る作業の計画書 + 実測ログ。作業の途中経過もここに追記していく。

対象リポジトリ: `OBS-Noise-Suppressor` / 作業場所: `clear_re/plugin/`
（プラグイン本体 `clear_re/dpdfnet/` `clear_re/obs_plugin/` `clear_re/vst3/` は別担当が編集中のため触らない）

---

## 0. 進捗サマリ（随時更新）

| # | 項目 | 状態 |
|---|---|---|
| 1 | 計画書 | 完了（このファイル） |
| 2 | 上流の入手（.pth / 実装 / clone） | 完了 |
| 3 | 数値一致の関門（torch == 公式 ONNX） | **通過**（§5.1） |
| 4 | 中間モデルの構成を確定 | 完了: `dprnn_num_blocks=4`（= dp4）が上流の定義そのもの（§5.2） |
| 5 | 学習 | 未着手 |
| 6 | ONNX 書き出し + 2 出力化 + 契約検証 | 未着手 |
| 7 | 速度・品質の実測 | 未着手 |

**次にやること**: dp4 の初期値（dp8 の DPRNN ブロック 0〜3）を作り、蒸留の学習ループを回す。

---

## 1. 目的と目標値

### 背景（実測済みの事実）

このプラグインの CPU 負荷はほぼ全部が ONNX 推論。実測では dp8 2out ステレオで
1 ホップ = 5.63 ms のうち 5.61 ms が推論（99.7 %）。予算は 1 ホップ 480 サンプル = 10 ms。

| モデル | ステレオ 1 ホップ | 予算比 | 品質 |
|---|---|---|---|
| `dpdfnet8_48khz_hr_2out.onnx` | 5.6 ms | 56 % | 高い |
| `dpdfnet2_48khz_hr_2out.onnx` | 2.0 ms | 20 % | 落ちる |

INT8 量子化（動的・静的とも）は実測で不可（`tools/quantize_model.py` /
`tools/quantize_static.py` の docstring に結論と実測値）。ORT のセッション設定の詰めも
既に最適（`clear_re/dpdfnet/src/net.rs` に実測コメントあり）。

### 目標

**ステレオ 1 ホップ 3〜4 ms で dp8 に近い品質**の第 3 のモデルを作り、選択肢に加える。

### 成果物の形

* `clear_re/plugin/models/dpdfnet4_48khz_hr_2out.onnx`（新モデル。既存 4 ファイルは変更しない）
* 再現手順と実測値（このドキュメント）
* 学習・検証スクリプト（`clear_re/plugin/tools/dpdfnet_mid/`）

---

## 2. 上流の入手元とライセンス

| 何 | どこ | ライセンス |
|---|---|---|
| モデルの重み `dpdfnet2_48khz_hr.pth` / `dpdfnet8_48khz_hr.pth` | Hugging Face `Ceva-IP/DPDFNet` (`checkpoints/`) | Apache-2.0 |
| モデル実装 (PyTorch / ONNX エクスポート) | GitHub `ceva-ip/DPDFNet` (`model/`, `onnx_model/`) | Apache-2.0 |
| 論文 | arXiv 2512.16420 (DPDFNet: Boosting DeepFilterNet2 via Dual-Path RNN) | — |
| 学習データ | 後述（§6） | データごとに記載 |

DPDFNet は DeepFilterNet2 の上に Dual-Path RNN (DPRNN) ブロックを足した構成で、
**48 kHz fullband モデルは `dprnn_num_blocks` だけが dp2 / dp8 の違い**
（README の表: dp2 = DPRNN 2 ブロック, dp8 = 8 ブロック）。
つまり **dp4 = DPRNN 4 ブロック** が「中間モデル」の自然な定義で、アーキテクチャに
新しい部品は要らない（= 重みの形も同じなので warm start できる）。

上流リポジトリはすべて Apache-2.0。派生物（新しい重み）も Apache-2.0 を継承させ、
クレジットを明記する（§9）。

---

## 3. 環境

| 項目 | 実測値 |
|---|---|
| OS | Windows / PowerShell 5.1 |
| Python | 3.12.10 |
| torch | 2.6.0+cu124 / `torch.cuda.is_available() == True` |
| GPU | NVIDIA GeForce RTX 4070 Ti (12 GB, driver 617.42) |
| onnx | 1.23.2 |
| onnxruntime (検証用) | 1.24.4 |
| numpy | 2.5.4 |
| einops | 0.8.2（追加インストール。上流の学習モデル実装が要求） |
| soundfile / librosa / scipy | あり |
| 上流コードの clone 先 | `clear_re/plugin/build/dpdfnet_train/upstream`（`build/` は .gitignore 済み） |

配布するプラグイン側の onnxruntime は 1.31.0 だが、**I/O 契約は ONNX グラフの形なので
バージョン差では壊れない**（新モデルでも 1.24.4 で検証し、既存モデルと同じ扱いにする）。

---

## 4. I/O 契約（これを守らないとプラグインで動かない）

`tools/sim_stream.py` と `clear_re/dpdfnet/src/net.rs` が正。

* 入力 `spec`: float32 `[1, 1, 481, 2]`（481 = n_fft 960 の rfft ビン、2 = 実部/虚部）
* 入力 `state_in`: float32 `[90228]`（先頭に erb/spec 正規化の初期値）
* 出力 `spec_e`: float32 `[1, 1, 481, 2]`、`state_out`: float32 `[90228]`
* メタデータ: `erb_norm_init` / `spec_norm_init` / `erb_norm_state_size` / `spec_norm_state_size`
  / `state_size` / `window_type=vorbis` など（既存と同じ意味）
* 1 ホップ = 480 サンプル、n_fft = 960（窓長 960、Vorbis 窓、center=True 相当）
* 2 出力化は既存の `tools/add_second_output.py`（マスク経路の出力 `spec_ns` を足すだけ）

state の内訳は上流 `onnx_model/dpdfnet_48khz_hr.py` の `state_size()` の順序に従う:

```
erb_norm(481) + spec_norm(96) + enc(...) + erb_dec(...) + df_dec(...) + mask(...) + df_op(...)
= 90228
```

`erb_norm_init` は上流 `InitMagNorm.get_ampirical_mu_0(481)`、`spec_norm_init` は
`InitSpecNorm.get_ampirical_s_0(96)` から作られる（既存モデルのメタデータと一致することを
§5 で確認する）。

---

## 5. 進め方と実測記録

### 5.1 数値一致の関門（最重要）— **通過 (2026-10-19)**

上流の `.pth` とモデル実装を取り込み、**torch で再現した出力が公式 ONNX と一致する**ことを
確かめた。ここが通らないと学習は無意味なので最初に確定させる。

* 使うスクリプト: `tools/dpdfnet_mid/parity_check.py`（± `parity_drift.py` / `parity_state_segments.py`）
* 経路: 上流 `onnx_model/dpdfnet_48khz_hr.py` のストリーミング実装を `.pth` から構築し、
  公式エクスポートと同じラッパー `DPDFNet48HROnnxWrapper`（`spec*wnorm → model → *inv_wnorm`）
  経由で推論。公式 ONNX に同じ `spec` を 1 フレームずつ流して比較。
* 入力: `test_signals/speech_tail_48k.wav` と `speech_tail_reverb_48k.wav`（各 16.2 秒 = 1621 フレーム）

| モデル | state_size | norm 初期値の差 | t=0 の spec_e 最大差 | 全体 SNR |
|---|---|---|---|---|
| dp2 | 56436 (ONNX と一致) | 0（完全一致） | **0.000e+00（ビット一致）** | 75.6 dB / 65.2 dB |
| dp8 | 90228 (ONNX と一致) | 0（完全一致） | 0.000e+00 | （別途計測。下の注記を参照） |

**結論**:
1. `correct_state_dict()` で `.pth` → ストリーミング実装の変換は正しい。
2. state の並び（先頭 481 = erb 正規化 mu、次の 96 = spec 正規化 s、以下 GRU 隠れ状態…）と
   正規化の初期値は公式 ONNX のメタデータと完全一致。
3. t=0 の `spec_e` は**ビット完全一致**。以降の差は GRU 隠れ状態の float32 丸めが
   時間方向に残るだけで、実音声 16 秒でも SNR 65〜76 dB（＝実用上は同一）。

補足（重要な発見）: 手元の上流実装で `dpdfnet2_48khz_hr.pth` を
`onnx_model/export_dpdfnet_48khz_hr_to_onnx.py` で再エクスポートした `fresh_dp2.onnx` は、
**配布中の `models/dpdfnet2_48khz_hr.onnx` と同一の出力**（同じ入力に対して差の値まで一致）を
返した。つまり上流 main のコードで公式 ONNX を再現できる。→ 中間モデルのエクスポートに
上流コードをそのまま使ってよい。

失敗していた原因（記録）: 最初はラッパーを通さず生 `spec` を torch モデルへ直接入れて
いた（ONNX の `spec` は `wnorm` を掛ける前の生スペクトル）。このため code 内の
`_feature_extraction` のスケールがずれ、大きく不一致になった。`DPDFNet48HROnnxWrapper`
を通すことで解決。

### 5.2 中間モデルの構成

`DPDFNet48HR(dprnn_num_blocks=4)`。重みの初期値は **dp8 の DPRNN ブロック 0〜3** を
そのまま使い、それ以外（conv / GRU / 正規化 / DF 経路）は dp8 からコピーする。
DPRNN ブロックは residual 加算なので、後半 4 ブロックを落とした状態から微調整すると
「ゼロから 4 ブロックを学習」より速く品質が戻る。

### 5.3 学習（蒸留）

* teacher: dp8（手元の `.pth`。同じ上流実装で動かせる）
* student: dp4（上記の初期値）
* 損失: `spec` の MSE（実部・虚部、teacher 出力を目標）＋（余裕があれば）iSTFT 後波形の MSE
* データ: §6。ラベル不要（teacher 出力を目標にする）
* **1 回あたり 45 分以内**で区切り、チェックポイントを残して再開できるようにする

### 5.4 書き出しと検証

1. torch → ONNX（上流 `onnx_model/export_dpdfnet_48khz_hr_to_onnx.py` を
   `--dprnn-num-blocks 4` で使う。契約どおりの `spec` / `state_in` → `spec_e` / `state_out`）
2. `tools/add_second_output.py` で 2 出力化 → `dpdfnet4_48khz_hr_2out.onnx`
3. `tools/check_onnx_model.py` / `tools/verify_two_output.py` / `tools/inspect_graph.py` で確認
4. 速度: `tools/bench_model.py` でステレオ 1 ホップの中央値を dp2 / dp8 と並べる
5. 品質: teacher との SNR、`tools/measure_effect.py` での抑圧量、確認用 WAV 出力
6. プラグインでの実検証: `clear_re/dpdfnet` のオフラインテストを `DPDFNET_MODEL=<新モデル>`
   に向けて走らせる（プラグイン側は変更しない）

---

## 6. 学習データ

現状手元の音声は `clear_re/plugin/test_signals/` の 3.6 MB のみ。学習用に追加取得する。

（取得したデータ・サイズ・ライセンスはここに追記する）

---

## 7. 実測結果（まとめ表）

（測ったら追記する）

---

## 8. 付録: 学習の再現手順

（確定したら追記する）

---

## 9. ライセンスとクレジット

* 上流の重み・実装は Apache-2.0 (`Ceva-IP/DPDFNet`, `ceva-ip/DPDFNet`)。
  派生した新モデルも Apache-2.0 とする。
* 表記すべきクレジット: DPDFNet (Rika, Sapir, Gus, 2025) / DeepFilterNet2 / Apache-2.0。
* 学習データのライセンスは §6 に記載する。
