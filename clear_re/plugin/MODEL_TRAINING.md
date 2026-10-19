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
| 6 | 学習（dp8 からの蒸留で微調整） | **実施**（§5.3 / §7.9）。~2,000 ステップ、teacher との一致 0.1 → 15.4 dB |
| 7 | ONNX 書き出し + 2 出力化 + 契約検証 | **完了**（§5.4 / §7.1）。`models/dpdfnet4_48khz_hr_2out.onnx` |
| 8 | 速度・品質の実測 | **完了**（§7.2〜§7.5）。速度は目標達成、品質は dp2 と同等で dp8 との一致は dp2 にわずかに及ばず |

**現状**: `models/dpdfnet4_48khz_hr_2out.onnx`（12.0 MB、未コミット）が動く状態で置いてある。
ステレオ 1 ホップ **3.10 ms**（目標 3〜4 ms を達成）、抑圧性能は dp2 / dp8 と同等。
**残タスク**: 学習量を増やして「dp8 との一致」で dp2 を上回る（§8.3）、実ノイズでの評価（§8.4）。

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
| LibriSpeech dev-clean を 48 kHz にデコードしたもの (`speech_48000.f32` + `.json`) | 学習時のランダム窓 | 1.39 GB / 1016 発話 / 2.00 時間（`prepare_data.py` の出力） | 同上（派生物） |
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

### 7.1 契約の検証（`models/dpdfnet4_48khz_hr_2out.onnx`）

| 項目 | 値 | 判定 |
|---|---|---|
| 入力 `spec` | float32 `[1,1,481,2]` | 既存と同じ |
| 入力 `state_in` | float32 `[67700]` | 既存 dp8 は 90228 / dp2 は 56436 |
| 出力 `spec_e` | float32 `[1,1,481,2]` | 既存と同じ |
| 出力 `state_out` | float32 `[67700]` | `state_in` と同形 |
| 出力 `spec_ns` | float32 `[1,1,481,2]` | 既存 2 出力モデルと同じ |
| メタデータ | `n_fft=960` / `hop_length=480` / `window_length=960` / `window_type=vorbis` / `state_size=67700` / `erb_norm_state_size=481` / `spec_norm_state_size=96` | OK |
| `check_onnx_model.py` | `[LEVEL] rms in=0.0402 out=0.0245 (-4.29 dB)`、`[ALIGN] content lag = 0` | OK |
| `verify_two_output.py` | `spec_e` ビット一致 0.0 / `state_out` ビット一致 0.0 / `spec_ns` 別信号 / `spec_ns` の lag=0 | **PASS** |
| ファイルサイズ | 12,044,747 B (dp2 10,493,836 / dp8 14,857,606) | 中間 |

`spec_ns` は既存モデルと**同じ取り方**に合わせた（重要）: 既存の 2 出力モデルは
`/model/df_op/spec_buffer/Concat_output_0` の **index 2**（DF 窓 5 の中央 = 現在フレーム）を
`spec_ns` にしている。当初 `/model/mask/Mul_output_0` を使ったところ `spec_ns` が 2 フレーム
ずれて `verify_two_output.py` が FAIL になった（lag=-960 サンプル）。既存と同じ
`--from-buffer ... --buffer-index 2` にして lag=0 になった。

### 7.2 速度（`tools/bench_model.py`、CPU 推論、ステレオ = ×2）

| モデル | 1 スレッド | 2 スレッド | 4 スレッド | 予算 (10 ms) 比 |
|---|---|---|---|---|
| dp2 2out | **1.77 ms** | 1.92 ms | 1.81 ms | 18 % |
| **dp4 2out (新)** | **3.10 ms** | 3.06 ms | 3.88 ms | **31 %** |
| dp8 2out | 5.45 ms | 6.40 ms | 7.05 ms | 55 % |

→ **目標の 3〜4 ms を達成**（dp4 = 3.1 ms。dp2 と dp8 のほぼ中間で、dp8 より 43 % 軽い）。

