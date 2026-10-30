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
| 5 | 学習データの用意 | LibriSpeech dev-clean（学習）+ test-clean（評価）+ **MUSAN noise**（実ノイズ、CC BY 4.0）を 48 kHz にデコード（§6） |
| 6 | 学習（dp8 からの蒸留で微調整） | **実施**（§5.3 / §7.9）。合成ノイズのみで ~2,200 ステップ → 実ノイズを 6 割混ぜて追加 **4,955 ステップ**。teacher との一致 0.1 → **17.8 dB** |
| 7 | ONNX 書き出し + 2 出力化 + 契約検証 | **完了**（§5.4 / §7.1）。`models/dpdfnet4_48khz_hr_2out.onnx`（再書き出し済み・PASS） |
| 8 | 速度・品質の実測 | **完了**（§7.2〜§7.5, §7.10, §7.11）。速度 **2.62 ms**（目標達成）。品質は最終版 `dp4q` で SI-SDR **3.89 dB**（§7.11） |
| 9 | 知覚寄りの損失での微調整 | **成功**（§7.11）。`dp4q` は同じ 32 条件で SI-SDR **3.89 dB**（dp2 比 +1.83 / dp8 比 +1.74）、PESQ **2.59**（dp4 比 +0.18）。速度・サイズは dp4 と同一。**v0.3.2 で配布物に反映済み** |

**現状**: `models/dpdfnet4_48khz_hr_2out.onnx`（12,044,747 B、v0.3.2 でコミット済み）が
配布物の既定モデル。ステレオ 1 ホップ **2.62 ms**（目標 3〜4 ms を達成 = dp2 の 1.5 倍、
dp8 の 55 %）。実ノイズ（MUSAN eval split）+ 未学習音声（LibriSpeech test-clean）の
32 条件平均で **SI-SDR 3.89 dB / STOI 0.944 / PESQ-WB 2.59**（§7.11）。
実ノイズを学習に足す前の合成ノイズ評価は SI-SDR 4.19 / STOI 0.916 / PESQ 2.10 だったので、
**分布のミスマッチはほぼ解消した**。

**残タスク**: 配布時のライセンス表記は対応済み（`models/LICENSE-DPDFNet.txt` と
`models/NOTICE-DPDFNet.txt` を追加し、配布 `README.txt` とトップの README から参照）。
品質をさらに上げるなら §8.3。

**追記 (2026-10-25): 知覚寄りの損失で dp4 を上回る版ができた**（§7.11）。同じ 32 条件で
`dp4q` = **SI-SDR 3.89 dB / STOI 0.944 / PESQ 2.59**。dp2 比 SI-SDR **+1.83 dB**、
dp4 比 PESQ **+0.18**、教師 dp8 比でも SI-SDR **+1.74 dB**。速度・サイズは dp4 と同一。
**v0.3.2 で `models/` の dp4 を `dp4q` に差し替え、配布物の既定に反映済み**。

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

### 6.1 取得したデータ（2026-10-20 更新: 実ノイズを追加）

| データ | 用途 | サイズ | ライセンス |
|---|---|---|---|
| LibriSpeech dev-clean (OpenSLR SLR12, `dev-clean.tar.gz`) | クリーン音声（**学習用**） | 338 MB / 2.00 時間 / 1016 発話 | **CC BY 4.0** (Panayotov et al. 2015) |
| LibriSpeech **test-clean** (`test-clean.tar.gz`) | クリーン音声（**評価専用**。学習には一切使わない） | 346 MB 中 0.70 時間 / 287 発話を切り出し | **CC BY 4.0** |
| **MUSAN** の noise 部分 (OpenSLR SLR17) | 実ノイズ（学習 80 % / 評価 20 %） | 585 MB（noise のみ）/ 5.07 時間 / 757 ファイル → raw 3.51 GB | **CC BY 4.0** (Snyder et al. 2015) |
| `test_signals/rir_48k.wav` | 残響オーギュメント | 既存 | リポジトリ内 |
| 合成 RIR（指数減衰 + 初期反射 1〜4 本） | 残響オーギュメント | 生成 | — |

デコード済みキャッシュ（`build/dpdfnet_train/`、いずれも `.gitignore` 済み）:

| ファイル | 中身 |
|---|---|
| `speech_48000.f32` + `.json` | dev-clean 1016 発話 / 2.00 h（学習） |
| `test_speech_48000.f32` + `.json` | test-clean 287 発話 / 0.70 h（評価。**学習に未使用**） |
| `noise_48000.f32` + `.json` | MUSAN noise 757 ファイル / 5.07 h（`names` / `lengths` / `eval_indices` 付き） |

### 6.2 なぜ MUSAN にしたか（候補の比較）

| 候補 | サイズ | ライセンス | 判断 |
|---|---|---|---|
| **MUSAN noise**（採用） | **585 MB**（noise 部分だけ取得） | **CC BY 4.0** | 採用。6 種類の環境音・技術音を含み、プラグイン (Apache-2.0) に派生を入れやすい |
| ESC-50 | 約 600 MB | **CC BY-NC 3.0（非商用）** | 見送り。製品に載せる可能性があるため NC は避けた（ESC-10 だけなら CC BY） |
| MUSAN 全体 | 11 GB | CC BY 4.0 | 見送り。music/speech は不要で、回線が細い（実測 0.3〜0.5 MB/s） |
| DNS-Challenge noise | 数十 GB | 独自（CC BY 4.0 が多い） | 見送り。サイズと時間が厳しい |
| DEMAND | 数 GB | CC BY-SA 4.0 | 見送り。16 kHz のみで今回の MUSAN と同条件になるため優先度低 |

**入手元**: MUSAN 本体は <https://www.openslr.org/17/>。今回は Hugging Face のミラー
`FluidInference/musan` の `noise/**` だけを取得した（`prepare_noise.py --download`、
`huggingface_hub.snapshot_download`）。元コーパスのライセンスは CC BY 4.0 で、
`noise/free-sound/` と `noise/sound-bible/` には**クリップ単位の帰属表示**
（Attribution 3.0 / Public Domain）が同梱されている。ミラーを使ったのは
11 GB の tar を丸ごと落とさずに noise 部分（585 MB）だけ取るため。

**既知の制約（正直に）**: MUSAN の wav は **16 kHz** なので、48 kHz へアップサンプリング
すると **8 kHz より上に成分がない**。実際の環境音は 8〜20 kHz にも成分を持つため、
高域のノイズ（サー音・ヒス・風切り）への耐性はこのデータでは検証できない。
評価側でも同じ制約があるので、**dp2 / dp4 / dp8 の相対比較には影響しない**が、
絶対値は実環境より甘めに出る可能性がある。

### 6.3 ノイズの合成と実ノイズの混ぜ方

`distill.py` は 1 サンプルごとに

