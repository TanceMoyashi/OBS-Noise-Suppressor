# OBS 実機確認手順（OBS ネイティブ版）

対象: **このプラグイン単体**（VST3 + atkAudio は使いません）。OBS 32.2.2 で確認済み。

前提:

- OBS の設定 → 音声 → **サンプルレート**（48 kHz を推奨。それ以外でも内部で
  48 kHz へ変換して処理します。変換段を作れない異常値のときだけパススルー）
- `install.ps1` でインストール済み（`%ProgramData%\obs-studio\plugins\obs_noise_suppressor`）

---

## 0. 先に「期待値」を知っておく

聴感だけで判断すると「効いているのか分からない」となりがちなので、先に数字を
確定させます（VST3 版と同一の DSP なので期待値も同じ）。

```powershell
cd ..\plugin
python tools\make_test_signals.py
python tools\measure_effect.py --model models\dpdfnet8_48khz_hr_2out.onnx `
    --wav test_signals\speech_tail_reverb_48k.wav
```

このマシンでの実測（低活動区間 = 無音ギャップの減衰量。ノイズ抑圧 + 残響抑圧の総合効果）:

| モデル | 入力 | Voice Reverb = 0 | Voice Reverb = 1 | de-reverb の効き幅 |
| --- | --- | --- | --- | --- |
| dpdfnet2 | 残響つき | −28.3 dB | −49.9 dB | **−21.6 dB** |
| dpdfnet8 | 残響つき | −36.6 dB | −60.6 dB | **−23.9 dB** |
| dpdfnet2 | 残響なし（デジタル無音区間） | 出力 −84.4 dBFS | 出力 −70.1 dBFS | （減衰量は定義不可） |

- 全体レベル: dpdfnet2 −5.9 dB / dpdfnet8 −14.5 dB（入力比）
- 遅延: **2880 サンプル = 60 ms**
- CPU（offline, 1 フレーム = 10 ms 音声）: stereo で dpdfnet2 1.66 ms（RTF 0.17）/
  dpdfnet8 4.79 ms（RTF 0.48）

**残響つきで Voice Reverb を 0→1 にすると、尾が 20 dB 以上小さくなる**のが正常な
状態です。聴いてこれが分からなければ、どこかで止まっています。

---

## 1. 自動で回す（推奨）

```powershell
cd obs_plugin
.\tools\run_obs_verify.ps1
```

これ 1 本で次を全部やります。

1. 検証用のシーンコレクション / プロファイルを作る
   （`tools/make_verify_collection.py`。48 kHz / Stereo、メディアソース 1 つ +
   本フィルタ 1 つだけの最小構成）
2. （既定で）ビルドしてインストールする
3. 起動中の OBS を閉じる
4. `DPDFNET_DUMP` を設定して OBS を起動（入力と出力を WAV に落とす）
5. ダンプが十分な長さになるまで待つ（既定 25 秒）
6. 画面をスクリーンショット（パネルの見た目確認用）
7. OBS のログからプラグインの状態行を出す
8. OBS を閉じる（`destroy` が走って WAV ヘッダのサイズが確定する）
9. `user.ini` を元に戻し、検証用コレクションを片付ける
10. `cargo test --release --lib live_dump` で
    **「実機の出力 == 同じ入力をオフラインで回した出力」**と
    **「実機の遅延 = 2880」**を検証する

主なオプション:

| オプション | 意味 |
| --- | --- |
| `-SkipBuild` | ビルドしない |
| `-SkipInstall` | インストールしない（`-SkipBuild` と併用） |
| `-SkipTest` | 最後のテストを飛ばす（見た目だけ見たいとき） |
| `-Seconds 40` | 音を流す秒数を変える |
| `-NoPanel` | パネルを自動で開かない |
| `-KeepObsOpen` | 最後に OBS を閉じない（手で見たいとき） |
| `-KeepCollection` | 検証用コレクションを消さない |

> **昇格について**
> このマシンの `obs64.exe` には互換性設定 `RUNASADMIN` が付いており、OBS は
> 管理者権限で動作します。UIPI のため非昇格プロセスからは OBS を閉じられないので、
> このスクリプトは**自分自身を管理者権限で起動し直します**。
> UAC が「通知しない」設定（`ConsentPromptBehaviorAdmin = 0`）なら無言で昇格します。
> 昇格できなかった場合だけ、最後に手で OBS を閉じてください。

出力されるファイル（既定 `%LOCALAPPDATA%\Temp\opencode\live`）:

| ファイル | 内容 |
| --- | --- |
| `dump_in.wav` / `dump_out.wav` | フィルタ前後の音（32 bit float, OBS 内部と同じ形式） |
| `panel_live.png` | **パネル自身が書いた絵**（隠されていても取れる。メーターは実測値） |
| `obs_panel.png` | 全画面スクリーンショット（他のウィンドウが前に出ていると写らない） |
| `elevated.log` | 昇格側のログ（全文） |
| `user.ini.bak` | 検証前の `user.ini` の控え |

> `panel_live.png` は `DPDFNET_PANEL_SNAPSHOT` にパスを入れると、パネルが開いて
> 5 秒後（メーターが動いてから）に自分の DIB を 32 bit BMP で書き出したものです。

### 期待する出力（このマシンでの実測）

```text
[obs-noise-suppressor] module loaded (libobs api 0x20020002, obs_source_info 424 bytes, ort api 17)
[obs-noise-suppressor] onnxruntime 1.31.0 (...\plugins\obs_noise_suppressor\data\dpdfnet_onnxruntime.dll) を読み込みました
[obs-noise-suppressor] status: 48 kHz / ch=2 / latency 2880 samples (60 ms) / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2

