# OBS 実機確認手順（OBS ネイティブ版）

対象: **このプラグイン単体**（VST3 + atkAudio は使いません）。OBS 32.2.2 で確認済み。

前提:

- OBS の設定 → 音声 → **サンプルレート = 48 kHz**（本プラグインは 48 kHz 専用。
  それ以外ではパススルー）
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
| `obs_panel.png` | 全画面スクリーンショット（パネルの見た目） |
| `elevated.log` | 昇格側のログ（全文） |
| `user.ini.bak` | 検証前の `user.ini` の控え |

### 期待する出力（このマシンでの実測）

```text
[obs-noise-suppressor] module loaded (libobs api 0x20020002, obs_source_info 424 bytes, ort api 17)
[obs-noise-suppressor] onnxruntime 1.31.0 (...\plugins\obs_noise_suppressor\data\dpdfnet_onnxruntime.dll) を読み込みました
[obs-noise-suppressor] status: 48 kHz / ch=2 / latency 2880 samples / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2

実機 lag = 2880 samples (6.000 hop, 60.0 ms) corr = 0.8165 (in_rms 0.115885 out_rms 0.027794)
参照 vs 実機: 1256832 frames x 2 ch / max diff = 0e0 / diff rms = 0e0
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
| `48 kHz / ch=2 / latency 2880 samples / <model> \| n_fft=… hop=… state=… outputs=2` | ✅ 正常。`outputs=2` なので Voice Reverb が有効 |
| `48 kHz / ch=1 / latency 2880 samples / …` | ✅ モノラル設定で正常 |
| `OBS の音声が 44100 Hz です。48 kHz に設定してください（今はパススルー）` | OBS が 48 kHz になっていない（設定 → 音声） |
| `DPDFNet モデルが見つかりません (探索先: …)` | `data\models\*.onnx` の配置を確認 |
| `モデルの読み込みに失敗: …` | モデルが壊れている / ORT と不一致 |
| `onnxruntime を読み込めません: …` | 同梱 `dpdfnet_onnxruntime.dll` の配置を確認 |

パネルの 1 行表示は `2 ch  ·  48 kHz  ·  遅延 2880 samples (60 ms)  ·  dereverb あり`
のように短く出ます（詳細はプロパティ側）。

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
| **BYPASS** | 素通しに戻る。**遅延は維持されない**（切り替えると音の位置が 60 ms 飛ぶ） |

**リアルタイム性の確認（重要）**:

- プツ切れ・クリック・ドロップアウトが出ないこと
- OBS の **表示 → 統計** で「ドロップしたフレーム」が増えないこと
- CPU 使用率が常識的な範囲に収まること（重ければ `data\models` から
  `dpdfnet8_*.onnx` をどけて dpdfnet2 に落ちる）

マイクで実際に使う場合は、マイクソースに同じフィルタを挿します（48 kHz であること）。

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

| 測定結果 | 判定 |
| --- | --- |
| `実機 lag = 2880 samples` | ✅ 設計どおり（`kLatencyHops = 6` × hop 480） |
| `max diff = 0e0` | ✅ 実機とオフライン参照がビット一致 |
| `max diff > 1e-4` | ❌ OBS 経路のどこか（チャンネル数 / ブロック / リセット）が怪しい |
| 遅延が 2880 でない | ❌ `src/dsp/engine.rs` の `K_LATENCY_HOPS` とホップ処理を確認 |

---

## 4. 困ったときの切り分け

| 症状 | 見るところ |
| --- | --- |
| フィルタ一覧に「OBS Noise Suppressor」が出ない | インストール先（`bin\64bit\obs_noise_suppressor.dll`）、OBS のログの `[obs-noise-suppressor] module loaded` |
| 音がまったく出ない | フィルタの有効/無効、ミュート、OBS のサンプルレート |
| 状態が `48 kHz に設定してください` | OBS の設定 → 音声 → サンプルレート |
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
- 実機遅延: `2880 samples (6.000 hop, 60.0 ms)` / 相関 `0.8165`
- 実機 vs オフライン参照: `1256832 frames x 2 ch / max diff = 0e0 / diff rms = 0e0`（**ビット一致**）
- パネル: ウィンドウが表示され、入力 VU 2 本・縦フェーダー 3 本・GR / 3 バンド
  メーター・ステータス・BYPASS が正しく描画されることをスクリーンショットで確認
- エンジン生成の検証: `activate` を一時的に無効化しても、`create` 時点で ORT が
  読み込まれ `status:` 行が出て、実機 vs オフライン参照がビット一致することを確認
  （= 後からフィルタを追加したケースでも動く）
- 自動テスト: `cargo test --release --lib` → **15 passed / 0 failed**
- OBS は WM_CLOSE で正常終了し、`user.ini` は検証前の状態へ復元された