1. 確率 `--real-noise-p`（既定 0.6）で **MUSAN の train split** からランダム窓を取る
2. それ以外は従来の合成ノイズ（白色 / ピンク / ブラウン / ハム / バブル / 街頭風 / クリック）
3. どちらも RMS 1 に正規化してから、指定 SNR（-5〜15 dB）でクリーン音声に混ぜる

**評価用の分離**: `noise_48000.json` の `eval_indices`（ファイル名の sha1 で決まる 20 %、
163 ファイル / 61 分）は学習で**一度も使わない**。学習は train split（594 ファイル）のみ。
評価用クリーン音声も **test-clean**（dev-clean とは別コーパス）にしてあるので、
§7.10 の比較は「dp4 が学習で見ていない音声 + ノイズ」で行われている。

`prepare_data.py` はダウンロード・展開・48 kHz デコード（mono / DC 除去 / ピーク 0.9）まで
行い、1 本の raw float32 と index (`.json`) を `build/dpdfnet_train/` に作る。
`prepare_noise.py` も同じ形式（`--download` で取得、`--hours` で上限）。

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
| `check_onnx_model.py` | `[LEVEL] rms in=0.0402 out=0.0257 (-3.86 dB)`、`[ALIGN] content lag = 0` | OK（再書き出し後に再確認） |
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
| dp2 2out | **1.69 ms** | — | — | 17 % |
| **dp4 2out (新)** | **2.99 ms** | 2.84 ms | 2.91 ms | **30 %** |
| dp8 2out | 6.64 ms | 7.00 ms | 6.21 ms | 66 % |

**再測定（2026-10-20、実ノイズ学習後のモデル。他プロセスが止まった状態）**:

| モデル | 1 スレッド | 2 スレッド | 4 スレッド | 予算 (10 ms) 比 |
|---|---|---|---|---|
| dp2 2out | **1.66 ms** | 1.71 ms | 1.58 ms | 17 % |
| **dp4 2out (新)** | **2.69 ms** | 2.66 ms | 2.55 ms | **27 %** |
| dp8 2out | 4.69 ms | 4.64 ms | 4.38 ms | 47 % |

→ **目標の 3〜4 ms を達成**（dp4 = 2.69 ms。dp2 の約 1.6 倍、dp8 の約 57 %）。
同じ ONNX グラフなので、学習内容が変わっても速度は変わらない（差は測定条件のみ）。

* 測定は同じ機械でも実行ごとに ±15 % 程度ばらつく（背景で学習を回しているときは
  遅めに出る）。1 回目の測定では dp2 1.77 / dp4 3.10 / dp8 5.45 ms だった。
  桁関係（dp2 < dp4 < dp8、dp4 ≒ dp2 の 1.7〜1.8 倍）は毎回同じ。
* ONNX Runtime のセッション設定はプラグインと同じ（intra_op=スレッド数、inter_op=1、
  実行モード=SEQUENTIAL、最適化=ALL）。

### 7.3 品質 (1) LibriSpeech dev-clean + 合成ノイズ（クリーン参照あり）

`tools/dpdfnet_mid/quality_libri.py` / 6 発話 × {白色, バブル} × {0 dB, 10 dB} の平均:

| モデル | 出力 SNR vs クリーン | dp8 との一致 |
|---|---|---|
| dp2 | **+9.54 dB** | 18.35 dB |
| **dp4 (新・更新前)** | **+9.25 dB** | **18.93 dB** |
| dp8 | +9.50 dB | — (正解) |

**再測定（実ノイズ学習後、同じ 24 条件）**:

| モデル | 出力 SNR vs クリーン | dp8 との一致 |
|---|---|---|
| dp2 | +9.54 dB | 18.35 dB |
| **dp4 (新・更新後)** | +8.96 dB | **19.49 dB** |
| dp8 | +9.50 dB | — (正解) |

* 実ノイズを混ぜたことで、この**合成ノイズ条件では**出力 SNR が 0.3 dB 下がり、
  dp8 との一致は 0.56 dB 上がった。合成ノイズに最適化されていた分が取れた形。

* 「出力 SNR vs クリーン」= ノイズ抑圧の性能そのもの。3 モデルともほぼ同等
  （この条件では ERB マスクが支配的で、DPRNN ブロック数の差は出にくい）。
* 「dp8 との一致」= dp8 とどれだけ同じ結果を出すか。dp4 は **dp2 (18.35) を上回る 18.93 dB**。
* **注意（正直に）**: LibriSpeech dev-clean は dp4 の蒸留に使ったので dp4 に有利に出うる。
  学習に使っていない音声での比較は次節。実際、次節では dp2 の方が良い。

### 7.4 品質 (2) プラグイン付属の音声（学習に未使用 = ホールドアウト）

`tools/dpdfnet_mid/quality_report.py` / `speech_tail_48k.wav` と
`speech_tail_reverb_48k.wav` に合成ノイズを混ぜた 16 条件の平均:

| モデル | dp8 との一致 |
|---|---|
| dp2 | **18.91 dB** |
| **dp4 (新・更新前)** | **16.68 dB** |
| dp8 | — |
| **dp4 (新・更新後)** | **18.55 dB** |

* 実ノイズ学習後は **16.68 → 18.55 dB**。dp2 (18.91) との差は **2.2 → 0.36 dB** に縮んだ。
* 更新前と更新後で同じ 2 ファイル・同じ条件（`quality_report.py` の既定）。
  更新後の値は「更新前のモデルを選んだ基準」と同じデータだが、選択の根拠には
  使っていない（§7.10.2 のモデル選択は distill の検証値 + 32 条件の知覚指標で行った）。

これら 2 つの WAV はプラグイン付属の自前録音で、**すでにノイズ/残響を含む**（下記の
透明性テストで dp8 でも入出力 SNR が 4 dB しかない ＝ クリーン参照には使えない）。
そのため「出力 SNR」は出さず、dp8 との一致度だけを載せている。

**§7.3 と §7.4 の食い違いは重要**: dp4 は LibriSpeech + 合成ノイズで蒸留したので、
その分布では dp2 を上回るが、プラグイン付属の「実録音（ノイズ・残響入り）」では
dp2 に 2.2 dB 劣る。**学習データの分布が効いている**ということ。
→ 実ノイズを学習に足した後の再測定では **18.55 dB（dp2 は 18.91）まで回復**した（上の表）。

### 7.5 品質 (3) クリーン音声の素通し（透明性）と抑圧量

| 指標 | dp2 | **dp4 (新)** | dp8 |
|---|---|---|---|
| クリーン音声の透明性 SNR(out,in)、4 発話平均（`tools/dpdfnet_mid/transparency.py`） | 16.22 dB | **14.09 dB** → **14.03 dB**（更新後） | 14.14 dB |
| `measure_effect.py` (speech_tail_reverb, 全体減衰 / Voice Reverb=0) | -5.16 dB | -9.23 dB → **-3.16 dB**（更新後） | -14.50 dB |
| 同 (Voice Reverb=1) | -5.86 dB | -12.33 dB → **-4.92 dB**（更新後） | -14.49 dB |
| `measure_effect.py` de-reverb の効き | -21.56 dB | -33.61 dB → **-17.88 dB**（更新後） | -23.94 dB |

