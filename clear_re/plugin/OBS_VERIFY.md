# OBS 実機確認手順

対象: OBS 32.x + **atkAudio**（VST3）。この環境では atkAudio を確認済み
（`C:\Program Files\obs-studio\obs-plugins\64bit\atkaudio-pluginforobs.dll`、OBS 32.2.2）。

前提が満たされていること:

- OBS の設定 → 音声 → **サンプルレート = 48 kHz**（本プラグインは 48 kHz 専用。それ以外はパススルー）
- `Documents\OBS\VST3\DPDFNet Voice.vst3` と `Documents\OBS\VST3\models\*.onnx` が配置済み

---

## 0. 先に「期待値」を知っておく

聴感だけで判断すると「効いているのか分からない」となりがちなので、先に数字を確定させる:

```powershell
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
- CPU（offline, 1 フレーム = 10 ms 音声）: stereo で dpdfnet2 1.66 ms（RTF 0.17）/ dpdfnet8 4.79 ms（RTF 0.48）

**残響つきで Voice Reverb を 0→1 にすると、尾が 20 dB 以上小さくなる**のが正常な状態。
聴いてこれが分からなければ、どこかで止まっている。

---

## 1. プラグインが読まれているか（音を出さずに確認）

1. OBS に **メディアソース** を追加 → `test_signals\speech_tail_reverb_48k.wav`、**ループ ON**、ミュートでよい
2. そのソース → **フィルタ** → `+` → **atkAudio (VST3)** を追加
3. プラグイン一覧に **DPDFNet Voice** が出るか
   - 出ない場合: atkAudio の VST3 検索パスに `Documents\OBS\VST3` を追加して再スキャン
     （`atkaudio-pluginforobs_scanner.exe` でも可）
4. 選択するとエディタが開く。**ステータス行**を確認:

| 表示 | 意味 |
| --- | --- |
| `48 kHz / model loaded (2 outputs)` | ✅ 正常。Voice Reverb が有効 |
| `48 kHz / model loaded (1 output)` | 1 出力版しか見つかっていない → `models\` の配置を確認 |
| `48 kHz only - passthrough` | OBS が 48 kHz になっていない |
| `model not found ...` | `models\*.onnx` が見つからない |

5. メディアソースの音声メーターが動いていれば DSP が回っている

---

## 2. 聴いて確認（主たる検証）

各ファイルを再生し、フィルタの **Bypass** を ON/OFF して聞き比べる。

| 確認 | 期待 |
| --- | --- |
| `speech_tail_48k.wav`（ノイズのみ） | ノイズが消え、無音区間が静かになる |
| `speech_tail_reverb_48k.wav`（残響つき） | 残響が締まる。**Voice Reverb 0→1 で尾が明らかに消える** |
| **Ambience** 左→右 | 左で最大抑圧、右で入力に近づく（環境ノイズが戻る） |
| **Voice** −12→+12 | 高域の色づけと音量の変化 |
| **Bypass** | 素通しに戻る。**遅延は維持されない**（ON/OFF で音の位置が 60 ms 飛ぶ） |

**リアルタイム性の確認（重要）**:

- プツ切れ・クリック・ドロップアウトが出ないこと
- OBS の **表示 → 統計** で「ドロップしたフレーム（ネットワーク/エンコード）」が増えないこと
- CPU 使用率が常識的な範囲に収まること（重ければ `models\` から `dpdfnet8_*.onnx` を外すだけで
  dpdfnet2 に落ちる）

マイクで実際に使う場合は、マイクソースに同じフィルタを挿す（48 kHz であること）。

---

## 3. 遅延と PDC（プラグイン遅延補正）の確認

### 3.1 聴感チェック（簡単）

同じメディアソースをもう 1 つ追加し（フィルタ無し）、**両方をミックスに出す**。

- PDC が正しく効いていれば、2 つは重なって聞こえる（単に大きくなる）
- 60 ms ずれていると **スラップバック/ダブり**として明確に聞こえる

ずれて聞こえる場合の実用上の影響: マイク 1 本をフィルタに通して配信するだけなら、
そのソースが 60 ms 遅れるだけで**問題にはならない**（自分の声を直接モニターしない限り）。
気になる場合のみ atkAudio の遅延補正/手動ディレイ設定を確認する。

### 3.2 正確に測る場合（参照経路が必要・任意）

OBS 内では参照経路が無いため、**2 トラック録音**で測る。

1. 同じシーンにメディアソースを 2 つ追加（両方 `test_signals\impulse_48k.wav`、ループ ON）
   - 「impulse (plugin)」 … atkAudio → DPDFNet Voice を挿す
   - 「impulse (ref)」 … フィルタ無し
2. 音声ミキサーの歯車 → **詳細音声プロパティ**で分離:
   - 「impulse (plugin)」 のトラックは **1 のみ ON**
   - 「impulse (ref)」 のトラックは **2 のみ ON**
3. 録画を開始 → 2〜3 秒 → 停止
4. mkv から 2 トラックを 1 つのステレオ wav にする（ffmpeg が必要。無ければ §3.1 で足りる）:

   ```powershell
   ffmpeg -i rec.mkv -filter_complex "[0:a:0][0:a:1]amerge=inputs=2[a]" -map "[a]" -ac 2 -ar 48000 out.wav
   ```

5. 解析:

   ```powershell
   python tools\analyze_obs_capture.py --two-track out.wav
   ```

| 測定結果 | 判定 |
| --- | --- |
| **2880**（= 6 hop） | atkAudio が PDC を掛けていない。プラグインの申告値どおりで正常 |
| **0** | PDC が完全に働いて補正済み。これも正常 |
| それ以外 | 不一致。`src\PluginProcessor.h` の `kLatencyHops` と atkAudio の設定を確認 |

> 測定器自体は `python tools\analyze_obs_capture.py --selftest` で検証済み
> （既知の遅延 2880 と減衰 −12 dB を仕込むと、そのとおりに測る）。

---

## 4. 効果を数値で確認したい場合（任意）

OBS の録音から wav を作り（詳細出力の「カスタム出力 (FFmpeg)」でコンテナ `wav` /
音声エンコーダ `pcm_s16le` にすると直接 wav で録れる）:

```powershell
python tools\analyze_obs_capture.py --input test_signals\speech_tail_reverb_48k.wav --capture obs_rec.wav
```

- メディアソースの再生開始位置が任意なので**絶対遅延は測れない**（§3.2 を使う）
- 低活動区間の減衰量は測れる → §0 の期待値と比較する
- **Voice Reverb を 1 にして録る / 0 にして録る** の 2 本を比べれば、de-reverb の効き幅が出る

---

## 5. 困ったときの切り分け

| 症状 | 見るところ |
| --- | --- |
| プラグイン一覧に出ない | atkAudio の VST3 検索パス / 再スキャン / バンドルの中身（`Contents\x86_64-win\` に DLL 2 つ） |
| 音がまったく出ない | Bypass、ミックスのミュート、OBS のサンプルレート |
| ステータスが `passthrough` | OBS が 48 kHz でない（設定 → 音声） |
| ノイズが消えない | メーターが動いているか、`models\` の配置、モデル名 |
| プツプツ切れる | OBS の統計（ドロップフレーム）、CPU。`dpdfnet8_*.onnx` をどけて dpdfnet2 に |
| 音が 60 ms ずれる | §3（PDC / 手動補正） |
| Voice Reverb がグレーアウト | 1 出力モデルしか無い（`*_2out.onnx` を `models\` に置く） |
| **プラグインを選んだ瞬間に OBS が落ちる** | 旧ビルドの同梱 `onnxruntime.dll` が原因だった（System32 の ORT 1.17 を掴む）。現在は `dpdfnet_onnxruntime.dll` に改名済みなので、バンドルに `onnxruntime.dll` が残っていないか確認し、古いファイルを消す。`build\Release\dpdfnet_ort_smoke.exe` が `ONNX Runtime 1.31.0` と出れば正常 |
| ステータスが `ONNX Runtime unavailable: …` | 同梱 DLL が見つからない / 古い。`dpdfnet_onnxruntime.dll` の配置とサイズ、`DPDFNET_ORT` を確認 |

---

## 6. 参考: 実装側の確認済み事実

- バンドル: `Contents\x86_64-win\DPDFNet Voice.vst3`（5.4 MB）+ `dpdfnet_onnxruntime.dll`（16.5 MB）
  + `Contents\Resources\moduleinfo.json`
  - **`onnxruntime.dll` という名前で置いてはいけない**（Windows 11 の System32 版 ORT 1.17 と
    名前衝突し、探索順でそちらが読まれて OBS ごと落ちる）。必ず `dpdfnet_onnxruntime.dll`。
- エクスポート: `InitDll` / `ExitDll` / `GetPluginFactory`（dumpbin で確認済み）
- モデル探索: プラグイン DLL の 3 階層上までの `models\` を見るので
  `Documents\OBS\VST3\models\` がそのまま効く
- モデルの優先順: `dpdfnet8_48khz_hr_2out.onnx` → `dpdfnet2_48khz_hr_2out.onnx`
  → `dpdfnet8_48khz_hr.onnx` → `dpdfnet2_48khz_hr.onnx`
