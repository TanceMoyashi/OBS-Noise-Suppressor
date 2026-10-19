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
| 5 | 学習データの用意 | LibriSpeech dev-clean を 48 kHz にデコードして準備（§6） |
| 6 | 学習（dp8 からの蒸留で微調整） | **実施中**（§5.3 / §7） |
| 7 | ONNX 書き出し + 2 出力化 + 契約検証 | パイプラインは動作確認済み（§5.4）。学習後に本番の重みで再実行 |
| 8 | 速度・品質の実測 | 未着手 |

**次にやること**: `tools/dpdfnet_mid/distill.py` を 45 分以内で回し、teacher との SNR が
頭打ちになったら書き出して dp2 / dp8 と並べて測る。

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
* 入力 `state_in`: float32 `[state_size]`（先頭に erb/spec 正規化の初期値）。**state_size は
  DPRNN ブロック数で変わる**: dp2 = 56436、dp4（新） = 67700、dp8 = 90228。プラグイン側は
  メタデータ `state_size` / `erb_norm_state_size` / `spec_norm_state_size` を読むので
  モデルごとの差は吸収される（`clear_re/dpdfnet/src/net.rs` の実装を確認済み）
* 出力 `spec_e`: float32 `[1, 1, 481, 2]`、`state_out`: float32 `[90228]`
* メタデータ: `erb_norm_init` / `spec_norm_init` / `erb_norm_state_size` / `spec_norm_state_size`
  / `state_size` / `window_type=vorbis` など（既存と同じ意味）
* 1 ホップ = 480 サンプル、n_fft = 960（窓長 960、Vorbis 窓、center=True 相当）
* 2 出力化は既存の `tools/add_second_output.py`（マスク経路の出力 `spec_ns` を足すだけ）

state の内訳は上流 `onnx_model/dpdfnet_48khz_hr.py` の `state_size()` の順序に従う:

```
erb_norm(481) + spec_norm(96) + enc(...) + erb_dec(...) + df_dec(...) + mask(...) + df_op(...)
= 90228 (dp8) / 56436 (dp2) / 67700 (dp4)
```

dp2 の実測内訳（`tools/dpdfnet_mid/parity_state_segments.py` の出力）:

| 区間 | サイズ | 内容 |
|---|---|---|
| 0..481 | 481 | erb 正規化 (MagNorm48) の mu |
| 481..577 | 96 | spec 正規化 (SpecNorm48) の s |
| 577..2020 | 1443 | enc.erb_conv0 のフレームバッファ |
| 2020..7140 | 5120 | enc.dprnn_erb (DPRNN の隠れ状態 / バッファ) |
| 7140..7716 | 576 | enc.df_conv0 のバッファ |
| 7716..13860 | 6144 | enc.dprnn_df |
| 13860..14116 | 256 | enc.emb_gru |
| 14116..14628 | 512 | erb_dec.emb_gru |
| 14628..15140 | 512 | df_dec.df_gru |
| 15140..45860 | 30720 | df_dec.df_convp のバッファ |
| 45860..48746 | 2886 | mask (MagnitudeMask) の 2 フレーム分 |
| 48746..56436 | 7690 | df_op (DF の履歴バッファ) |

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

### 5.2 中間モデルの構成 — **確定**

`DPDFNet48HR(dprnn_num_blocks=4)`（dp4）。上流の定義そのままで、追加部品は無い。

上流実装を読んで分かった重要ポイント（`model/dpdfnet_48khz_hr.py` / `model/modules.py`）:

* `e0, e1, e2, e3` は **ERB 側 conv の出力 (skip connection)** で、DPRNN ブロックの出力では
  ない。DPRNN ブロックは `e3` を refine して `e3_dprnn` を作り、そこから `emb`（時間方向の
  埋め込み）を計算する。DF 側も同様に `c1 -> c1_dprnn -> cemb`。
* したがって **DPRNN ブロック数が変わって影響を受けるのは `emb` だけ**。decoder の skip
  (`e0..e3`, `c0`) は dp8 と共通。
* つまり「dp8 の先頭 4 ブロックをコピーした dp4」は、`emb` が未調整のまま = decoder への
  入力分布がずれる。

**実測（warm start の素の状態）**: この初期値のモデルは **出力がほぼ無音に潰れる**
（`spec_e` rms 4e-6、マスクが ~0）。DPRNN ブロックが residual 積み上げなので、後半 4
ブロックを落とすと `emb` が大きくずれ、decoder の sigmoid が閉じてしまうため。参照として
**学習済みの dp2 は同じ入力でちゃんと動く**（teacher との spec SNR 18.5 dB）。

→ 結論: **warm start は「学習の出発点」としてのみ使い、微調整 (蒸留) は必須**。
ゼロから 4 ブロックを学習するよりは圧倒的に速く収束する（重みの大半は dp8 と同一）。
初期値は `tools/dpdfnet_mid/build_mid.py` で作成（693 テンソルを dp8 からコピー、
初期値のまま残るテンソルは 0）。

### 5.3 学習（蒸留）— 実装済み

* teacher: dp8（手元の `.pth`。同じ上流オフライン実装で動かせる）
* student: dp4（§5.2 の初期値）
* 損失: teacher の `spec_e` に合わせる。
  `MSE(|S|^0.3, |T|^0.3) + 0.25 * MSE(S, T)`（振幅のべき乗圧縮でレベル差の影響をならし、
  複素 MSE を弱く足す）。プラグインが出すのは最終的に `spec_e` なのでそこを直接合わせる。