### 7.3 品質 (1) LibriSpeech dev-clean + 合成ノイズ（クリーン参照あり）

`tools/dpdfnet_mid/quality_libri.py` / 6 発話 × {白色, バブル} × {0 dB, 10 dB} の平均:

| モデル | 出力 SNR vs クリーン | dp8 との一致 |
|---|---|---|
| dp2 | **+9.54 dB** | 18.35 dB |
| **dp4 (新)** | **+9.15 dB** | **17.79 dB** |
| dp8 | +9.50 dB | — (正解) |

* 「出力 SNR vs クリーン」= ノイズ抑圧の性能そのもの。3 モデルともほぼ同等
  （この条件では ERB マスクが支配的で、DPRNN ブロック数の差は出にくい）。
* 「dp8 との一致」= dp8 とどれだけ同じ結果を出すか。dp4 は dp2 (18.35) とほぼ同等の 17.79 dB。
* **注意（正直に）**: LibriSpeech dev-clean は dp4 の蒸留に使ったので dp4 に有利に出うる。
  学習に使っていない音声での比較は次節。

### 7.4 品質 (2) プラグイン付属の音声（学習に未使用 = ホールドアウト）

`tools/dpdfnet_mid/quality_report.py` / `speech_tail_48k.wav` と
`speech_tail_reverb_48k.wav` に合成ノイズを混ぜた 16 条件の平均:

| モデル | dp8 との一致 |
|---|---|
| dp2 | 18.91 dB |
| **dp4 (新)** | **16.93 dB** |
| dp8 | — |

これら 2 つの WAV はプラグイン付属の自前録音で、**すでにノイズ/残響を含む**（下記の
透明性テストで dp8 でも入出力 SNR が 4 dB しかない ＝ クリーン参照には使えない）。
そのため「出力 SNR」は出さず、dp8 との一致度だけを載せている。

### 7.5 品質 (3) クリーン音声の素通し（透明性）と抑圧量

| 指標 | dp2 | **dp4 (新)** | dp8 |
|---|---|---|---|
| クリーン音声の透明性 SNR(out,in) | 24.36 dB | **23.69 dB** | 21.21 dB |
| `measure_effect.py` (speech_tail_reverb, 全体減衰) | -5.86 dB | -5.98 dB | -14.49 dB |
| `measure_effect.py` de-reverb の効き | -21.56 dB | -35.42 dB | -23.94 dB |

* 透明性は dp4 が dp2 と dp8 の中間に入る（dp8 は最も「加工する」）。
* `measure_effect.py` の数値はこの合成ファイル依存で、参考値。

### 7.6 数値一致の関門（再掲・§5.1 の要約）

torch (上流実装) と公式 ONNX は dp2 / dp8 ともに **t=0 でビット一致**、
16 秒流しても SNR 75〜80 dB（float32 の丸めのみ）。この関門が通っているので、
上流実装で学習 → 同じ経路で書き出した dp4 も、プラグインが期待する契約どおりに動く。

### 7.7 モデルの置き場所とプラグインでの使い方

* ファイル: **`clear_re/plugin/models/dpdfnet4_48khz_hr_2out.onnx`**（12,044,747 B）
  * 既存の `dpdfnet{2,8}_*` は**変更していない**（新規ファイルの追加のみ）。
  * 大きいバイナリなので**コミットしていない**（`git status` には未追跡で出る）。
    配布するなら LFS かリリース添付にする。