* 透明性で **dp4 (14.03) は dp8 (14.14) とほぼ同じ**。更新前後でほぼ変化なし。
* **抑圧量は更新で大きく変わった**（重要な副作用）。更新前は dp8 寄り（-9.2 / -12.3 dB）だったのが、
  更新後は **dp2 寄り（-3.2 / -4.9 dB）** になった。実ノイズで蒸留すると
  「強く抑える」方向の解から「dp2 と同じくらい控えめ」な解に移ったため。
  知覚指標が dp2 とほぼ同じになったのは、この挙動の変化と整合する（§7.10.3）。
* 透明性の 4 発話の内訳: dp2 [24.36, 25.80, 13.60, 1.13] /
  dp4 [23.41, 18.24, 13.87, 0.86] / dp8 [21.21, 23.78, 11.64, -0.06]。
  utt413 はどのモデルでも 0 dB 前後（ほぼ無音に近い発話で、モデルが抑圧してしまう）。
* `measure_effect.py` の値はファイル依存の参考値。

### 7.6 数値一致の関門（再掲・§5.1 の要約）

torch (上流実装) と公式 ONNX は dp2 / dp8 ともに **t=0 でビット一致**、
16 秒流しても SNR 75〜80 dB（float32 の丸めのみ）。この関門が通っているので、
上流実装で学習 → 同じ経路で書き出した dp4 も、プラグインが期待する契約どおりに動く。

### 7.7 モデルの置き場所とプラグインでの使い方

* ファイル: **`clear_re/plugin/models/dpdfnet4_48khz_hr_2out.onnx`**（12,044,747 B）
  * 既存の `dpdfnet{2,8}_*` は**変更していない**（新規ファイルの追加のみ）。
  * 大きいバイナリだが、dp2 / dp8 と同じ扱いで**コミット済み**（v0.3.2）。
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
* **ただし既定にはしない方がよい**（§7.10.3）。知覚指標で dp2 と同等以上とは言えず、
  CPU は 1.6 倍かかる。優先リストに足す場合は「dp8 の代わりに選べる軽い選択肢」として
  dp2 より後ろに置くのが妥当。

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
| 1600 | 15.31 dB |
| 1800 | 15.34 dB |
| 2000 | 15.44 dB（lr 4e-5、複素 MSE の重み 0.25 → 3.0 に変更後） |
| 2200 | 15.30 dB |

* 学習は `spec_e` の蒸留で、**teacher との一致が上がる = dp8 の音に近づく**方向。
* 800 ステップ付近で一度 15 dB に乗り、そこからは**ほぼ横ばい（15.3〜15.4 dB）**。
  学習率 2.5e-4 では振動してむしろ悪化した（val 14.7 → 13.1）ので 8e-5 → 1.2e-4 → 4e-5 と下げた。
* 複素 MSE の重みを 3.0 に上げた最終区間で、この検証セットではほぼ同じ (15.44) だが、
  **LibriSpeech での dp8 一致が 17.79 → 18.93 dB に改善**した（§7.3）。検証セット 3 本より
  遥かに多くの条件で測った値なので、こちらを信じる。
* この検証セット（3 本）は条件が偏っていて、dp4 が苦手なノイズ種が 1 本入っている
  （内訳 [8.1, 21.7, 16.6] dB に対し dp2 は [11.9, 23.8, 17.6] dB）。
  1 本だけ 3.8 dB 差がある。
* 打ち切り時点で **teacher との一致 15.4 dB**（dp2 は同じ検証セットで 17.8 dB、
  ただし dp2 は「良質な音声処理器」として大規模に学習済みなので、この比較は dp2 が有利）。

**実ノイズを混ぜた追加学習（2026-10-20）**。ここからは検証セットも変わっている
（実ノイズ 6 割 + 初期反射入り RIR で作り直した固定 3 本）ので、**上の表とは数値を比較できない**。

| 実行 | ステップ | lr | 所要 | 検証 SNR（新しい検証セット） |
|---|---|---|---|---|
| 1（実ノイズ mix 開始） | 200 | 1e-4 | 0.7 分 | 14.42 dB |
| | 400 | 1e-4 | 1.6 分 | 15.21 dB |
| | 1,100 | 1e-4 | 5.6 分 | 15.30 dB |
| | 1,700 | 1e-4 | 9.4 分 | 16.23 dB |
| | 2,300 | 1e-4 | 13.1 分 | 16.37 dB |
| | **2,595** | 1e-4 | **40.0 分** | **16.66 dB**（この実行の best） |
| 2（--resume、lr 下げ） | 3,200 | 5e-5 | 11.6 分 | 16.92 dB |
| | 4,100 | 5e-5 | 20.0 分 | **17.68 dB** |
| | 4,900 | 5e-5 | 31.4 分 | 17.24 dB |
| | **4,955** | 5e-5 | **40.0 分** | **17.81 dB**（最終 best） |

* 1 回目は 40 分で 2,595 ステップ（1.1 step/s、batch 4 × 3 秒 = 12 秒分/ステップ）。
  2 回目は他のプロセスが止まって 2.1〜5.1 step/s まで上がった。
* **実ノイズを混ぜても蒸留の検証値は上がり続けた**（15.2 → 17.8 dB）。
  「分布を広げると教師との一致は下がる」とはならなかった。
* ただし**蒸留の検証値と知覚指標は完全には連動しない**（§7.10.2 のモデル選択の項）。
  検証値が 16.66 → 17.81 と上がっても、32 条件の SI-SDR は 1.74 → 1.44 と
  わずかに下がった（差は誤差の範囲）。**打ち切りは検証値の頭打ちではなく、
  知覚指標が動かなくなったことで判断した**（§8.3 参照）。
* 合計学習量: 旧 2,200 + 新 4,955 = 7,155 ステップ ≒ 23.8 時間分の音声
  （12 秒 × ステップ数）。上流の学習量よりはまだ桁違いに少ない。

**未達 / 改善余地**: 「dp8 に近い品質」という目標に対しては、
**LibriSpeech（蒸留に使った分布）では dp2 を上回り、プラグイン付属の実録音では 2.2 dB 劣る**。
学習データの分布がそのまま効いている。改善案は §8.3。

### 7.10 品質 (4) 知覚指標 × 実ノイズ（ホールドアウト）

`tools/dpdfnet_mid/perceptual_report.py`。**SNR 系だけでは dp2 と dp4 の優劣が決まらない**
（§7.3 はどれも +9.2〜+9.5 dB に飽和する）ため、知覚指標を足した。