* データ: §6。ラベル不要（teacher 出力を目標にする）。
* 入力レベル: 学習と評価で同じ土俵に乗せるため、`test_signals/speech_tail_48k.wav` の RMS に
  合わせる（モデルの `MagNorm48` / `SpecNorm48` は想定入力レベルを持つため）。±4 dB の
  ランダムゲインは掛ける。
* `tools/dpdfnet_mid/distill.py`。**1 回あたり 45 分以内**で区切り（`--minutes`）、
  `--save-every` でチェックポイントを残して `--resume` で再開できる。`--val-every` ごとに
  固定の検証信号で teacher との spec SNR を測り、最良を `*.best.pth` に保存する。

### 5.4 書き出しと検証 — **パイプライン動作確認済み**

`tools/dpdfnet_mid/export_mid.py` が 1〜3 をまとめて実行する。

1. torch → ONNX（上流 `onnx_model/export_dpdfnet_48khz_hr_to_onnx.py` を
   `--dprnn-num-blocks 4` で使う。契約どおりの `spec` / `state_in` → `spec_e` / `state_out`）
2. `onnx.shape_inference.infer_shapes()` で中間テンソルの shape 情報 (value_info) を付ける
   * **必須**: `tools/add_second_output.py` は `spec_ns` に使う `/model/mask/Mul_output_0` の
     rank を value_info から判定して Squeeze を挟む。これが無いと `spec_ns` が
     `[1,1,1,481,2]` になって契約違反になる（実測で確認）。上流エクスポートは onnxsim が
     無いと value_info を付けないため、明示的に shape inference を走らせる。
3. `tools/add_second_output.py --tensor-name /model/mask/Mul_output_0` で 2 出力化
4. `tools/check_onnx_model.py` / `tools/verify_two_output.py` / `tools/inspect_graph.py` で確認
5. 速度: `tools/bench_model.py` でステレオ 1 ホップの中央値を dp2 / dp8 と並べる
6. 品質: teacher との SNR、`tools/measure_effect.py` での抑圧量、確認用 WAV 出力
7. プラグインでの実検証: `clear_re/dpdfnet` のオフラインテストを `DPDFNET_MODEL=<新モデル>`
   に向けて走らせる（プラグイン側は変更しない）

#### パイプラインの動作確認（初期値モデルで実施、2026-10-19）

`init_dp4.pth`（未学習・出力はほぼ無音）で書き出しと 2 出力化を通した結果:

| 確認 | 結果 |
|---|---|
| 1 出力 ONNX の I/O | `spec [1,1,481,2]` / `state_in [67700]` → `spec_e [1,1,481,2]` / `state_out [67700]` |
| 2 出力 ONNX の I/O | 上記 + `spec_ns [1,1,481,2]` |
| `verify_two_output.py` [1] `spec_e` ビット一致 | **0.000e+00 (OK)** |
| `verify_two_output.py` [2] `state_out` ビット一致 | **0.000e+00 (OK)** |
| `verify_two_output.py` [3] `spec_ns` が別信号 | OK |
| 生成の所要時間 | エクスポート〜検証で数分（onnxsim 無しのためグラフ最適化は省かれる） |

※ `init_dp4` は無音なので `verify_two_output.py` の遅延推定 ([4]) は相関が取れず
`spec_ns` の lag が -2 hop と出る。学習後のモデルで再確認する。

---

## 6. 学習データ

現状手元の音声は `clear_re/plugin/test_signals/` の 3.6 MB のみだったので追加取得した。

| データ | 用途 | サイズ | ライセンス |
|---|---|---|---|
| LibriSpeech dev-clean (OpenSLR SLR12, `dev-clean.tar.gz`) | クリーン音声 | 338 MB (0.3 GB) | **CC BY 4.0** (Panayotov et al. 2015) |
| LibriSpeech dev-clean を 48 kHz にデコードしたもの (`speech_48000.f32` + `.json`) | 学習時のランダム窓 | 約 2 dB 相当（`prepare_data.py` の出力） | 同上（派生物） |
| `test_signals/rir_48k.wav` | 残響オーギュメント | 既存 | リポジトリ内 |

ノイズは**合成**した（白色 / ピンク / ブラウン / ハム (50/60 Hz 高調波) / バブル (LibriSpeech
の他発話を重ねる) / 非定常 (街頭風の振幅変調) / クリック）。理由:

* MUSAN (11 GB) / DNS-Challenge (数十 GB) は今回の作業時間とレート制限に対して大きすぎる。
* **蒸留では教師 (dp8) の出力が目標**なので、教師が見たことのある実ノイズ分布を完全に覆う
  必要は薄い。学習の目的は「4 ブロックでも同じ `emb` を作れるようにする」ことで、
  入力の多様性はそこそこあれば足りる。

**次の改善（時間ができたら）**: MUSAN の noise subset と DNS-Challenge の noise を足す。
手順は `prepare_data.py` の後に `distill.py` の `NOISE_KINDS` に実ファイル読み込みを
足すだけ（`build_batch()` の `make_noise()` を差し替える）。

`prepare_data.py` はダウンロード・展開・48 kHz デコード（mono / DC 除去 / ピーク 0.9）まで
行い、1 本の raw float32 と index (`.json`) を `build/dpdfnet_train/` に作る。

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