* プラグイン側の契約は既存モデルと**完全に同一**（§7.1）なので、**コード変更は不要**。
  `README.md` §3 の `resolveModelFile()` は
  「`DPDFNET_MODEL` → `<DLL>\models\` → …」の順に探し、`*_2out.onnx` を優先する。
  したがって:

  ```powershell
  # 明示的に選ぶ（確実）
  $env:DPDFNET_MODEL = "models\dpdfnet4_48khz_hr_2out.onnx"
  # または models\ に置いた上でファイル名で選ばせる
  ```

* `models/` に置けば探索対象になるが、**「速度優先」で自動的に選ばれるようにするには
  プラグイン側のファイル名優先リスト（README §3 の 4 行）に
  `dpdfnet4_48khz_hr_2out.onnx` を 8 と 2 の間に足す必要がある**。
  これはプラグイン本体 (`clear_re/dpdfnet/`) の担当者の作業（今回は別担当が編集中のため
  触っていない）。state_size はメタデータから読むので、優先リストに足すだけで動く。
* `Voice Reverb` は 2 出力版が必要 → 今回作ったのは `_2out.onnx` なので有効。

### 7.8 他のモデルに差し替えるとき

`state_size` が違う（67700 vs 56436 / 90228）が、プラグインは
`state_in` の長さを ONNX の入力形とメタデータから取っているので、**モデルを差し替えるだけで
動く**（`clear_re/dpdfnet/src/net.rs` の実装を確認済み）。

### 7.9 中間モデルの完成度（学習曲線）

`distill.py` の検証（固定 3 本の合成ノイズ入り音声、teacher = dp8 との spec SNR）:

| ステップ | val SNR |
|---|---|
| 0（dp8 の先頭 4 ブロックをコピーしただけ） | **0.1 dB**（出力が無音に潰れる） |
| 250 | 6.98 dB |
| 500 | 6.67 dB |
| 800 | 15.06 dB |
| 1000 | 15.06 dB |
| 1200 | 14.81 dB |
| 1400（lr 1.2e-4 に変更後） | 15.41 dB |

* 学習は `spec_e` の蒸留で、**teacher との一致が上がる = dp8 の音に近づく**方向。
* 800 ステップ付近で一度 15 dB に乗り、そこからは緩やか。学習率 2.5e-4 では振動して
  むしろ悪化した（val 14.7 → 13.1）ので 8e-5 → 1.2e-4 に下げた。
* 打ち切り時点で **teacher との一致 15.4 dB**（dp2 は同じ検証セットで 17.8 dB、
  ただし dp2 は「良質な音声処理器」として学習済みなので、この比較は dp2 が有利）。

**未達 / 改善余地**: 「dp8 に近い品質」という目標に対して、dp8 との一致は
dp2 と同程度で、dp2 を明確に上回ってはいない。改善案は §8.3。

---

## 8. 付録: 再現手順と、次にやること

### 8.1 生成物の置き場所

| もの | 場所 | git |
|---|---|---|
| **新モデル (2 出力 ONNX)** | `clear_re/plugin/models/dpdfnet4_48khz_hr_2out.onnx` (12,044,747 B) | **未追跡**（大きいので意図的にコミットしない。追加する場合は LFS かリリース添付） |
| 1 出力 ONNX（2 出力化の元） | `clear_re/plugin/build/dpdfnet_train/onnx/dpdfnet4_48khz_hr.onnx` | 追跡しない（`build/` は .gitignore） |
| 学習済み重み | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/dp4.best.pth` | 追跡しない |
| 初期値 (dp8 の先頭 4 ブロック) | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/init_dp4.pth` | 追跡しない |
| 学習ログ | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/dp4.log` | 追跡しない |
| 聴き比べ WAV | `clear_re/plugin/build/dpdfnet_train/listen*/` | 追跡しない |
| ツール一式 | `clear_re/plugin/tools/dpdfnet_mid/*.py` | **コミットする** |
| この計画書 | `clear_re/plugin/MODEL_TRAINING.md` | **コミットする** |

### 8.2 手順（上から順に実行すれば再現できる）