* クリーン音声: **LibriSpeech test-clean**（dev-clean = 学習用とは別コーパス。未学習）
* ノイズ: **MUSAN の eval split**（`eval_indices`、学習では未使用）
* 条件: test-clean の長い 4 発話 × 8 秒 × SNR {-5, 0, +5, +10} dB × ランダムなノイズファイル = **16 条件**
* 指標: SI-SDR / STOI / PESQ-WB（16 kHz に落として測定）/ セグメンタル SNR / 出力 SNR / dp8 との一致

#### 7.10.1 更新前（旧 dp4 = 合成ノイズだけで蒸留したモデル）

| モデル | SI-SDR [dB] | STOI | PESQ-WB | segSNR [dB] | 出力 SNR [dB] | dp8 一致 [dB] |
|---|---|---|---|---|---|---|
| dp2 | **5.93** | **0.947** | **2.58** | **11.44** | **6.25** | 21.32 |
| dp4（旧） | 4.19 | 0.916 | 2.10 | 9.26 | 4.85 | 14.67 |
| dp8 | 5.84 | 0.948 | 2.58 | 11.45 | 6.18 | — |

SNR 別の SI-SDR [dB]:

| 入力 SNR | dp2 | dp4（旧） | dp8 |
|---|---|---|---|
| -5 dB | -0.17 | -1.17 | **1.43** |
| 0 dB | **6.56** | 4.71 | 5.51 |
| +5 dB | **8.58** | 6.79 | 8.27 |
| +10 dB | **8.73** | 6.42 | 8.14 |

**この時点の結論（重要）**: 実ノイズでは **dp4 は dp2 に明確に劣る**
（SI-SDR で 1.7 dB、STOI で 0.031、PESQ で 0.48 低い）。§7.4 の「dp8 一致で 2.2 dB 負け」と
同じ向きで、**§7.3 の「dp4 が dp2 を上回る」は合成ノイズ分布に固有の結果**だったと判断できる。
ユーザーの指摘（SNR だけでは決められない）は正しく、指標を変えたことで結論が反転した。

#### 7.10.2 更新後（実ノイズを混ぜて再蒸留したモデル）

同じ条件で測り直した。**更新前と条件を揃えた 16 条件**（4 発話）と、
**条件数を増やした 32 条件**（8 発話、統計的なばらつきを減らすため）の両方を載せる。

32 条件（test-clean 8 発話 × 8 秒 × SNR 4 水準）:

| モデル | SI-SDR [dB] | STOI | PESQ-WB | segSNR [dB] | 出力 SNR [dB] | dp8 一致 [dB] |
|---|---|---|---|---|---|---|
| dp2 | **2.06** | **0.944** | **2.49** | **11.28** | **2.34** | 21.24 |
| **dp4（更新後）** | 1.44 | 0.936 | 2.41 | 10.68 | 1.80 | 18.28 |
| dp8 | 2.15 | 0.948 | 2.53 | 11.38 | 2.43 | — |

16 条件（更新前の表と同じ発話・同じノイズ。**更新前 → 更新後**で比較）:

| モデル | SI-SDR [dB] | STOI | PESQ-WB | segSNR [dB] | 出力 SNR [dB] | dp8 一致 [dB] |
|---|---|---|---|---|---|---|
| dp2 | **5.93** | **0.947** | **2.58** | **11.44** | **6.25** | 21.32 |
| dp4（更新前） | 4.19 | 0.916 | 2.10 | 9.26 | 4.85 | 14.67 |
| **dp4（更新後）** | 4.83 | 0.939 | 2.49 | 10.74 | 5.24 | **17.59** |
| dp8 | 5.84 | 0.948 | 2.58 | 11.45 | 6.18 | — |

**更新による改善（16 条件、同じ入力）**: SI-SDR **+0.64 dB**、STOI **+0.023**、
PESQ **+0.39**、segSNR +1.48 dB、dp8 一致 +2.92 dB。
実ノイズを学習に足しただけで、**「dp2 との差」が SI-SDR 1.74 → 1.10 dB、
PESQ 0.48 → 0.09 まで縮んだ**（この 16 条件では）。

SNR 別 SI-SDR [dB]（32 条件）:

| 入力 SNR | dp2 | **dp4（更新後）** | dp8 |
|---|---|---|---|
| -5 dB | -2.11 | -2.32 | **-1.33** |
| 0 dB | **3.09** | 1.88 | 2.87 |
| +5 dB | **2.94** | 2.42 | 2.67 |
| +10 dB | **4.32** | 3.76 | **4.38** |

**対応のある差（32 条件、同じ入力に対する dp2 − dp4）**:

| 指標 | 平均差 | 標準誤差 | 中央値 | dp4 ≧ dp2 だった条件 |
|---|---|---|---|---|
| SI-SDR | **+0.62 dB** | 0.45 dB | +0.81 dB | 10 / 32 |
| STOI | +0.0079 | 0.0010 | +0.0065 | 1 / 32 |
| PESQ | +0.078 | 0.025 | +0.088 | 10 / 32 |

* **SI-SDR は 0.62 dB 差だが標準誤差 0.45 dB で、有意とは言えない**（条件ごとのばらつきが
  非常に大きい。1 発話の中でも -20 dB になる条件と +17 dB になる条件がある）。
* **STOI は 0.008 だけ dp2 が高い。差は小さいが、32 条件中 31 条件で dp2 が上**＝
  偶然ではなく一貫した差。ただし 0.9 前後の値を 0.008 動かしても聴感差はまず分からない。
* **PESQ は 0.078 差**（標準誤差 0.025）。やはり小さく、dp4 が上回る条件も 10/32 ある。

**モデル選択について（正直に）**: 学習の途中（2,595 ステップ、蒸留検証 16.66 dB）と
最後（4,955 ステップ、17.81 dB）の 2 つのチェックポイントは、32 条件で対応比較すると
SI-SDR −0.30 ± 0.24 dB / STOI +0.001 / PESQ −0.005 で**ほぼ同じ**（＝学習を延ばした効果は
頭打ち）。最終版を選んだ理由は **蒸留の検証値が高いこと**と
**プラグイン付属の実録音での dp8 一致がわずかに良いこと**（18.55 vs 18.33 dB）で、
知覚指標の良し悪しで選んだわけではない（評価データでの選択を避けるため）。

#### 7.10.3 結論: dp4 は dp2 と「同等以上」か

**「同等」とは言える。「以上」とは言えない。**

| 観点 | 判定 |
|---|---|
| SI-SDR（32 条件） | 差 0.62 ± 0.45 dB。**有意差なし**（同等） |
| STOI | 0.008 低い。**一貫した差だが聴感上は無視できる**（同等とみなせる） |
| PESQ-WB | 0.078 低い。**わずかに dp2 が上** |
| 実録音（プラグイン付属）での dp8 一致 | 18.55 vs dp2 18.91 dB。**0.36 dB 差まで縮んだ**（更新前は 2.2 dB 差） |
| 速度 | dp4 は dp2 の **1.6 倍**（2.69 vs 1.66 ms） |