実機 lag = 2880 samples (6.000 hop, 60.0 ms) corr = 0.8140 (in_rms 0.114889 out_rms 0.027569)
参照 vs 実機: 1248640 frames x 2 ch / max diff = 0e0 / diff rms = 0e0
test result: ok. 1 passed; 0 failed
```

`max diff = 0e0` = **OBS の実出力がオフライン参照とビット一致**しています。
つまり OBS 経路（チャンネル処理・ブロック分割・フォーマット変換）で一切壊して
いない、ということです。

---

## 2. 手で確認する

1. OBS に **メディアソース** を追加 → `plugin\test_signals\speech_tail_reverb_48k.wav`、
   **ループ ON**、ミュートでよい
2. そのソース → **フィルタ** → `+` → **OBS Noise Suppressor**
3. プロパティの **「パネルを開く（フェーダー / メーター）」** ボタンでパネルが開く
4. プロパティの説明欄（＝状態表示）を確認する

| 表示 | 意味 |
| --- | --- |
| `48 kHz / ch=2 / latency 2880 samples (60 ms) / <model> \| n_fft=… hop=… state=… outputs=2` | ✅ 正常。`outputs=2` なので Voice Reverb が有効 |
| `48 kHz / ch=1 / latency 2880 samples (60 ms) / …` | ✅ モノラル設定で正常 |
| `96 kHz / ch=2 / latency 2880 samples (60 ms) / …` | ✅ 48 kHz 以外でも正常（内部でレート変換。パネルには `+SRC 2.4 ms` が出ます） |
| `OBS の音声が 12345 Hz です。この値は変換できないためパススルーします` | 変換段を作れない異常値（通常は起きません。設定 → 音声） |
| `DPDFNet モデルが見つかりません (探索先: …)` | `data\models\*.onnx` の配置を確認 |
| `モデルの読み込みに失敗: …` | モデルが壊れている / ORT と不一致 |
| `onnxruntime を読み込めません: …` | 同梱 `dpdfnet_onnxruntime.dll` の配置を確認 |

パネルの 1 行表示は `2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり`
のように短く出ます（詳細はプロパティ側）。48 kHz 以外ではレートと
`+SRC x.x ms`（レート変換ぶんの遅延）が付きます。

5. メディアソースの音声メーターが動いていれば DSP が回っています
6. パネルの GR メーター（抑圧量）と 3 バンドメーターも反応します

### 聴いて確認

フィルタの **有効/無効**（OBS のフィルタ一覧のチェックボックス = パネルの BYPASS）を
切り替えて聞き比べます。

| 確認 | 期待 |
| --- | --- |
| `speech_tail_48k.wav`（ノイズのみ） | ノイズが消え、無音区間が静かになる |
| `speech_tail_reverb_48k.wav`（残響つき） | 残響が締まる。**Voice Reverb 0→1 で尾が明らかに消える** |
| **Ambience** 左→右 | 左で最大抑圧、右で入力に近づく（環境ノイズが戻る） |
| **Voice** −12→+12 | 高域の色づけと音量の変化 |
| AMBIENCE の **MUTE / BYP** | MUTE で環境ノイズが完全に消える（0 %）/ BYP で素の入力に近づく（100 %）。**つまみの値表示は暗くなるが位置は動かない**。同じキーをもう一度で有効へ、BYP を押すと MUTE が落ちる |
| VOICE の **MUTE / BYP** | MUTE で presence が最小（−12 dB）/ BYP で無色（0 dB）。**効きが切り替わって**聞こえる |
| REVERB の **MUTE / BYP** | MUTE で残響が消える（0 %）/ BYP で素の残響（100 %）。1 出力モデルではキーごと暗くて押せない |
| MUTE / BYP と**プリセット** | MUTE にした状態で SAVE → 値を変えて呼び出すと MUTE ごと戻る。OBS 再起動後も残る |
| **BYPASS** | 素通しに戻る。**遅延は維持されない**（切り替えると音の位置が 60 ms 飛ぶ） |
| **モデル** を dp8 ↔ dp2 | 数十 ms 無音になったあと、効きと CPU 負荷が変わる（dp2 が軽い） |
| **VU 基準値** を −20 / −18 / −14 | 針の位置と VU 目盛りが平行移動する（音は変わらない） |
| **メモリー** SAVE → 番号 | LED が点く。別の値にして番号を押すと戻る。OBS 再起動後も残る |
| **メモリー** Ctrl + 番号 | 消去され、LED が消える |
| フェーダー / VU の **dB と %** | 数字が実際の値と一致する（VU の窓 = 名前 + 現在値とピーク dBFS） |
| **IN / OUT の針** | 抑圧が掛かると OUT の針が IN より左に落ちる（ステレオは L / R の平均、モノラルはそのまま） |
| VU 窓を**ダブルクリック** | その窓のピークホールドが消える（IN / OUT どちらでも） |

**リアルタイム性の確認（重要）**:

- プツ切れ・クリック・ドロップアウトが出ないこと
- OBS の **表示 → 統計** で「ドロップしたフレーム」が増えないこと
- CPU 使用率が常識的な範囲に収まること（重ければ `data\models` から
  `dpdfnet8_*.onnx` をどけて dpdfnet2 に落ちる）

マイクで実際に使う場合は、マイクソースに同じフィルタを挿します（OBS の音声設定と
同じサンプルレートで動きます。48 kHz 以外なら内部で変換されます）。

---

## 3. 遅延を数値で確認する（自動ハーネスの中身）

`run_obs_verify.ps1` は環境変数を 2 つ使って実機の入出力を取ります。手で回す場合:

```powershell
$env:DPDFNET_DUMP = 'C:\tmp\live\dump'          # プレフィックス（_in / _out が付く）
$env:DPDFNET_OPEN_PANEL_ON_START = '1'          # 起動時にパネルを自動で開く
& 'C:\Program Files\obs-studio\bin\64bit\obs64.exe' --multi `
    --collection dpdfnet-verify --profile dpdfnet-verify
```

