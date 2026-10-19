# DPDFNet Voice — VST3 版（DAW 用）

OBS 版フィルタ（[`clear_re/obs_plugin`](../obs_plugin/README.md)）と**同じ DSP コア**
（[`clear_re/dpdfnet`](../dpdfnet/)）を使う VST3 プラグインです。話し声から暗騒音と
残響を落とすところ、パラメータの意味、遅延（48 kHz で 60 ms）は OBS 版と同一です。

- 旧 VST3 版（JUCE、[`clear_re/plugin`](../plugin/)）の復活ではありません。
  オーディオ経路は現行の Rust DSP をそのまま共有しています。
- ホスト側は [nih-plug](https://github.com/robbert-vdh/nih-plug)（`rev` 固定）です。

## 入れる（配布パッケージ）

1. `DPDFNet-Voice-x.y.z-vst3.zip` を展開します。
2. `install.bat` を実行します（中身は `install.ps1`）。

```text
install.bat                       C:\Program Files\Common Files\VST3\   (管理者が要ります)
install.bat -User                 %APPDATA%\VST3\                     (管理者不要)
install.bat -Dest "D:\VST3"       好きな場所
```

3. DAW を起動し直して（プラグインを再スキャンさせて）、`DPDFNet Voice` を挿します。
   モノラルのトラックにもステレオのトラックにも挿せます。

消すときは `uninstall.bat`（既定では上の 2 か所を探します）。

配置されるもの（bundle の中で自己完結しています）:

```text
DPDFNet Voice.vst3\Contents\x86_64-win\DPDFNet Voice.vst3   本体
                                      \dpdfnet_onnxruntime.dll
                                      \models\dpdfnet8_48khz_hr_2out.onnx  高品質
                                      \models\dpdfnet2_48khz_hr_2out.onnx  軽量
```

モデルと ONNX Runtime は OBS 版とまったく同じもの（リポジトリに追跡済み）です。
**OBS 版が入っていなくても動きます。**

## ビルド

```powershell
cd clear_re\vst3
.\build.ps1                      # cargo build --release
.\build.ps1 -Tests               # テスト（DSP + パネル UI）
.\tools\make_package.ps1         # dist\ に配布パッケージと ZIP を作る
.\tools\make_package.ps1 -WithGpu   # GPU (DirectML) の DLL も同梱する
```

必要なもの: Rust（MSVC ツールチェーン）+ Visual Studio の C++ ビルドツール。
初回は nih-plug と VST3 SDK を取ってくるため**ネットワークが要ります**
（2 回目以降はキャッシュされます）。OBS は不要です。

出力は `target\release\dpdfnet_voice.dll` です。手で試すときは
`DPDFNet Voice.vst3` にリネームして VST3 フォルダに置いてください
（同梱のモデル / ONNX Runtime の探索先は「DLL の隣」と「DLL の隣の `models\`」です）。

## パラメータ

| パラメータ | 範囲 / 既定 | 内容 |
| --- | --- | --- |
| Ambience | 0 – 100 % / 50 % | 残す暗騒音。0 % = 最大抑圧、100 % = そのまま |
| Voice | −12 – +12 dB / 0 dB | presence |
| Voice Reverb | 0 – 100 % / 0 % | 残す残響。0 % = 残さない、100 % = そのまま |
| （各モード） | 有効 / ミュート / バイパス | つまみの位置は保ったまま効きだけ切る（OBS 版の MUTE / BYP と同じ） |
| Model | 自動 / 高品質 (dp8) / 軽量 (dp2) | **自動化されない**設定 |
| Inference Device | CPU / GPU (DirectML) | **自動化されない**設定。既定 CPU |
| Bypass | オフ / オン | 全体のバイパス。オンにすると素通し（パネルの BYPASS キー） |

`Model` と `Inference Device` は自動化に載せていません（音声スレッドで組み立て直す
必要があり、オートメーションで頻繁に変わると破綻するため）。**変えたときは
`process` の先頭で組み立て直すので、その一瞬だけ音が途切れます。** 状態は DAW の
保存（プリセット）に乗ります。

## GUI（パネル）

OBS 版と**同じ自前 Win32 パネル**を、DAW が渡す親ウィンドウの子ウィンドウとして
出します（共通クレート [`clear_re/panel_ui`](../panel_ui/)）。見た目・数値・操作は
OBS 版と揃えてあります。

```text
+--------------------------------------------------------------+
| OBS NOISE SUPPRESSOR          MEMORY 1 2 3 4 5 6 7 8  SAVE  |
|  IN (VU)      OUT (VU)      CONTROL          EFFECT          |
|  [針 + 数値]  [針 + 数値]   AMBIENCE|VOICE|REVERB   GR + 3 バンド|
|                              MUTE/BYP 付き                    |
|  ● 48 kHz / ch=2 / ...        MODEL AUTO   DEV CPU   BYPASS   |
+--------------------------------------------------------------+
```

操作できるもの:

| 場所 | 操作 |
| --- | --- |
| AMBIENCE / VOICE / REVERB | ドラッグ / ホイール / ダブルクリックで既定値へ。ホストの自動化にも乗ります |
| MUTE / BYP | つまみの値は保ったまま効きだけ切る。同じキーをもう一度押すと戻る（排他） |
| BYPASS | 全体の有効 / 無効。オフにすると素通しし、針は静止位置へ戻ります |
| MODEL | 押すたびに 自動 → dp8 → dp2 → 自動（`Model` パラメータと同じ） |
| DEV | 押すたびに CPU → GPU → CPU（`Inference Device` と同じ） |
| MEMORY 1〜8 | 押すとその設定を呼び出す。`SAVE` を押してからスロットを押すと保存。`Ctrl` + クリックで消去 |
| メーター | IN / OUT の VU とピーク、EFFECT の GR / 3 バンド。ダブルクリックでピークホールドを消す |

メモリーの保存先は `%APPDATA%\DPDFNet Voice\presets.json` です（DAW のプロジェクト
やホストのプリセットとは別。プラグインをまたいで使い回せます）。

## 遅延

| レート | 報告する遅延 |
| --- | --- |
| 48 kHz | **2880 サンプル (60 ms)** |
| 96 kHz | 5988 サンプル (約 62.4 ms) = 2 倍 + レート変換段 |

内部は 48 kHz 固定で、レート変換は入出力だけです（OBS 版と同じ）。ホストには
`set_latency_samples` で報告するので、DAW 側で自動補正されます。録音しながら
聴くと 60 ms 遅れて聞こえます。

## 起動時のモデル選択（「自動」のとき）

`Model` が「自動」のときは、**音を出す前に**実際に何ホップか回して 1 ホップの
平均処理時間を実測します（`Engine::measure_hop_us`）。予算（1 ホップ = 10 ms）の
75 % を超えていれば、**最初から軽量モデルで開始**します。

- 実測はマシンが空いているときに取るので実運用より小さく出ます（開発機の dp8:
  実測 5.5 ms ↔ 実運用 6.7 ms = 約 1.2 倍）。その差を見込んで実行中の警告水準
  (95 %) より低い 75 % で判定します。
- 実測は `reset()` で終わるので、エンジンには痕跡が残りません（オフライン検証で
  ビット一致を確認しています）。
- `Model` を明示指定したとき（自動以外）と `DPDFNET_MODEL` で固定したときは
  何もしません。

OBS 版の「3 秒続けて間に合わないと軽量へ落とす」監視は、その 3 秒ぶん音が乱れて
「音が 3 秒遅れる」ように見えるのが弱点でした。VST3 版はホストが音を流す前に
`initialize` で組み立てるので、その 3 秒が**録音に残りません**。

## 検証

`DPDFNET_ORT` と `DPDFNET_MODEL` を指定してテストを回すと、ホスト（DAW）無しで
オフライン検証します（未設定ならスキップして成功扱い）。

```powershell
$env:DPDFNET_ORT   = "..\obs_plugin\vendor"
$env:DPDFNET_MODEL = "..\plugin\models\dpdfnet8_48khz_hr_2out.onnx"
.\build.ps1 -Tests -NoCapture
```

| テスト | 確かめること |
| --- | --- |
| `output_does_not_depend_on_the_host_block_size` | ブロックサイズ 1 / 480 / 960 / 1024 / 4096 / 8192 で出力が**ビット一致**（DAW は好きな長さで呼んでくる） |
| `mono_and_stereo_produce_the_same_channels` | モノラルとステレオ（同一入力）でチャンネル出力がビット一致 |
| `latency_is_reported_in_host_samples` | 48 kHz = 2880 / 96 kHz = 2 倍 + 変換段 |
| `auto_model_calibration_picks_one_model_without_leaving_a_trace` | 「自動」の出力が、選ばれたモデル（高品質 / 軽量）の出力と一致する |
| `default_snapshot_matches_the_obs_defaults` ほか | パラメータの既定値と範囲が OBS 版と同じ |

DSP そのもの（遅延 2880 サンプル、モデルの読み込み、レート変換、負荷判定）は
[`clear_re/dpdfnet`](../dpdfnet/) 側のオフライン検証で確認しています。実機（OBS）の
検証手順は [`../obs_plugin/OBS_VERIFY.md`](../obs_plugin/OBS_VERIFY.md) を参照。

## 制限と注意

- パネルは**固定サイズ**です（リサイズできません）。倍率はホストが
  `IPlugViewContentScaleSupport::set_scale_factor` で伝えてきた値に従います
  （伝えてこないホストでは 1.0 のままなので、高 DPI では小さく見えることがあります）。
- ホストによっては、子ウィンドウが他のウィンドウの後ろに隠れたり、DAW 側の
  再描画の都合でパネルの更新が遅れることがあります（音には影響しません）。
- MODEL / DEV はクリックで選択肢を巡回するキーです（ドロップダウンはありません）。
- メモリー（1〜8）は DAW のプロジェクトではなく
  `%APPDATA%\DPDFNet Voice\presets.json` に保存されます。
- ログは DAW 側に出ます（`NIH_LOG=<ファイル>` でファイルへも出せます）。
- `Model` を `GPU (DirectML)` にするには GPU 用の DLL が必要です。`-WithGpu` 付きで
  作ったパッケージなら同梱されています。**実測では GPU は CPU より遅い**です
  （開発機 / ステレオ: dp2 2.51 → 4.66 ms、dp8 6.68 → 12.73 ms）。
- GPU を使うときは出力が CPU と**ビット一致しません**（DirectML の計算順が違う
  ため）。音質の差は実用上問題ありませんが、検証のつもりで比べないでください。

## ファイル構成

| パス | 内容 |
| --- | --- |
| `src/lib.rs` | nih-plug の `Plugin` / `Vst3Plugin`（パラメータ → DSP、遅延の報告、BYPASS） |
| `src/params.rs` | パラメータ定義（OBS 版と同じ範囲・既定値・意味） |
| `src/core.rs` | DSP の駆動（組み立て / レート / 遅延 / キャリブレーション）。ホスト無しでテストできる形 |
| `src/editor.rs` | パネル（`panel_ui`）のホスト結線。子ウィンドウ / `ParamSetter` |
| `src/offline_tests.rs` | オフライン検証（ブロックサイズ非依存など） |
| `build.ps1` | vcvars → cargo |
| `installer/install.ps1` / `uninstall.ps1` | 配布パッケージに入るインストーラ（BAT は起動だけ） |
| `tools/make_package.ps1` | 配布パッケージと ZIP を作る |