* 更新前は「実ノイズで dp4 が明確に負け」（SI-SDR 1.74 dB、PESQ 0.48 差）だったのが、
  **実ノイズを学習に混ぜたことでほぼ互角**になった。**分布のミスマッチが主因**だったことは
  これではっきりした（§7.10.1 → §7.10.2）。
* ただし **dp4 が dp2 を上回る証拠は無い**。SI-SDR の平均は dp2 が上、STOI は一貫して
  わずかに dp2 が上、PESQ も dp2 が上。dp4 が勝っているのは「dp8 との波形一致」だけ。
* **コストは dp4 の方が高い**（CPU 1.6 倍、サイズ +15 %、state 67700 vs 56436）。
* **したがって既定を dp4 に切り替える積極的な理由は無い**。切り替えるとすれば
  「dp8 に近い挙動（de-reverb など）を、dp8 より軽い計算量で」という用途だが、
  今回の再学習で抑圧量が dp2 寄り（-3.2 dB、§7.5）になったため、その差別化も弱まった。
* 逆に言えば、**「dp8 の 57 % の CPU で dp2 と同等の音質」は達成**している。
  dp2 と dp8 の間に新しい選択肢を置く、という当初の目的は満たす。

#### 7.10.4 聴き比べ WAV の場所

| ディレクトリ | 中身 |
|---|---|
| `build/dpdfnet_train/listen_real/` | **最終版**。32 条件 × 5 本 = 160 ファイル。`u<発話>_<SNR>dB_{1_clean,2_noisy,3_dp2,4_dp4,5_dp8}.wav` |
| `build/dpdfnet_train/listen_real_before/` | 更新前のモデル。16 条件 × 5 本 = 80 ファイル（同じ命名） |
| `build/dpdfnet_train/listen/` | 付属 WAV + 合成ノイズ（`quality_report.py --write-wav`） |

ファイル名の例: `u130_+5dB_2_noisy.wav`（入力）/ `..._4_dp4.wav`（新モデル）。
同じ `u*_<SNR>dB` の 5 本を並べれば、入力 → dp2 → dp4 → dp8 の順に聴き比べられる。
いずれも `build/` の中なので**コミットしていない**（§8.1）。

#### 7.11 知覚寄りの損失での微調整（2026-10-25・**成功**）

§8.3-3 の案を実装して試した。従来の損失は**教師 (dp8) の `spec_e` との一致**だけ
だったので、**クリーン音声を参照**にした波形側の損失を足せるようにした。

* `distill.py` に `--mr-weight`（マルチ解像度 STFT: 対数振幅 L1 + スペクトル収束、
  窓長 512 / 1024 / 2048）と `--sisdr-weight`（SI-SDR）を追加。既定は 0 で従来と同一。
* `build_batch()` がクリーン波形も返すようにした（ノイズ・残響・レベルは入力と共通）。
* best チェックポイントの選択も `--select sisdr` で「クリーンとの SI-SDR」に変更できる
  （既定 `auto` = 波形損失があれば sisdr、無ければ従来の教師 SNR）。

初期値はどちらも `dp4b.best.pth`（従来の最終版）。重みを 10 倍変えた 2 本を回した。

| 実験 | lr | 重み (mr / sisdr) | ステップ | 教師との spec SNR | クリーンとの SI-SDR |
|---|---|---|---|---|---|
| **dp4q** | 3e-5 | 0.005 / 0.002 | 2,795（40 分） | 12.81 dB | **12.90 dB** |
| dp4p | 5e-5 | 0.05 / 0.01 | 1,260（16 分・中断） | 10.70 dB | 12.58 dB |
| dp4b（従来） | — | —（蒸留のみ） | 4,955 | **17.81 dB** | — |

**まず押さえるべき事実**: ごく小さな重み (0.005 / 0.002) でも、**教師との一致は 17.8 → 12.8 dB に落ちる**。
理由は損失の桁が揃っていないこと。蒸留損失は収束後 **2e-5** まで下がっており、
MR 損失 (3〜4) に 0.005 を掛けた項は **0.02** で 3 桁大きい。つまり
**「教師の模倣をやめてクリーン参照の学習に切り替わる」**。狙った「微調整」ではなく、
実質は「最後の 40 分だけ別の目的関数で学習し直した」に近い。それでも結果は良くなった（下）。

32 条件（§7.10.2 と同じ test-clean 8 発話 × 8 秒 × SNR 4 水準）:

| モデル | SI-SDR [dB] | STOI | PESQ-WB | segSNR [dB] | 出力 SNR [dB] | dp8 一致 [dB] |
|---|---|---|---|---|---|---|
| dp2 | 2.06 | 0.944 | 2.49 | 11.28 | 2.34 | 21.24 |
| dp4（従来） | 1.43 | 0.936 | 2.41 | 10.68 | 1.80 | 18.28 |
| dp4p（強め） | 4.02 | 0.941 | 2.48 | 10.57 | 3.18 | 12.96 |
| **dp4q（弱め）** | 3.89 | **0.944** | **2.59** | **12.53** | **3.92** | 14.33 |
| dp8 | 2.15 | 0.948 | 2.53 | 11.38 | 2.43 | — |

**対応のある差（32 条件、dp4q − 比較対象）**:

| 比較 | SI-SDR | STOI | PESQ | segSNR | 出力 SNR |
|---|---|---|---|---|---|
| vs dp2 | **+1.83 ± 0.41 dB**（26/32 勝ち） | +0.001 ± 0.002 | +0.105 ± 0.077 | +1.25 | +1.58 |
| vs dp4 | **+2.45 ± 0.57 dB**（24/32） | **+0.009 ± 0.002**（25/32） | **+0.182 ± 0.078** | +1.85 | +2.12 |
| vs dp8 | **+1.74 ± 0.59 dB**（23/32） | −0.004 ± 0.003 | +0.068 ± 0.086 | +1.15 | +1.49 |

* **SI-SDR は dp2 比 +1.83 dB（標準誤差 0.41）で、今回の比較では有意**。dp4 比では +2.45 dB。
  **教師である dp8 すら上回る**（+1.74 dB）。波形側の損失 (SI-SDR) を直接学習したのだから
  不思議ではないが、**教師を超えた**という意味では初めての結果。
* **PESQ も dp4 比 +0.18（標準誤差 0.078）で、こちらは 2.3σ 相当**。dp8 比でも +0.07 上。
* **STOI は dp2 と同等、dp8 にはわずかに負け**（−0.004、11/32 勝ち）。ただし dp4 比では +0.009。
* 出力 SNR は dp2 比 +1.6 dB / dp4 比 +2.1 dB（= 抑圧量が増えた）方向に出ている。
  抑圧を強めながら SI-SDR も上がっているので、「抑圧不足」でも「潰しすぎ」でもない。