`dump_in.wav` / `dump_out.wav` は**環境変数が無ければ一切作られない**ので、通常運用の
オーディオスレッドには影響しません（ロックすら取りません）。

OBS を終了すると `destroy` が走り、WAV の RIFF サイズが確定します
（強制終了した場合も、ファイル長からデータ長を決める読み方をするので解析は可能）。

```powershell
# 同じ入力をオフラインで回した結果と突き合わせる
$env:DPDFNET_LIVE_DUMP = 'C:\tmp\live\dump'
$env:DPDFNET_ORT   = 'D:\...\DPDFNet Voice.vst3\Contents\x86_64-win'
$env:DPDFNET_MODEL = 'D:\...\plugin\models\dpdfnet8_48khz_hr_2out.onnx'
.\build.ps1 -Tests -TestFilter live_dump -NoCapture
```

OBS のレートを変えて回すときはハーネスに `-SampleRate` を渡します（検証用
プロファイルの `SampleRate` に反映され、ダンプもそのレートで出ます）:

```powershell
.\tools\run_obs_verify.ps1 -SampleRate 96000
```

`live_dump_matches_reference` は**ダンプ WAV のサンプルレートを読んで**期待遅延を
エンジンの申告から計算するので、48 kHz でもそれ以外でも同じ手順で判定できます。

| 測定結果（48 kHz） | 判定 |
| --- | --- |
| `実機 lag = 2880 samples` | ✅ 設計どおり（`kLatencyHops = 6` × hop 480） |
| `max diff = 0e0` | ✅ 実機とオフライン参照がビット一致 |
| `max diff > 1e-4` | ❌ OBS 経路のどこか（チャンネル数 / ブロック / リセット）が怪しい |
| 遅延が 2880 でない | ❌ `src/dsp/engine.rs` の `K_LATENCY_HOPS` とホップ処理を確認 |

| 測定結果（48 kHz 以外。例: 96 kHz） | 判定 |
| --- | --- |
| `実機 lag = 5988 samples`（`2880@48k + SRC 2.38 ms`） | ✅ 期待値はエンジンの申告から自動計算して照合 |
| `max diff = 0e0` | ✅ レート変換込みでビット一致 |
| 入出力のフレーム数が不一致 / ゼロ埋めが続く | ❌ `src/dsp/engine.rs` の `Rack`（持ち越し）を確認 |

---

## 4. 困ったときの切り分け

| 症状 | 見るところ |
| --- | --- |
| 何が起きたか分からない | 専用ログ `%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\obs_noise_suppressor.log`（エラー / 警告 / 起動情報。OBS 標準ログと同内容） |
| フィルタ一覧に「OBS Noise Suppressor」が出ない | インストール先（`bin\64bit\obs_noise_suppressor.dll`）、OBS のログの `[obs-noise-suppressor] module loaded` |
| 音がまったく出ない | フィルタの有効/無効、ミュート、OBS のサンプルレート |
| 状態が `OBS の音声が … です。この値は変換できないためパススルーします` | 想定外のレート（変換比が 1024 超など）。OBS の設定 → 音声 → サンプルレート |
| ノイズが消えない | メーターが動いているか、`data\models` の配置、モデル名 |
| プツプツ切れる | OBS の統計（ドロップフレーム）、CPU。`dpdfnet8_*.onnx` をどけて dpdfnet2 に |
| 音が 60 ms ずれる | 仕様（§0 / §2）。フィルタ無しのソースと混ぜない |
| REVERB フェーダーがグレー | 1 出力モデルしか無い（`*_2out.onnx` を `data\models` に置く） |
| **フィルタを選んだ瞬間に OBS が落ちる** | 同梱 ORT の名前を確認。`onnxruntime.dll` という名前で置くと System32 の ORT 1.17 を掴んで即死する（現在は `dpdfnet_onnxruntime.dll`）。ログに `onnxruntime 1.31.0 (…plugins\obs_noise_suppressor\data\dpdfnet_onnxruntime.dll) を読み込みました` と出れば正常 |
| 状態が `onnxruntime を読み込めません` | 同梱 DLL の配置・サイズ。`DPDFNET_ORT` で明示指定も可 |
| パネルが開かない | OBS のログに `パネルを開けません:` が出ていないか。プロパティのボタンをもう一度押す |
| パネルが残ったまま OBS が終了しない | モジュールのアンロードで `ui::shutdown` が 2 秒待って畳む。それでも残る場合はログを確認 |