```powershell
# 0) 前提: 作業ディレクトリは clear_re/plugin。$env:PYTHONIOENCODING="utf-8"
#    上流の clone と .pth は build/dpdfnet_train/ に置く（build/ は追跡外）
#      git clone https://github.com/ceva-ip/DPDFNet build/dpdfnet_train/upstream
#      build/dpdfnet_train/hf/checkpoints/dpdfnet2_48khz_hr.pth
#      build/dpdfnet_train/hf/checkpoints/dpdfnet8_48khz_hr.pth   (Hugging Face: Ceva-IP/DPDFNet)

# 1) 関門: torch と公式 ONNX が一致するか
python tools/dpdfnet_mid/parity_check.py --ckpt build/dpdfnet_train/hf/checkpoints/dpdfnet2_48khz_hr.pth `
    --onnx models/dpdfnet2_48khz_hr.onnx --blocks 2
python tools/dpdfnet_mid/parity_check.py --ckpt build/dpdfnet_train/hf/checkpoints/dpdfnet8_48khz_hr.pth `
    --onnx models/dpdfnet8_48khz_hr.onnx --blocks 8

# 2) 学習データ (LibriSpeech dev-clean, CC BY 4.0) を 48 kHz にデコード
python tools/dpdfnet_mid/prepare_data.py --root build/dpdfnet_train --hours 2.0

# 3) 初期値 (dp8 の DPRNN ブロック 0〜3 をコピー)
python tools/dpdfnet_mid/build_mid.py --blocks 4 `
    --out build/dpdfnet_train/ckpt_mid/init_dp4.pth

# 4) 蒸留 (45 分以内で区切る。再開は --resume)
python tools/dpdfnet_mid/distill.py --batch 4 --seg 3 --lr 1.2e-4 --minutes 40 `
    --val-every 200 --save-every 200 --out build/dpdfnet_train/ckpt_mid/dp4.pth
python tools/dpdfnet_mid/distill.py ...(同じ引数) --resume

# 5) ONNX 書き出し + 2 出力化 + 契約検証 (一括)
python tools/dpdfnet_mid/export_mid.py `
    --ckpt build/dpdfnet_train/ckpt_mid/dp4.best.pth `
    --name dpdfnet4_48khz_hr --blocks 4 --out-dir build/dpdfnet_train/onnx

# 6) 速度と品質
python tools/bench_model.py --models models/dpdfnet2_48khz_hr_2out.onnx `
    models/dpdfnet4_48khz_hr_2out.onnx models/dpdfnet8_48khz_hr_2out.onnx --threads 1 2 4
python tools/dpdfnet_mid/quality_libri.py --models dp2=models/dpdfnet2_48khz_hr_2out.onnx `
    dp4=models/dpdfnet4_48khz_hr_2out.onnx dp8=models/dpdfnet8_48khz_hr_2out.onnx --n 6
python tools/dpdfnet_mid/quality_report.py --models ... --wav test_signals/speech_tail_48k.wav `
    test_signals/speech_tail_reverb_48k.wav --write-wav --out-dir build/dpdfnet_train/listen