* 速度とサイズは **dp4 と完全に同一**（4 ブロック、1.28 ms/frame @1 スレッド、2 出力 ONNX
  12,044,747 B）。損失の変え方は推論には一切影響しない。

**測定条件についての注意**: 同じ dp4p を「4 秒 × 32 条件」で測ると SI-SDR 0.97 dB
（dp4 2.87 dB より 1.9 dB 低い）と**逆の結論**になった。4 秒だとノイズ窓の引き方が変わり、
条件間のばらつきが大きすぎる。**比較は必ず同じ設定で**行うこと。上の 8 秒 × 32 条件では
dp2 / dp4 / dp8 の値が §7.10.2 の表と一致する（＝条件が揃っていることの確認になる）。

**残る留保**:
* 評価ノイズは MUSAN の eval split（学習と別ファイルだが同じコーパス）。実マイク録音や
  8 kHz より上の帯域は相変わらず未評価（§8.3-1, -2）。
* 検証値は 100 ステップで 12.75 dB、2,100 ステップで 12.90 dB とほぼ横ばい。
  **40 分で頭打ち**なので、さらに伸ばすなら学習データ側の改善が要る。
* 損失の桁が揃っていない問題（教師損失 2e-5 vs 波形損失 0.02）は未解決。今回は
  「結果的に良かった」だけで、重みの根拠は経験的。蒸留損失を正規化すれば
  「教師の模倣を保ったまま知覚指標を上げる」調整が可能になるはず（§8.4 に残した）。

---

## 8. 付録: 再現手順と、次にやること

### 8.1 生成物の置き場所

| もの | 場所 | git |
|---|---|---|
| **新モデル (2 出力 ONNX)** | `clear_re/plugin/models/dpdfnet4_48khz_hr_2out.onnx` (12,044,747 B) | **コミット済み**（dp2 / dp8 と同じ扱い。v0.3.2） |
| **新モデルのライセンス全文 (Apache-2.0)** | `clear_re/plugin/models/LICENSE-DPDFNet.txt` | **コミットする**（配布物にも同梱） |
| **新モデルの帰属表示 (改変内容 + 学習データ)** | `clear_re/plugin/models/NOTICE-DPDFNet.txt` | **コミットする**（配布物にも同梱） |
| 1 出力 ONNX（2 出力化の元） | `clear_re/plugin/build/dpdfnet_train/onnx/dpdfnet4_48khz_hr.onnx` | 追跡しない（`build/` は .gitignore） |
| 学習済み重み（最終） | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/dp4b.best.pth` | 追跡しない |
| 学習済み重み（知覚損失版・採用候補 = dp4q） | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/dp4q.best.pth` | 追跡しない |
| 学習済み重み（知覚損失・重み強めの比較用 = dp4p） | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/dp4p.best.pth` | 追跡しない |
| 知覚指標の生データ（dp4p / dp4q 比較） | `clear_re/plugin/build/dpdfnet_train/perceptual_report_dp4pq.json` | 追跡しない |
| 学習済み重み（実ノイズ導入前の旧版） | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/dp4.best.pth` | 追跡しない（比較用に残してある） |
| 初期値 (dp8 の先頭 4 ブロック) | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/init_dp4.pth` | 追跡しない |
| 学習ログ | `clear_re/plugin/build/dpdfnet_train/ckpt_mid/{dp4,dp4b}.log` | 追跡しない |
| 学習データのキャッシュ | `clear_re/plugin/build/dpdfnet_train/{speech,test_speech,noise}_48000.{f32,json}` | 追跡しない |
| 聴き比べ WAV（合成ノイズ） | `clear_re/plugin/build/dpdfnet_train/listen*/` | 追跡しない |
| **聴き比べ WAV（実ノイズ）** | `clear_re/plugin/build/dpdfnet_train/listen_real*/` | 追跡しない |
| 知覚指標の生データ | `clear_re/plugin/build/dpdfnet_train/perceptual_report.json` | 追跡しない |
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
# 2b) 評価用のクリーン音声 (test-clean) と実ノイズ (MUSAN noise, CC BY 4.0)
python tools/dpdfnet_mid/prepare_data.py --root build/dpdfnet_train --dataset test-clean --hours 0.7
python tools/dpdfnet_mid/prepare_noise.py --root build/dpdfnet_train --download

# 3) 初期値 (dp8 の DPRNN ブロック 0〜3 をコピー)
python tools/dpdfnet_mid/build_mid.py --blocks 4 `
    --out build/dpdfnet_train/ckpt_mid/init_dp4.pth

# 4) 蒸留 (45 分以内で区切る。再開は --resume)
#    実ノイズを 6 割混ぜる (--real-noise-p 0.6)。0 にすると従来の合成ノイズのみ。
python tools/dpdfnet_mid/distill.py --init build/dpdfnet_train/ckpt_mid/init_dp4.pth `
    --batch 4 --seg 3 --lr 1e-4 --ri-weight 3.0 --real-noise-p 0.6 --minutes 40 `
    --val-every 100 --save-every 100 --out build/dpdfnet_train/ckpt_mid/dp4b.pth
python tools/dpdfnet_mid/distill.py ...(同じ引数) --resume

# 5) ONNX 書き出し + 2 出力化 + 契約検証 (一括)
python tools/dpdfnet_mid/export_mid.py `
    --ckpt build/dpdfnet_train/ckpt_mid/dp4b.best.pth `
    --name dpdfnet4_48khz_hr --blocks 4 --out-dir build/dpdfnet_train/onnx

# 6) 速度と品質
python tools/bench_model.py --models models/dpdfnet2_48khz_hr_2out.onnx `
    models/dpdfnet4_48khz_hr_2out.onnx models/dpdfnet8_48khz_hr_2out.onnx --threads 1 2 4
python tools/dpdfnet_mid/quality_libri.py --models dp2=models/dpdfnet2_48khz_hr_2out.onnx `
    dp4=models/dpdfnet4_48khz_hr_2out.onnx dp8=models/dpdfnet8_48khz_hr_2out.onnx --n 6
python tools/dpdfnet_mid/quality_report.py --models ... --wav test_signals/speech_tail_48k.wav `
    test_signals/speech_tail_reverb_48k.wav --write-wav --out-dir build/dpdfnet_train/listen
# 6b) 知覚指標 (実ノイズ = MUSAN eval split、test-clean 音声)
python tools/dpdfnet_mid/perceptual_report.py `
    --models dp2=models/dpdfnet2_48khz_hr_2out.onnx dp4=models/dpdfnet4_48khz_hr_2out.onnx `
             dp8=models/dpdfnet8_48khz_hr_2out.onnx `
    --utts 4 --seconds 8 --snr -5 0 5 10 --write-wav --out-dir build/dpdfnet_train/listen_real