---

## 5. 参考: 実装側の確認済み事実

- `obs_module_ver()` は実行中の libobs と同じ API バージョン（`0x20020002`）を返す。
  PE エクスポートは `obs_module_load` / `obs_module_set_pointer` / `obs_module_ver`
  （＋ `obs_module_unload` / `obs_module_name` / `obs_module_description`）。
- ORT は **絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH`** で読む。名前だけの解決は
  System32 の ORT を掴むため禁止。`onnxruntime.dll` を同梱してはいけない。
- モデルメタデータ: `n_fft=960`, `hop_length=480`, `sample_rate=48000`,
  profile `dpdfnet8_48khz_hr_2out`（`freq_bins=481`, `state=90228`, 2 出力）
- OBS は `update` と `filter_audio` の相互排他をしない（音声専用ソースでは
  `filter_mutex` を取らない）。そのため `update` はアトミックなパラメータ書き込み
  だけを行い、ORT セッションの再構築はエンジン生成専用の経路で行う。
- libobs はフィルタの `activate` を「親ソースのアクティブ状態が変化した時」しか
  呼ばない（`libobs/obs-source.c`）。`obs_source_filter_add` 自体はアクティブ化
  しないため、**すでにアクティブなソースへ後からフィルタを追加したケースでは
  `activate` が来ない**。そのためエンジンは `create` / `update` / `activate` の
  いずれでも生成する（`create` 時点で生成していなかった版では、この経路で
  永久にパススルーになっていた）。
- エンジンの差し替え時は古いエンジンをその場で破棄せず「引退リスト」に移し、
  `destroy` でまとめて破棄する（オーディオスレッドが使用中の可能性があるため）。
- パネルは専用スレッド + 専用メッセージループ。状態は `Arc<Shared>` で共有し、
  フィルタが先に破棄されたら `Shared::alive` が false になって触らなくなる。
- パニックは FFI 境界ごとに `catch_unwind` で捕まえ、パススルーへ退避する
  （`panic = "unwind"` 固定）。

---

## 6. このマシンでの検証結果（記録）

2026-10-17 実施。`run_obs_verify.ps1` 実行ログから:

- ロード: `[obs-noise-suppressor] module loaded (libobs api 0x20020002, obs_source_info 424 bytes, ort api 17)`
- ORT: `onnxruntime 1.31.0 (C:\ProgramData\obs-studio\plugins\obs_noise_suppressor\data\dpdfnet_onnxruntime.dll)`
- 状態: `status: 48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2`
- 実機遅延: `2880 samples (6.000 hop, 60.0 ms)` / 相関 `0.8140`
- 実機 vs オフライン参照: `1248640 frames x 2 ch / max diff = 0e0 / diff rms = 0e0`（**ビット一致**）
- パネル: `パネルを表示しました (858, 488, 844x464, scale 1.00)` → `panel_live.png` で
  **入力 VU 2 本（針 + 現在値 / ピーク dBFS）・縦フェーダー 3 本（dB と % 併記）・
  GR メーター（実測 -17.4 dB）・3 バンドメーター・メモリー行・MODEL 刻印** が
  実測値つきで正しく描画されることを確認（`panel_live.png`）
- メーターの実測: 入力 `-29.9 dBFS` / ピーク `-4.9 dBFS`（検証用 wav を通した値）
- エンジン生成の検証: `activate` を一時的に無効化しても、`create` 時点で ORT が
  読み込まれ `status:` 行が出て、実機 vs オフライン参照がビット一致することを確認
  （= 後からフィルタを追加したケースでも動く）
- 自動テスト: `cargo test --release --lib` → **38 passed / 0 failed**（UI のスナップショット、
  メーターの弾道、プリセットの JSON、モデル / VU 基準値の往復を含む）
- インストール済み DLL の SHA256: `679ea862dc57c91a5864b89a8838e6c5318770f4b638e807f1ff3206e028d6fc`
- OBS は WM_CLOSE で正常終了し、`user.ini` は検証前の状態へ復元された

### v0.2.0（2026-10-18 実施）

同じ `run_obs_verify.ps1` を再実行（5 機能 + パネルの質感改修を入れた後）:

- ロード: `module loaded (libobs api 0x20020002, obs_source_info 424 bytes, ort api 17)`
- 状態: `status: 48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2`
- 実機遅延: `2880 samples (6.000 hop, 60.0 ms)` / 相関 `0.7986`
- 実機 vs オフライン参照: `998784 frames x 2 ch / max diff = 0e0 / diff rms = 0e0`（**ビット一致**）
- パネル: `パネルを表示しました (858, 488, 844x464, scale 1.00)` →
  `panel_live.png`（実際に音を流した状態）。**アンバーに灯る VU 盤面（上ほど暖色）・
  太い黒の弧と 0 〜 +3 の赤いオーバーロード帯・赤い「0」「+3」・2 本目の細い
  校正弧（50 / 100 %）・細い黒針と支点カバー・ガラスの斜めの反射**、面板の
  ヘアラインと十字ネジ、フェーダーの金属レールまで描画されることを確認
- メーターの実測: 入力 `-27.9 dBFS` / ピーク `-4.9 dBFS` / GR `-17.1 dB`
- 自動テスト: `cargo test --release --lib` → **38 passed / 0 failed**
- ビルド警告: Release / Debug ともに **0 件**
- インストール済み DLL の SHA256: `d4d480e502f94b82fc7876d61935f83a07706e8f7a9fd0687910842e7a4095e2`
  （ビルド出力と一致、1,586,176 bytes）

### v0.2.1（2026-10-18 実施）

専用ログ出力を入れて版を上げ、`run_obs_verify.ps1` をビルド + インストール込みで再実行:

- ロード: `OBS Noise Suppressor v0.2.1 を読み込みました` →
  `module loaded (libobs api 0x20020002, obs_source_info 424 bytes, ort api 17)`
- 状態: `status: 48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2`
- 実機遅延: `2880 samples (6.000 hop, 60.0 ms)` / 相関 `0.8186`
- 実機 vs オフライン参照: `1260928 frames x 2 ch / max diff = 0e0 / diff rms = 0e0`（**ビット一致**）
- ログファイル: `%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\obs_noise_suppressor.log` が
  自動で作られること（`plugin_config` 側にフォルダが無い状態から作成された）。
  1 セッションは 14 行で、`<時刻> [LEVEL] 本文` がモジュール読み込み → フィルタ生成 →
  ORT 読み込み → `status:` → フィルタ破棄 → モジュール解放まで並ぶ
- 同じ行が OBS 標準ログにも `[obs-noise-suppressor]` 付きで出ること
- パネルの絵: `ui.rs` を変更していないため v0.2.0 と同一（ライブ取り込みは
  メーターの針位置・読取値だけが違う）
- 自動テスト: `cargo test --release --lib` → **54 passed / 0 failed**
  （`obs_log!` → ファイルの経路、世代退避、連続重複の間引き、上限での停止、
  複数行メッセージの整形を含む）
- ビルド警告: Release / Debug ともに **0 件**
- インストール済み DLL の SHA256: `0ae254b4949ce12489b2c40a0ba2688c7a9d8d0fa1ea90545c549edd0f9df98d`
  （ビルド出力・配布 ZIP 内と一致、1,608,192 bytes）
- 配布 ZIP: `OBS-Noise-Suppressor-0.2.1-win64.zip`（53,420,449 bytes、
  SHA256 `c9ea75c392e3bee5d2ed2603ee7ce68bfe992177b79b28ffbe145d905ad303e1`）

### v0.3.0（レート変換対応 / 2026-10-18）

48 kHz 以外のレートでもパススルーせず、入出力で 48 kHz へ相互変換して処理する版。

**実機（OBS）での確認を 48 kHz と 96 kHz の両方で実施しました**（OBS 32.2.2 / 25 秒
再生。`tools/run_obs_verify.ps1` に `-SampleRate` を追加して取得）:

| OBS レート | 入出力フレーム数 | 実機 lag | オフライン参照との差 |
| --- | --- | --- | --- |
| 48 kHz | 1,260,928 / 1,260,928（一致） | `2880 samples (60.0 ms)` | **max diff = 0e0**（ビット一致） |
| 96 kHz | 2,521,824 / 2,521,824（一致） | `5988 samples (62.4 ms)` | **max diff = 0e0**（ビット一致） |

96 kHz の行は、**配布 ZIP と同一バイトの DLL**（`install.ps1 -SkipBuild` で入れたもの）で
取り直したもの。パッケージの中身がそのまま実機で正しく動くことを確認している。

- 96 kHz の遅延はエンジンの申告どおり `2880@48k + 変換 2.38 ms = 5988 samples`
  （オフラインの `offline_rate_conversion_96k` の 62.4 ms と一致）。
- 96 kHz の実機出力は、同じ入力をオフラインの 96 kHz エンジンに通した結果と
  **1 サンプルも違わない**。チャンネル・ブロック長・フォーマット変換を含めて
  OBS 経路が正しいことが確認できた。
- 96 kHz の実ログ:
  `status: 96 kHz / ch=2 / latency 2880 samples (60 ms) / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 …`
  パネルには `2 ch · 96 kHz · 遅延 2880 samples (60 ms +SRC 2.4 ms) · …` と出る。

オフライン検証（`DPDFNET_MODEL=dpdfnet2_48khz_hr_2out.onnx` / `speech_tail_48k.wav`）:

- `offline_rate_conversion_96k`（96 kHz 入力 → 本経路 → 96 kHz 出力）
  - 出力サンプル数が入力と**完全一致**
  - 内容遅延 `5989 samples (62.4 ms)` / 期待 `5988`（= 60 ms + 変換 2.4 ms）、相関 `0.9341`
  - 出力を 48 kHz に戻したものと 48 kHz 版の相関 `0.9981`、相対 RMS 差 `0.061`
  - 出力レベル: 48 kHz 版 `0.078000` / 96 kHz 版 `0.078040`（差 0.05 %）
- `rack_keeps_sample_counts_and_level`（モデル不要の変換段単体）
  - 22.05 / 44.1 / 88.2 / 96 / 192 kHz × ブロック長 1 / 7 / 480 / 1023 / 1024 / 4096 で
    入出力のサンプル数が一致、ゼロ埋めは起動直後のみ、定常で直流がそのまま通る、L/R 一致
- `live_dump_matches_reference`（実機ダンプ照合）はサンプルレートを WAV から読むように
  したので、48 kHz / 96 kHz のどちらでも同じ手順で確認できる
- 自動テスト: `cargo test --release --lib` → **62 passed / 0 failed**、ビルド警告 0 件
- ビルドした DLL: 1,630,720 bytes / SHA256
  `128097cfbaf0edb5c932d3bf026d32eb3535f3eb7cbed9224ed453e61252d616`
  （配布 ZIP 内・インストール済みとバイト一致）
- 配布 ZIP: `OBS-Noise-Suppressor-0.3.0-win64.zip`（53,429,737 bytes、SHA256
  `2e3c58c9280733523b98dcdfa0ced7d99cab32dce2b6dfbd25183847548d2d59`）
  - 初版の資産から **LICENSE だけ差し替えた再パッケージ**（DLL は `128097cf…` のまま不変）。
    理由は「ライセンス表示（GitHub の判定）」の節を参照
- **ハマりどころ（実際に起きた）**: `build.ps1 -Tests` はテスト用の lib しか作らず、
  cdylib（配布 DLL）を作り直さない。バージョンを上げたあとに `make_package.ps1
  -SkipBuild` を回すと、**古いバージョン文字列のままの DLL を梱包**してしまう
  （v0.3.0 で実際に発生。DLL に `0.2.1` が残っていた）。いまは `make_package.ps1`
  が梱包した DLL に `Cargo.toml` のバージョン文字列が入っているか検査し、
  無ければ例外で止める。バージョンを上げたら必ず `build.ps1 -Configuration Release`
  （または `install.ps1`）で cdylib を作り直すこと。

### v0.3.0 追記（IN / OUT VU + フェーダーごとのミュート / バイパス / 2026-10-18）

パネルの 2 本の VU を **入力（IN）と出力（OUT）** に変え、3 本のフェーダー
（AMBIENCE / VOICE / REVERB）それぞれに **MUTE / BYP キー**を足した版。
バージョン番号は上げていない（v0.3.0 のまま資産を差し替え）。

**設計上の要点**: モードの既定は **有効（Active）** なので、**DSP の音声経路は従来と
1 ビットも変わらない**（`apply(v, min, neutral)` を通すだけで、既定では
`v` がそのまま返る）。したがって上の節のビット一致・遅延の実測はそのまま有効で、
今回**新しい DLL でも取り直して同じ値**になることを確認した。

| キー | AMBIENCE | VOICE | REVERB |
| --- | --- | --- | --- |
| **MUTE**（赤 LED） | 0 %（暗騒音を残さない） | −12 dB | 0 %（残響を残さない） |
| **BYP**（アンバー LED） | 100 %（そのまま） | 0 dB（掛けない） | 100 %（そのまま） |

- MUTE と BYP は**排他**（同じキーをもう一度押すと有効へ戻る 3 状態）。
  **つまみの値と保存値は書き換えず**、効きだけを差し替える（値表示は暗くなる）。
- 設定キーは `ambienceMode` / `voiceMode` / `voiceReverbMode`
  （`"active"` / `"mute"` / `"bypass"`）。OBS プロパティにも各スライダーの直後に
  3 択リストとして出る。プリセット JSON も `ambience_mode` / `voice_mode` /
  `reverb_mode` を保存する（**キーが無い古いプリセットは Active として読む**）。
- メーターは**ステレオで L / R の平均**を 1 本の針にまとめる（モノラルは ch0 のみ）。
  死んでいるチャンネルで半分に落ちないよう、割る相手はチャンネル数で変えている。

**このマシンでの検証結果**:

- 自動テスト: `cargo test --release --lib` → **69 passed / 0 failed**、
  `cargo build --release` のビルド警告 **0 件**（Debug / Release の両方で確認）
- 実機（OBS 32.2.2 / `tools/run_obs_verify.ps1` / 25 秒）:
  `lag = 2880 samples (60.0 ms)` / 相関 `0.8186` / in_rms `0.116016` / out_rms `0.027953`、
  オフライン参照との差は **1,260,928 frames / max diff = 0e0 / diff rms = 0e0**（ビット一致）、
  `live_dump_matches_reference` 1 passed
- 実機のパネル（`panel_live.png`）で **IN / OUT の 2 本の VU（名前つきの窓）**と
  **3 本ぶんの MUTE / BYP キー**が正しく描画されることを確認（既定は有効 = LED 消灯）
- オフラインのスナップショット（`DPDFNET_SNAPSHOT_DIR` を設定して
  `cargo test --lib ui::`）で、**MUTE 点灯 / BYP 点灯 / 1 出力モデルでの REVERB 無効化**の
  3 通りを目視確認（`panel-modes.bmp` など）
- ビルドした DLL: 1,640,960 bytes / SHA256
  `444afdad2a9bb01601d055e9d67908c477ceebac234af760acbd15c754e1ec38`
  （配布 ZIP 内・インストール済みとバイト一致）
- 配布 ZIP: `OBS-Noise-Suppressor-0.3.0-win64.zip`（53,434,905 bytes、SHA256
  `e74dc1e0dd05b6003f15f4c302bda49f1975e5af8e88524e1d91fc8165ddc9b7`）

### v0.3.0 追記 2（負荷の診断と自動フォールバック / 2026-10-18）

「音の遅延が 3 秒近くある / 音がブツ切れになる」という別 PC からの報告を受けて、
**処理がリアルタイムに間に合っているか**をログから判断できるようにした版。
**DSP の経路は 1 ビットも変えていない**（既定のモデル・パラメータでのビット一致は
下のとおり再確認。上の節の DLL / ZIP のハッシュはこの版で置き換わる）。
バージョン番号は上げていない（v0.3.0 のまま資産を差し替え）。

**まず分かったこと（このマシンでの実測、ステレオ / 1 ホップ = 10 ms の予算）**:

| モデル | 1 ホップの処理時間 | 予算に対する割合 |
| --- | --- | --- |
| 高品質 `dpdfnet8`（「自動」で選ばれる方） | オフライン 6.5 – 7.1 ms / 実機 7.2 – 8.6 ms | **70 – 86 %** |
| 軽量 `dpdfnet2` | 約 2.1 ms | 約 21 % |

- 測定 1: 旧 VST3 版のベンチ（`python tools/bench_model.py --models … --threads 1`）
  → `dpdfnet8_48khz_hr_2out.onnx` は mono 3.55 ms（ステレオ = 2 インスタンスで
  7.11 ms）、`dpdfnet2_48khz_hr_2out.onnx` は mono 1.06 ms（同 2.12 ms）
- 測定 2: 実機のダンプ（26.3 秒 × 2 ch）をオフラインで流した時間 → 6.54 ms/hop
- 測定 3: 実機（OBS 内）の新ログ → **7.2 – 8.6 ms/hop (72 – 86 %)**。
  オフラインより重いのは OBS の他スレッドと CPU を分け合うため
- **スレッドを増やしても速くならない**: `--threads 2` で 6.37 ms、`4` で 6.33 ms、
  `8` では 9.69 ms と悪化（モデルが 1 ホップごとに小さいので並列化の利得より
  オーバーヘッドが勝つ）。旧 VST3 版と同じ **1 スレッド**のままでよい
- したがって「3 秒近く遅れる + ブツ切れ」は**CPU が足りていない症状**であって、
  プラグインが遅延を溜めているわけではない。内容遅延は 2880 サンプル (60 ms) の
  まま **1,207,680 フレーム流しても動かない**（下の `max diff = 0e0`）

**入れたもの**:

- `dsp::engine::Meters` に 1 ホップの処理時間の累計（ホップ数 / 予算超過数 /
  合計 / 最大）を追加。オーディオスレッドはリラックスしたアトミックを足すだけで、
  アロケーションもロックもしない（`Instant::now()` 2 回 = 数十 ns）
- `src/load.rs`: 累計 2 点の差分から「直近の負荷」を出す純関数（+ 10 テスト）
- `filter.rs` / `dsp::engine.rs`: 自動フォールバックのガード条件と負荷の計上に
  単体テスト 3 本（`can_fallback_to_light` / `rate_label` / `meters_records_dsp_time`）
- `filter.rs`: 1 秒周期の監視スレッド。`destroy` では `alive` を落としてから
  **join して**後始末するので、`FilterData` のポインタがぶら下がらない
- WARNING は 60 秒に 1 回まで、自動フォールバックは 1 フィルタにつき 1 回だけ

実機のログ（このマシン。73 % なので警告は出ない）:

```text
2026-10-18 23:19:24.958 [INFO] 音声処理の負荷: 1 ホップ平均 7.25 ms / 予算 10.00 ms (73 %)、最大 8.80 ms
```

警告の文面（しきい値を一時的に緩くして確認した回の実測。現在のしきい値では、
このマシンでは出ない）:

```text
2026-10-18 23:04:12.969 [WARNING] 音声処理がリアルタイムに間に合っていません (1 ホップ 10.00 ms の枠に平均 8.51 ms、予算超過 3 %、最大 10.94 ms)。音が途切れる場合は、プロパティの「モデル」を「軽量 (dp2)」にすると負荷が約 1/3 になります
```

しきい値（`src/load.rs`）:

| 判定 | 条件 |
| --- | --- |
| 警告（WARNING、60 秒に 1 回まで） | 平均が予算の 95 % 超、または予算超過のホップが 25 % 以上 |
| 軽量へ自動切替（1 回だけ） | 平均が予算の 100 % 超、または予算超過が 50 % 以上が **3 秒続いた**とき |

単発で 10 ms を少し超えるホップを数えているのは、**OBS が 21 ms ぶんをまとめて
処理する**ため（実測でも速い CPU で 5 % 前後ある）。したがって主たる判定は
「平均が予算を使い切っているか」で、超過ホップの割合は「スレッドが奪われ続けて
いる」ような異常時のための保険として、高い水準にしてある。この 2 段構えに
した理由は実機で確かめた: 予算の 73 – 86 % / 超過 6 % で動いている機械では
警告を出さないこと、一時的に 85 % しきい値にした回では出ることを確認した。

自動で切り替えるのは `DPDFNET_MODEL` 未設定 / プロパティの「モデル」が**自動** /
`dpdfnet2` が探索先にある、の 3 条件を満たすときだけ。**明示指定**（高品質・軽量）
なら警告だけ出して切り替えない。切り替えたときは WARNING とパネル最下段
（`model dp2 (負荷により自動切替)`）に出し、設定は書き換えない（プロパティで
モデルを選び直すと元に戻る。パネルのつまみを動かしただけでは解除しない —
解除すると高品質へ組み直してまた途切れるため）。

**このマシンでの検証結果**:

- 自動テスト: `cargo test --release --lib` → **82 passed / 0 failed**、
  `cargo build --release` のビルド警告 **0 件**（Debug / Release の両方で確認）
- オフラインの DSP 検証 5 本すべて成功（`offline_rate_conversion_96k`
  = 96 kHz で実測 5989 vs 期待 5988 サンプル、`offline_latency_and_channel_consistency`
  = mono と stereo[0] の差 0e0、`live_dump_matches_reference` など）
- 実機（OBS 32.2.2 / `tools/run_obs_verify.ps1` / 25 秒 / 48 kHz）:
  `lag = 2880 samples (60.0 ms)` / 相関 `0.8129` / in_rms `0.115438` /
  out_rms `0.027742`、オフライン参照との差は
  **1,207,680 frames × 2 ch / max diff = 0e0 / diff rms = 0e0**（ビット一致）
- 監視スレッドの出力を実機のログで確認（上の INFO。73 % < 95 % / 超過 6 % < 25 %
  なので警告は出ない）
- **未確認**: 自動フォールバックの発火そのもの（このマシンは予算内で発火しない）。
  ガード条件は `can_fallback_to_light` の単体テストで確認している
- ビルドした DLL: 1,696,256 bytes / SHA256
  `05ce800cc130149aaf882e736b321d0fd80334ba309a13739eb7bfbaece4f81f`
  （`target\release` とインストール済みがバイト一致）
- 配布 ZIP: `OBS-Noise-Suppressor-0.3.0-win64.zip`（下の節で更新）

## 7. ライセンス表示（GitHub の判定）

GitHub は `LICENSE` を **licensee** で判定する。しきい値は「正規テキストとの語彙一致
**98% 以上**」（`CONFIDENCE_THRESHOLD = 98`）で、さらに正規化後の**長さの差 4 文字ごとに
分母が 1 増える**ペナルティが付く。実際、自作の前書き（`OBS Noise Suppressor` /
`Copyright (C) …` / or later の注意書き）を冒頭に置いていたときは一致率 **86.6%** まで
落ちて `NOASSERTION`（Other）になっていた（licensee の式を Python で再現して確認）。

- 対策: `LICENSE` は GPL-2.0 の**正規テキストだけ**にする（choosealicense 版とバイト同一）。
  これで一致率 100% になり `GPL-2.0` と判定される。
- 著作権表示と `GPL-2.0-or-later` は README / 配布 `README.txt` 側に書く。
- `COPYING` に正規の全文を置いても効かない（判定は `LICENSE` を見て、`COPYING` には
  降りない。GitHub の `GET /repos/{owner}/{repo}/license` が `LICENSE` を返すことで確認）。
- 本文を正規テキストから変えると 98% を割ることがある（例: FSF の住所行を
  `<https://fsf.org/>` に戻すだけで 98.2% まで下がる）。`LICENSE` は触らない方がよい。