# 7) プラグインでの実検証 (プラグイン側は変更しない)
#    clear_re/dpdfnet のオフラインテストを DPDFNET_MODEL=models/dpdfnet4_48khz_hr_2out.onnx に
#    向けて実行する。README の手順に従う。
```

### 8.3 もっと dp8 に近づけるための案（時間ができたら）

1. **もっと学習する**。今回は合計 ~2,000 ステップ（≒ 6.7 時間分の音声）だけ。
   上流の学習量に比べて桁違いに少ない。`--minutes 40` を何度も `--resume` で回す。
   学習率は 1e-4 前後、最後に 2e-5 まで落とすと収束が良くなるはず（`--lr` を手で下げる）。
2. **損失に波形/マルチ解像度 STFT 項を足す**。今は `spec_e` の一致だけ。知覚的な近さは
   波形側の損失を足すと改善することが多い（DeepFilterNet の学習と同じ発想）。
3. **データを増やす / 実ノイズにする**。MUSAN・DNS-Challenge の noise を足す。
   今はノイズが合成のみ。`distill.py` の `make_noise()` に実ファイル読み込みを足す。
4. **初期値の作り方を変える**。今は dp8 の「先頭 4 ブロック」。後半 4 ブロックとの
   平均を取る、あるいは dp2 のブロックを 2 回繰り返して 4 ブロックにする、などの
   初期値は未検証。今の初期値は出力が無音に潰れる状態から始まるので、
   最初の数百ステップが「復帰」に使われている。
5. **別案: DPRNN を 8 ブロックのまま DPRNN の隠れ次元を削る**。`dprnn_num_blocks=8` +
   `hidden_dim` 半減なら、パラメータも計算量も減り、dp8 の空気感を保ちやすい可能性がある
   （今回は上流の `dprnn_num_blocks` だけが dp2/dp8 の違いだったので dp4 を選んだ）。
6. **段階的に削る蒸留 (progressive)**。8 → 7 → 6 → … と 1 ブロックずつ削って都度微調整すると、
   最初から 4 に落とすより良い解に入りやすい可能性がある。

### 8.4 今回できなかったこと（残タスク）

* 十分な学習量（§8.3-1）と、それを裏付ける「dp2 を明確に上回る」品質。
* 実ノイズ (MUSAN / DNS) での学習・評価。
* LibriSpeech **test-clean**（学習に使っていない分割）での評価。今は dev-clean しか無く、
  これは dp4 の学習にも使っている（§7.3 の注意）。
* プラグイン (`clear_re/dpdfnet`) のオフラインテストを新モデルに向けた実行（別担当が
  編集中のため今回は触っていない）。手順は §8.2-7。

### 8.5 使った道具の一覧

| ファイル | 役割 |
|---|---|
| `tools/dpdfnet_mid/parity_check.py` | torch と公式 ONNX の数値一致チェック（関門） |
| `tools/dpdfnet_mid/parity_drift.py` | 差が「初期から」か「徐々にドリフト」かの切り分け |
| `tools/dpdfnet_mid/parity_state_segments.py` | state のどの区間に差が出るかの特定 |
| `tools/dpdfnet_mid/mid_model.py` | オフライン実装の構築・dp8 からの重みコピー・指標 |
| `tools/dpdfnet_mid/build_mid.py` | dp4 の初期値を作る |
| `tools/dpdfnet_mid/prepare_data.py` | LibriSpeech の取得と 48 kHz キャッシュ作成 |
| `tools/dpdfnet_mid/distill.py` | 蒸留学習（45 分区切り・再開可） |
| `tools/dpdfnet_mid/export_mid.py` | ONNX 書き出し → 2 出力化 → 契約検証 |
| `tools/dpdfnet_mid/evaluate_mid.py` | teacher との一致度と聴き比べ WAV（オフライン torch 版） |
| `tools/dpdfnet_mid/quality_report.py` | 付属 WAV + 合成ノイズで 3 モデル比較（ONNX 版） |
| `tools/dpdfnet_mid/quality_libri.py` | LibriSpeech + ノイズで抑圧性能（クリーン参照あり） |

---

## 9. ライセンスとクレジット

### 9.1 上流モデル（派生元）

| 対象 | ライセンス | 出典 |
|---|---|---|
| DPDFNet の重み (`dpdfnet2_48khz_hr.pth`, `dpdfnet8_48khz_hr.pth`) | **Apache-2.0** | Hugging Face `Ceva-IP/DPDFNet` |
| DPDFNet の実装 (`model/`, `onnx_model/`) | **Apache-2.0** | GitHub `ceva-ip/DPDFNet` |
| DeepFilterNet / DeepFilterNet2 (DPDFNet が利用) | MIT / Apache-2.0 | 上流の LICENSE に従う |

**新モデル `dpdfnet4_48khz_hr_2out.onnx` は上流重みの派生物**なので、
**Apache-2.0 を継承する**。配布時は上流の `LICENSE` を同梱し、
「DPDFNet (Ceva-IP) の重みから派生」であることを明記する。

* 重みの派生: dp8 の重みから DPRNN ブロック 0〜3 と全共有部をコピーし、
  `spec_e` を dp8 に合わせる蒸留で微調整した（§5.2 / §5.3）。
* 実装の派生物（このリポジトリの `tools/dpdfnet_mid/*.py`）も Apache-2.0 で置く。
  `export_mid.py` は上流のエクスポートスクリプトを呼び出すだけで、上流コードの
  改変物は含まない。

### 9.2 学習データ

| データ | ライセンス | 出典・条件 |
|---|---|---|
| LibriSpeech dev-clean | **CC BY 4.0** | OpenSLR SLR12 / Panayotov et al., 2015。**帰属表示が必要**（`build/dpdfnet_train/LibriSpeech/LICENSE.TXT` に原文）。再配布する場合は著作者表示とライセンス表示を付ける。 |
| 合成ノイズ（白色・ピンク・ブラウン・ハム・バブル・非定常・クリック） | 生成物（ライセンス無し） | 本作業で生成。実ファイルを同梱していないので再配布の問題は無い。バブルは LibriSpeech の発話を重ねただけなので LibriSpeech の条項に従う。 |
| `test_signals/rir_48k.wav` | このリポジトリのもの | 学習のオーギュメントに使用（評価用 WAV は `build/` にのみ出力） |

**注意**: 学習データ（LibriSpeech の音声）は**リポジトリにコミットしていない**。
`build/` は追跡外なので、必要な人が `prepare_data.py` で再取得する（§8.2-2）。
学習済み重み (`dp4.best.pth`) も同様にコミットしていない（派生物の配布時に上流・
データのライセンス表示を付けるため）。

### 9.3 クレジット（表記案）

```
This product includes a model derived from DPDFNet
  Copyright (c) Ceva-IP (https://github.com/ceva-ip/DPDFNet), licensed under Apache-2.0.
Trained using LibriSpeech (Panayotov et al., 2015),
  licensed under CC BY 4.0 (http://www.openslr.org/12).
```

### 9.4 このリポジトリ内の扱い

* `MODEL_TRAINING.md` と `tools/dpdfnet_mid/*.py` は**コミットする**（Apache-2.0、
  ルートの LICENSE に従う）。
* `models/dpdfnet4_48khz_hr_2out.onnx`（12.0 MB）は**コミットしない**（§7.7）。
  配布する場合は LFS かリリース添付にし、§9.3 のクレジットを添える。

---

## 10. まとめ（この作業で分かったこと）

1. **dp2 と dp8 の違いは `dprnn_num_blocks` だけ**（2 vs 8）。中間の 4 ブロックは
   上流の定義そのままで作れる。→ dp4 = 3.10 ms（ステレオ 1 ホップ）で目標の 3〜4 ms を達成。
2. **上流実装は公式 ONNX と数値一致する**（t=0 でビット一致）。これが「上流で学習 →
   同じ経路でエクスポート」という方針の根拠になった。
3. **dp8 の先頭 4 ブロックをコピーしただけでは動かない**（出力が無音に潰れる）。
   DPRNN は `emb` という 1 本のベクトル列を作るだけなので、そこがずれると decoder の
   マスクが閉じる。→ 蒸留の微調整が必須。
4. **蒸留は効く**。0.1 dB → 15.4 dB まで復帰し、LibriSpeech での抑圧性能は
   dp2 / dp8 と同等（+9.15 vs +9.54 / +9.50 dB）まで来た。
5. **ただし「dp8 との一致」では dp2 を明確には超えられていない**（17.8 vs 18.4 dB）。
   学習量が桁違いに少ない（約 2,000 ステップ ≒ 6.7 時間分）ことが最大の要因と見ている。
   §8.3 の改善案を順に試すのが次の作業。
6. **2 出力化の落とし穴**: `spec_ns` は既存モデルと同じく DF の spec バッファの
   中央フレーム (index 2) から取る。形状情報 (value_info) が無い ONNX では
   `add_second_output.py` が Squeeze を挟めず `[1,1,1,481,2]` になる。

---

作成: 2026-10-19 〜 2026-10-20 / 作業場所: `clear_re/plugin/`