# 7) プラグインでの実検証 (プラグイン側は変更しない)
#    clear_re/dpdfnet のオフラインテストを DPDFNET_MODEL=models/dpdfnet4_48khz_hr_2out.onnx に
#    向けて実行する。README の手順に従う。
```

### 8.3 もっと dp8 に近づけるための案（優先順・2026-10-20 更新）

**今回やったこと（効果が確認できた）**: 実ノイズ (MUSAN) を学習に 6 割混ぜた。
→ 実ノイズでの SI-SDR が 4.19 → 4.83 dB（16 条件）、dp2 との差が 1.74 → 1.10 dB に縮んだ（§7.10.2）。
**分布のミスマッチが主因**という読みは当たっていた。

残っている改善案（上から効きそうな順）:

1. **実ノイズの帯域を 8 kHz より上まで広げる**。MUSAN は 16 kHz なので、48 kHz に
   アップサンプリングしても 8 kHz 以上に成分が無い。環境音の「サー」「ヒス」「風切り」は
   8〜20 kHz に多く、そこが今回の評価でも学習でも手薄。48 kHz の実ノイズ
   （DNS-Challenge の noise、FSD50K など CC BY のもの）を足す。
   ESC-50 は CC BY-NC なので避ける（§6.2）。
2. **実マイク録音を学習に入れる**。今の `build_batch()` は
   「クリーン音声 + 加算ノイズ + RIR」しか作らない。実マイクは
   (a) 非定常ノイズ、(b) 低域の唸り、(c) 部屋鳴り、(d) マイクの周波数特性 が
   まとめて乗るので、加算ノイズでは再現できない。手元にはプラグイン付属の 2 本しか
   無いので、自分で録るか、CC の実録音コーパスを探す必要がある。
3. ~~**損失を知覚指標に寄せる**~~ **→ §7.11 で実施・成功**。マルチ解像度 STFT 損失と
   SI-SDR 損失（クリーン参照）を足し、初期値 dp4b から 40 分だけ微調整した `dp4q` は
   SI-SDR が dp2 比 +1.83 dB / dp8 比 +1.74 dB、PESQ が dp4 比 +0.18。速度・サイズは dp4 と同一。
4. **入力レベルの幅を広げる**（今は基準 RMS ±4 dB）。
5. **初期値の作り方を変える**（dp8 の後半 4 ブロックとの平均、dp2 ブロックの反復など）。
   今は出力が無音に潰れる状態から始めるので、最初の数百ステップが復帰に使われる。
6. **段階的に削る蒸留 (progressive)**。8 → 7 → 6 → 5 → 4 と 1 ブロックずつ削って都度微調整。
7. **学習量**。今回は合計 7,155 ステップ ≒ 23.8 時間分。上流は桁違いに多い。
   ただし 2,595 → 4,955 ステップで知覚指標が動かなかったので、
   **量を増やすより 1〜3 の質の改善を先にやるべき**（今回の実測に基づく判断）。

### 8.4 今回できなかったこと（残タスク）

* **8 kHz より上の実ノイズ**（§8.3-1）。MUSAN が 16 kHz なので、高域のノイズ耐性は未検証。
* **実マイク録音の分布での学習**（§8.3-2）。今は「クリーン + 加算ノイズ + RIR」のみ。
* **損失のスケール正規化**（§7.11 の留保）。蒸留損失は収束後 2e-5、波形損失は 0.02 と
  桁が 3 つ違うため、重みをいくら下げても「教師の模倣」は保てない。今回は結果的に
  良かったが重みの根拠は経験的。蒸留損失を初期値で基準化する、あるいは最初から
  複合損失で学習し直すのが次の筋。
* 実 RIR の多様性（今回は `rir_48k.wav` 1 本 + 合成 3 本 + 初期反射）。

**対応済み**: 配布時のライセンス表記（`models/LICENSE-DPDFNet.txt` /
`models/NOTICE-DPDFNet.txt` を追加し、配布パッケージの `README.txt` から参照）、
プラグイン側のモデル一覧への追加と既定 dp4 への切り替え、dp4 の実機確認、
**知覚寄りの損失での微調整（§7.11。`dp4q` が SI-SDR +1.83 dB / PESQ +0.18 で dp4 を上回る）**、
**プラグイン (`clear_re/dpdfnet`) のオフラインテストを新モデルに向けた実行**
（`DPDFNET_MODEL` で dp4q を指定して 7 本すべて成功。遅延 2880 サンプル・mono/stereo 一致・
96 kHz 変換・10 ms 予算のいずれも維持）。

### 8.5 使った道具の一覧

| ファイル | 役割 |
|---|---|
| `tools/dpdfnet_mid/parity_check.py` | torch と公式 ONNX の数値一致チェック（関門） |
| `tools/dpdfnet_mid/parity_drift.py` | 差が「初期から」か「徐々にドリフト」かの切り分け |
| `tools/dpdfnet_mid/parity_state_segments.py` | state のどの区間に差が出るかの特定 |
| `tools/dpdfnet_mid/mid_model.py` | オフライン実装の構築・dp8 からの重みコピー・指標 |
| `tools/dpdfnet_mid/build_mid.py` | dp4 の初期値を作る |
| `tools/dpdfnet_mid/prepare_data.py` | LibriSpeech の取得と 48 kHz キャッシュ作成（`--dataset test-clean` で評価用） |
| `tools/dpdfnet_mid/prepare_noise.py` | MUSAN noise の取得・48 kHz 化・学習/評価 split |
| `tools/dpdfnet_mid/distill.py` | 蒸留学習（45 分区切り・再開可・実ノイズ mix・クリーン参照の波形損失） |
| `tools/dpdfnet_mid/export_mid.py` | ONNX 書き出し → 2 出力化 → 契約検証 |
| `tools/dpdfnet_mid/evaluate_mid.py` | teacher との一致度と聴き比べ WAV（オフライン torch 版） |
| `tools/dpdfnet_mid/quality_report.py` | 付属 WAV + 合成ノイズで 3 モデル比較（ONNX 版） |
| `tools/dpdfnet_mid/quality_libri.py` | LibriSpeech + ノイズで抑圧性能（クリーン参照あり） |
| `tools/dpdfnet_mid/transparency.py` | クリーン音声をどれだけ素通しするか（加工の強さ） |
| `tools/dpdfnet_mid/perceptual_report.py` | 実ノイズ (MUSAN eval) + test-clean で SI-SDR / STOI / PESQ / segSNR |

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
| LibriSpeech test-clean | **CC BY 4.0** | 同上。**評価にのみ使用**（学習には入れていない）。 |
| MUSAN noise subset | **CC BY 4.0** | OpenSLR SLR17 / Snyder, Chen, Povey, 2015。入手は HF ミラー `FluidInference/musan` の `noise/**`。コーパス全体が CC BY 4.0 で、**クリップ単位の帰属**が `noise/free-sound/` と `noise/sound-bible/` の LICENSE に列挙されている（Attribution 3.0 / Public Domain）。**帰属表示が必要**。 |
| 合成ノイズ（白色・ピンク・ブラウン・ハム・バブル・非定常・クリック） | 生成物（ライセンス無し） | 本作業で生成。実ファイルを同梱していないので再配布の問題は無い。バブルは LibriSpeech の発話を重ねただけなので LibriSpeech の条項に従う。 |
| 合成 RIR（指数減衰 + 初期反射） | 生成物（ライセンス無し） | 本作業で生成。 |
| `test_signals/rir_48k.wav` | このリポジトリのもの | 学習のオーギュメントに使用（評価用 WAV は `build/` にのみ出力） |

**使わなかったデータと理由**: ESC-50（約 600 MB、環境音 50 クラス）は **CC BY-NC 3.0（非商用）**
なので、製品に載せる可能性がある今回は避けた。ESC-10 だけなら CC BY だが、
クラス数が少なく MUSAN で足りる。DEMAND は 16 kHz のみで MUSAN と同条件になるため優先度が低い。
DNS-Challenge のノイズは数十 GB で、今回の回線（実測 0.3〜0.5 MB/s）では現実的でない。

**注意**: 学習データ（LibriSpeech の音声・MUSAN のノイズ）は**リポジトリにコミットしていない**。
`build/` は追跡外なので、必要な人が `prepare_data.py` / `prepare_noise.py` で再取得する（§8.2）。
学習済み重み (`dp4.best.pth`) も同様にコミットしていない（派生物の配布時に上流・
データのライセンス表示を付けるため）。

### 9.3 クレジット（表記案）

```
This product includes a model derived from DPDFNet
  Copyright (c) Ceva-IP (https://github.com/ceva-ip/DPDFNet), licensed under Apache-2.0.
Trained using LibriSpeech (Panayotov et al., 2015),
  licensed under CC BY 4.0 (http://www.openslr.org/12),
and MUSAN (Snyder, Chen, and Povey, 2015),
  licensed under CC BY 4.0 (http://www.openslr.org/17/).
MUSAN includes clips from freesound.org and soundbible.com;
  per-clip attributions are listed in the MUSAN distribution.
```

### 9.4 このリポジトリ内の扱い

* `MODEL_TRAINING.md` と `tools/dpdfnet_mid/*.py` は**コミットする**（Apache-2.0、
  ルートの LICENSE に従う）。
* `models/dpdfnet4_48khz_hr_2out.onnx`（12.0 MB）は**コミットしない**（§7.7）。
  配布する場合は LFS かリリース添付にし、§9.3 のクレジットを添える。

---

## 10. まとめ（この作業で分かったこと）

1. **dp2 と dp8 の違いは `dprnn_num_blocks` だけ**（2 vs 8）。中間の 4 ブロックは
   上流の定義そのままで作れる。→ dp4 = **2.69 ms**（ステレオ 1 ホップ）で目標の 3〜4 ms を達成。
2. **上流実装は公式 ONNX と数値一致する**（t=0 でビット一致）。これが「上流で学習 →
   同じ経路でエクスポート」という方針の根拠になった。
3. **dp8 の先頭 4 ブロックをコピーしただけでは動かない**（出力が無音に潰れる）。
   DPRNN は `emb` という 1 本のベクトル列を作るだけなので、そこがずれると decoder の
   マスクが閉じる。→ 蒸留の微調整が必須。
4. **蒸留は効く**。0.1 dB → 17.8 dB（teacher との一致）まで復帰。
5. **SNR 系の指標だけでは dp2 と dp4 の優劣を決められない**（§7.3 の出力 SNR は
   どれも +9.2〜+9.5 dB で飽和する）。**知覚指標（SI-SDR / STOI / PESQ）と、
   学習に使っていない実ノイズでの評価を足すと結論が変わる**（§7.10）。
   → 更新前の dp4 は実ノイズで dp2 に **SI-SDR 1.7 dB / PESQ 0.48 負け**ていた。
6. **原因は学習データの分布だった**。MUSAN の実ノイズを 6 割混ぜて 4,955 ステップ
   追加学習するだけで、**SI-SDR 4.19 → 4.83 dB、PESQ 2.10 → 2.49、dp8 一致 +2.9 dB**。
   dp2 との差は 1.74 → 1.10 dB（16 条件）、0.62 ± 0.45 dB（32 条件）まで縮んだ。
   **分布のミスマッチが主因**という読みは当たっていた。
7. **結論: dp4 は dp2 と「同等」だが「以上」ではない**（§7.10.3）。
   32 条件で SI-SDR 差 0.62 ± 0.45 dB（有意差なし）、STOI 0.008 低い、
   PESQ 0.078 低い。CPU は dp2 の 1.6 倍。**既定を dp4 に切り替える理由は薄い**。
8. **再学習で挙動が dp2 寄りになった**（重要な副作用）。抑圧量が -9.2 → -3.2 dB、
   de-reverb の効きが -33.6 → -17.9 dB（dp2 は -5.2 / -21.6、dp8 は -14.5 / -23.9）。
   「dp8 の性格を安く」という更新前の売りは弱くなった。
9. **2 出力化の落とし穴**: `spec_ns` は既存モデルと同じく DF の spec バッファの
   中央フレーム (index 2) から取る。形状情報 (value_info) が無い ONNX では
   `add_second_output.py` が Squeeze を挟めず `[1,1,1,481,2]` になる。
10. **学習量を増やしても知覚指標は動かなかった**（2,595 → 4,955 ステップで
    SI-SDR −0.30 ± 0.24 dB、PESQ −0.005）。次に効くのは量ではなく
    **帯域の広い実ノイズ・実マイク録音・知覚損失**（§8.3）。
11. **知覚損失（クリーン参照の MR-STFT + SI-SDR）は効いた**（§7.11）。蒸留済みの
    dp4b から 40 分だけ回した `dp4q` が、32 条件で **SI-SDR +1.83 dB（vs dp2）/
    +1.74 dB（vs dp8）**、**PESQ +0.18（vs dp4）**。STOI は dp2 と同等。
    推論コストは dp4 と完全に同じ。
12. **ただし教師との一致は 17.8 → 12.8 dB に落ちる**。蒸留損失が収束後 2e-5 まで
    下がっているのに対し、足した波形損失は 0.02 と 3 桁大きい。**「微調整」ではなく
    「目的関数の差し替え」に近い**状態で、重みの根拠はまだ経験的（§8.4）。

---

作成: 2026-10-19 〜 2026-10-25 / 作業場所: `clear_re/plugin/`
