# OBS Noise Suppressor — OBS Studio ネイティブ音声フィルタ

音声向けニューラル強調（ノイズ抑圧 + 残響抑圧）を OBS に直接組み込む、**Rust 実装の
OBS 音声フィルタ**です。旧構成（VST3 + atkAudio）を置き換えるもので、DSP は
JUCE 非依存だった VST3 版のコードをそのまま移植しています（`src/dsp/`）。
推論には DPDFNet モデルを使います。

- フィルタ名（OBS のフィルタ一覧）: **OBS Noise Suppressor**
- フィルタ ID: `dpdfnet_voice_filter`（**据え置き**。変えると既存シーンの
  フィルタが消えるため）
- 対応: OBS Studio 32.x / Windows x64

> 旧名は **DPDFNet Voice** でした。プロジェクト名・モジュール名・DLL 名・
> インストール先・表示名を `OBS Noise Suppressor` / `obs_noise_suppressor` に
> 変更していますが、**フィルタ ID は据え置き**なので既存のシーンコレクションは
> そのまま動きます。旧フォルダが残ると同じフィルタが二重に登録されるため、
> `install.ps1` / `install.bat` は旧名のフォルダを自動で削除します。

## 特徴

| 項目 | 内容 |
| --- | --- |
| サンプルレート | **44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz など**。48 kHz 以外は入出力でレート変換（有理比ポリフェーズ）して内部 48 kHz で処理。変換段を作れない異常値のみパススルー |
| チャンネル | OBS の音声設定に追従（モノラル → モノラル、ステレオ → ステレオ） |
| 内容遅延 | **2880 サンプル = 60 ms 固定**（`kLatencyHops = 6` × hop 480。VST3 版と同一）。48 kHz 以外は変換段のぶん約 2〜5 ms 加算 |
| 推論 | 同梱の ONNX Runtime 1.31.0（`dpdfnet_onnxruntime.dll`） |
| UI | 自前の Win32/GDI パネル（IN / OUT VU 2 本 + 縦フェーダー 3 本 + フェーダーごとの MUTE / BYP + GR / 3 バンドメーター + メモリー 1〜8 + BYPASS） |
| モデル | プロパティで 自動 / 高品質 (dp8) / 軽量 (dp2) を選択（切り替えは数十 ms の無音） |
| 音声スレッド | アロケーションなし（テンソル・バッファは事前確保、`IoBinding` で再利用）。**GPU のときだけ**毎 hop 束ねを作り直す（DirectML が最初に上げた入力を固定してしまうため） |
| パニック | FFI 境界ごとに `catch_unwind`。落ちずにパススルーへ退避 |
| ログ | エラー / 警告 / 起動情報を専用ファイル `obs_noise_suppressor.log` に常時記録（OBS 標準ログにも同じ内容が出ます） |

> **エンジンの生成タイミング**
> libobs はフィルタの `activate` を「親ソースのアクティブ状態が変わった時」しか
> 呼びません（`libobs/obs-source.c`）。そのため**すでにアクティブなソースへ後から
> 追加した場合**や、映像を持たない音声ソースでは `activate` が来ません。
> 本プラグインは `create` / `update` / `activate` のすべてでエンジン（ONNX セッション）
> を用意します。`update` からの作り直し中にオーディオスレッドが古いエンジンを
> 使っていても壊れないよう、古いエンジンは `destroy` まで保持してから破棄します。

## 動作要件

- OBS Studio 32.x（`obs64.exe`）
- OBS の音声設定は **44.1 / 48 / 88.2 / 96 / 176.4 / 192 kHz** など（48 kHz 以外は
  内部で 48 kHz にレート変換して処理。変換段を作れない異常値のときだけパススルー）
- `dpdfnet_onnxruntime.dll`（同梱）と DPDFNet モデル（`*.onnx`）
- ビルドする場合のみ: Rust (MSVC ツールチェーン) + Visual Studio の C++ ビルドツール

モデルはプロパティの**「モデル」**で選びます（自動 = 高品質を優先）。
**自動**のときは次の優先順で探します（`src/paths.rs`）。

1. `dpdfnet8_48khz_hr_2out.onnx`（品質優先・Voice Reverb 可）
2. `dpdfnet2_48khz_hr_2out.onnx`（速度優先・Voice Reverb 可）
3. `dpdfnet8_48khz_hr.onnx`
4. `dpdfnet2_48khz_hr.onnx`

探索先（`DPDFNET_MODEL` で明示指定も可）:

- OBS のモジュールデータディレクトリ（＝ `data\`）とその `models\`
- `obs_noise_suppressor.dll` の隣、および 2 つ上（`<plugin root>\data` / `data\models`）
- `ドキュメント\DPDFNetVoice\models`
- `ドキュメント\OBS\VST3\models`（旧 VST3 版の配置をそのまま使える）

## ビルド

```powershell
.\build.ps1                      # cargo build --release
.\build.ps1 -Tests               # テストを回す
.\build.ps1 -Tests -TestFilter dsp::offline_tests -NoCapture
```

`build.ps1` は vcvars64 を探して読み込んでから cargo を呼びます。

## インストール

### 配布パッケージを使う（通常はこちら）

```powershell
.\tools\make_package.ps1         # dist\OBS-Noise-Suppressor\ と ZIP を作る
```

できた `dist\OBS-Noise-Suppressor\install.bat` をダブルクリックします
（中身は `install.ps1`）。削除は `uninstall.bat` です。

```text
dist\OBS-Noise-Suppressor\
  install.bat / install.ps1          インストーラー
  uninstall.bat / uninstall.ps1      アンインストーラー
  README.txt                         使い方（日本語）
  bin\64bit\obs_noise_suppressor.dll 本体
  data\dpdfnet_onnxruntime.dll       ONNX Runtime（同梱）
  data\models\*.onnx                 モデル
  data\manifest.json                 OBS のプラグイン情報
```

### 配置先

既定は `%ProgramData%\obs-studio\plugins\obs_noise_suppressor` です。
**管理者権限もレジストリも使いません**（レジストリは読み書きしません）。

```text
%ProgramData%\obs-studio\plugins\obs_noise_suppressor\
  bin\64bit\obs_noise_suppressor.dll   本体
  data\dpdfnet_onnxruntime.dll         ONNX Runtime（同梱）
  data\models\*.onnx                   モデル
  data\manifest.json                   OBS のプラグイン情報
```

> **フォルダ名は DLL 名と一致させてください。**
> OBS は `<フォルダ名>\bin\64bit\<フォルダ名>.dll` を組み立てて読みます。

**インストール先は OBS のエディションに依存しません。** OBS 32 が Windows で
プラグインを探す場所は、通常インストーラー版・Steam 版・Microsoft Store 版の
どれでも次の 1 か所だからです（確認元: OBS ソース
`frontend/widgets/OBSBasic.cpp` の `AddExtraModulePaths()` と
`libobs/obs-windows.c` の `add_default_module_paths()`）。

**例外はポータブル版だけ**です。ポータブル版は `%ProgramData%` を読まないので、
`install.bat` はポータブル版を検出したとき、OBS 側の既定の相対パス

```text
<OBS ルート>\obs-plugins\64bit\obs_noise_suppressor.dll
<OBS ルート>\data\obs-plugins\obs_noise_suppressor\
```

にもコピーします。自動検出から漏れる場合は `install.bat -Obs "D:\obs"` のように
指定してください。

> **一度でも「管理者として実行」したときの注意**
> `%ProgramData%\obs-studio\plugins\` のファイルは、作成したユーザーにしか書き込み権を
> 与えません（`Users` は読み取り専用）。だから管理者として入れたファイルは、非昇格では
> 上書きも削除もできません（`アクセスが拒否されました`）。
> `install.ps1` / `uninstall.ps1` はこれを検出すると**自動で自分自身を管理者として
> 起動し直します**（UAC を出したくない場合は `-NoElevate`。そのときはメッセージに
> 従って手動で管理者実行してください）。逆に、非昇格で新しく入れたファイルは自分の
> 所有になるので、以後の更新も非昇格のまま通ります。

### リポジトリから直接（開発時）

```powershell
.\install.ps1                    # ビルド + 配置（OBS の検出もする）
.\install.ps1 -SkipBuild -SkipModels
```

`install.ps1` は、隣に `bin` / `data` があれば配布パッケージとして、なければ
リポジトリとして（ビルドしてから）配置します。

### インストーラーが BAT ではなく PowerShell な理由

`install.bat` / `uninstall.bat` は**ダブルクリック用の ASCII だけのランチャー**で、
実体は `install.ps1` / `uninstall.ps1` です。純バッチでも書けますが、**cmd.exe は
マルチバイト文字を含むバッチファイルで行を見失うことがあります**（ラベルと
`goto` が多いほど起きやすい）。実測では、日本語を直接書いた純バッチで

- UTF-8 + `chcp 65001`: 264 行中 5 行が壊れ、コメントの一部がコマンドとして実行された
- CP932（`chcp` なし）: 1 行が壊れた

という結果でした。PowerShell は UTF-8 (BOM) のスクリプトを正しく読むので、
日本語のメッセージはそちらに置いています。`-ExecutionPolicy Bypass` で起動するので
実行ポリシーの影響も受けません。

> **`onnxruntime.dll` という名前で置いてはいけません。**
> OBS のプロセスには他プラグイン（atkAudio 等）が System32 の ORT 1.17 を既に
> ロードしていることがあり、名前だけで解決するとそちらを掴んで即死します。
> 本プラグインは必ず絶対パス + `LOAD_WITH_ALTERED_SEARCH_PATH` で
> `dpdfnet_onnxruntime.dll` を読むので、この名前で同梱しています。

インストール後は OBS を再起動してください（OBS の起動中は DLL を上書きできません）。

## 使い方

1. フィルタをかけたいソース（マイク・メディアソースなど）を右クリック → **フィルタ**
2. `+` → **OBS Noise Suppressor** を追加
3. プロパティの **「パネルを開く（フェーダー / メーター / メモリー）」** ボタンで自前パネルが開きます

### プロパティ（OBS 標準 UI）

| 項目 | 範囲 | 既定 | 意味 |
| --- | --- | --- | --- |
| Ambience | 0 – 1 | 0.5 | 残す暗騒音。0 % = 最大抑圧 (−48 dB)、100 % = 最小抑圧 (−6 dB) |
| Ambience の扱い | 有効 / ミュート / バイパス | 有効 | フェーダーを動かさずに効きだけ切る（ミュート = 0 % / バイパス = 100 %） |
| Voice | −12 – +12 dB | 0.0 | 声の presence（高域の色づけ）。0 dB で完全なパススルー |
| Voice の扱い | 有効 / ミュート / バイパス | 有効 | 同じく効きだけ切る（ミュート = −12 dB / バイパス = 0 dB） |
| Voice Reverb | 0 – 1 | 0.0 | 残す残響。**2 出力モデルのときだけ有効** |
| Voice Reverb の扱い | 有効 / ミュート / バイパス | 有効 | 同じく効きだけ切る（ミュート = 0 % / バイパス = 100 %） |
| モデル | 自動 / 高品質 (dp8) / 軽量 (dp2) | 自動 | 使う ONNX モデル。切り替えると数十 ms 無音になります |
| 推論デバイス | CPU / GPU (DirectML) | CPU | 推論の実行先。**GPU は実測で CPU より遅い**（下記）。追加パックを入れたときだけ選べます |
| VU 基準値 | −20 / −18 / −14 dBFS | −18 dBFS | パネルの VU で「0 VU」にするレベル |

- **〜の扱い（ミュート / バイパス）**: パネルのフェーダーの下にある **MUTE / BYP キーと
  同じ設定**です。**つまみの値と位置はそのままで、効きだけ**を差し替えます
  （ミュート = そのフェーダーの最小値、バイパス = その機能を掛けない中立値）。
  ミュートとバイパスは排他で、どちらも押していない状態が「有効」です。

- **モデル**: 同梱しているのは `dpdfnet8`（高品質）と `dpdfnet2`（軽量・低負荷）の
  2 種類だけなので、選択肢はこの 3 つで足ります。**自動**は高品質を優先して探し、
  無いときだけ軽量に落ちます。2 出力版（Voice Reverb 付き）が無い環境では、
  パネルの REVERB フェーダーが自動で無効（暗く）なります。
  **自動**で高品質を使っていて音が途切れる（＝処理が 10 ms の枠に間に合わない）
  状態が続くと、1 度だけ軽量へ自動で切り替わります（[CPU 負荷の目安](#cpu-負荷の目安音が途切れる--遅れて聞こえるとき)）。
- **推論デバイス**: GPU（DirectML）は**追加パック**
  （`OBS-Noise-Suppressor-<版>-gpu-pack.zip`。`tools\make_package.ps1 -GpuPack`
  で作る）を入れたときだけ選べます。**実測では GPU は CPU より遅く**（このマシン /
  ステレオ / 予算 10 ms で `dp2` 2.51 → 4.66 ms、`dp8` 6.68 → **12.73 ms**）、
  1 ホップが小さいため DirectML の dispatch / sync の固定費が計算より大きく
  なります。CPU 使用率そのものは下がるので、CPU を他の作業が埋めているときの
  逃げ道です。出力は CPU とビット一致しません（実機の比較で SNR 66 dB、聴感上は
  同じ）。追加パックが無いときは選択肢が無効になります。
- **VU 基準値**: 業務用の VU は「0 VU」が何 dBFS かが機器ごとに違います。日本の
  放送基準は −20 dBFS、EBU は −18 dBFS、ラウドな素材向けは −14 dBFS です。
  ここを変えると **VU の目盛り全体（−20 〜 +3 VU）が対応する dBFS ごと
  平行移動**します（針の位置も一緒に変わります）。

プロパティの説明欄には、現在の状態（サンプルレート / チャンネル数 / 遅延 / モデル名）が
そのまま出ます。困ったときはまずここを見てください。

### パネル（自前 Win32 UI）

```text
+-----------------------------------------------------------------------+
| OBS Noise Suppressor                 MEMORY ① ② ③ ④ ⑤ ⑥ ⑦ ⑧  [SAVE]  |
| NEURAL DE-NOISE · DE-REVERB · 48 kHz                                  |
+---------------+----------------+--------------------------------------+
| IN / OUT      | CONTROL        | EFFECT                               |
|  [ IN VU 針 ] | AMBIENCE       | GR            L    M    H            |
|               | −22.0 dB       |  0  ▤         ▮    ▮    ▮            |
|  −20.5 dBFS   | 62 %           | −12  ▤        ▮    ▮    ▮            |
|  −17.8 PEAK   | [MUTE] [BYP] ┃ | −18  ▤        ▮    ▮    ▮            |
|  [OUT VU 針 ] | VOICE          | −30  ▤        ▮    ▮    ▮            |
|               | +3.5 dB        |                                      |
|  −32.5 dBFS   | 65 %           | −11.6 dB                             |
|  −29.4 PEAK   | [MUTE] [BYP] ┃ |                                      |
|               | REVERB         |                                      |
|               | 28 %           |                                      |
|               | −11.1 dB       |                                      |
|               | [MUTE] [BYP] ┃ |                                      |
+---------------+----------------+--------------------------------------+
| ● 2 ch · 48 kHz · 遅延 2880 samples (60 ms) · dereverb あり  MODEL DP8  [BYPASS] |
+-----------------------------------------------------------------------+
```

上の絵は 48 kHz のときのものです。OBS のレートが違うときは、見出しと最下段が
`96 kHz` のようになり、遅延に変換ぶんが足されます
（例: `2 ch · 96 kHz · 遅延 2880 samples (60 ms +SRC 2.4 ms) · …`）。

- **IN / OUT**: 入力（加工前）と出力（加工後）の**アナログ VU メーター**（針 + VU 目盛り）。
  VU は平均値メーターなので 300 ms 程度の緩やかな動きです（実機準拠、行き過ぎなし）。
  目盛りは −20 〜 +3 VU で、**0 VU = −18 dBFS（既定）** を境に上側が赤いオーバー
  ロード帯になり、**「0」「+3」の数字も赤**になります（実機の VU 盤面と同じ）。
  盤面には **2 本目の細い弧（50 / 100 % の校正目盛り）**も入っています。
  右の表示窓は**名前（IN / OUT）**、**現在値 dBFS**、**ピークホールド dBFS**
  （1.5 s 保持してから 20 dB/s で落ちます）です。
  窓を**ダブルクリックするとピークを消せます**。
  **ステレオは L / R の平均を 1 本の針**にまとめます（モノラルはそのまま）。
  抑圧が掛かっていると OUT の針が IN より左に落ちるので、効きが目で分かります。
- **CONTROL**: **縦フェーダー 3 本**（AMBIENCE / VOICE / REVERB）。
  クリックした位置へ飛び、そのままドラッグで動きます。ホイールでも ±2 % ずつ
  変えられ、**ダブルクリックで既定値に戻ります**。
  VOICE は 0 dB が中央の両極性フェーダーです。
  各フェーダーの上には **dB と % の併記**を出します
  （AMBIENCE = 抑圧量 dB と % / VOICE = ±dB と % / REVERB = % と残響レベル dB）。
  無効なフェーダー（1 出力モデルでの REVERB）はつまみごと暗くなります。
  各フェーダーの下の **MUTE / BYP キー**は「そのフェーダーだけ切る」スイッチです
  （後述）。
- **EFFECT**: **GR メーター**（コンプレッサーの GR と同じく、0 の線から下へ
  伸びる表示、右に現在値 dB）と、出力の **3 バンド（L / M / H）** バー
  （−60 〜 0 dBFS を 3 dB 刻みのセグメント、上に白いピークホールド線）。
  GR は「入力レベル − 出力レベル」を dB で見た値（0 〜 −30 dB）で、
  ノイズ抑圧がどれだけ掛かっているかを示します（掛かりは即時、戻りは緩やか）。
- ヘッダ右の **MEMORY**（プリセット。後述）と、フッタ右の **モデル刻印**
- 左下にステータスドット + 2 行の状態表示（1 行目 = 操作の通知 / 2 行目 = 現在の状態）
- 右下の **BYPASS** ボタン

**フェーダーごとの MUTE / BYP**（**つまみの位置はそのまま**で効きだけを差し替えます）:

| キー | AMBIENCE | VOICE | REVERB |
| --- | --- | --- | --- |
| **MUTE** | 0 %（暗騒音を残さない = 最大抑圧） | −12 dB（presence を最小） | 0 %（残響を残さない） |
| **BYP** | 100 %（暗騒音をそのまま） | 0 dB（掛けない） | 100 %（残響をそのまま） |

- 同じキーをもう一度押すと**有効**（つまみの値）へ戻ります。**MUTE と BYP は排他**で、
  どちらか片方だけが点きます（3 状態のスイッチ）。LED は MUTE = 赤、BYP = アンバー。
- 切っている間は**そのフェーダーの値表示が暗く**なります（つまみの位置と保存値は
  変わりません。切ったまま動かして、戻したときに効かせる使い方ができます）。
- 押すと左下に何が起きたかを 1 行出します
  （例: `VOICE: バイパス — presence を掛けません (0 dB)`）。
- **BYPASS（フッタ右）とは別物**です。BYPASS は OBS のフィルタ有効 / 無効（全体）、
  MUTE / BYP はそのフェーダー 1 本だけの操作です。
- 設定はフェーダーの値と一緒に**プリセットとシーンコレクションへ保存**されます
  （プロパティの「〜の扱い」からも同じものを選べます）。

**見た目（質感）**: 実機の業務用機器を参考にしています。

- 面板 = 黒アルマイト（縦のヘアライン + ざらつき）、面取りした縁、**四隅は十字の
  ネジ**、セクション（IN / OUT / CONTROL / EFFECT）の境目は**彫り込みの溝**。
- VU 盤面 = ランプで後ろから照らした**暖色のアイボリー**。上ほどアンバーに灯り、
  ガラスの斜めの反射と枠の奥行きが入ります。目盛りの弧は太い黒、針は細い黒で
  先端だけ赤、支点には黒いカバー。
- フェーダー = スリットの両側に**金属のレール**と目盛り、つまみは金属キャップ。

（描画はすべて自前の GDI + DIB で、ピクセル単位に書いています。画像素材や
テーマファイルは使っていません。）

**Bypass は OBS 側の「フィルタを有効化」にそのまま対応します**（別物のバイパスを
持たないので、OBS のフィルタ一覧のチェックボックスと常に同期します）。
バイパス中は**完全な 0 遅延の素通し**です（遅延は維持しません。VST3 版と同じ）。

### メモリー（プリセット 1 〜 8）

ヘッダ右の番号ボタンがスロットです。

- **呼び出し**: 光っているボタンを押すだけ（保存されていると LED が点きます）
- **保存**: `SAVE` を押すと待機状態（`SAVE` が赤く光ります）→ 保存先の番号を押す
- **消去**: `Ctrl` を押しながら番号を押す
- キーボードでも操作できます（パネルにフォーカスがあるとき）。
  `S` = SAVE 待機 / `R` = 取り消し / `Esc` = パネルを閉じる
- 保存先は `%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\presets.json`
  です（アンインストーラーでも消しません。消したいときは手で削除してください）
- 保存されるのは**音に関わる値だけ**です（Ambience / Voice / Voice Reverb と
  それぞれのミュート / バイパス / モデル）。VU 基準値は見た目の設定なので含めません
- モデルを切り替えるプリセットを呼ぶと、エンジンを作り直すため数十 ms 無音になります

### 遅延について

内容遅延は **2880 サンプル（48 kHz で 60 ms）** で固定です。OBS のレートが
48 kHz 以外のときは、入出力のレート変換ぶん（約 2〜5 ms）がこれに加算されます
（例: 96 kHz で 62.4 ms）。
OBS には音声フィルタ用の汎用遅延補正がないため、同じシーンでフィルタ無しの
ソースと混ぜると 60 ms ずれて聞こえます。マイク 1 本を通すだけの使い方
（自分の声を直接モニターしない）なら実害はありません。

### CPU 負荷の目安（音が途切れる / 遅れて聞こえるとき）

DPDFNet の推論は **1 ホップ = 480 サンプル（48 kHz で 10 ms）ごと**に走ります。
この 10 ms に間に合わなくなると、OBS の音声スレッドが詰まって**音が途切れ**、
見かけ上**音が遅れて**聞こえます（プラグイン自身が遅延を溜め込むことはありません。
内容遅延は上記のとおり常に 2880 サンプルです）。

実測値（ステレオ、1 ホップ = 10 ms の予算に対して。Ryzen 9 9950X3D の実機検証時）:

| モデル | 1 ホップの処理時間 | 予算に対する割合 |
| --- | --- | --- |
| 高品質 `dpdfnet8`（自動で選ばれる方） | 約 7 – 8.6 ms（オフライン計測は 6.5 – 7.1 ms） | **70 – 86 %** |
| 軽量 `dpdfnet2` | 約 2.1 ms | 約 21 % |

高品質モデルは**速い CPU のコア 1 本をほぼ使い切る**ので、次の心当たりがある
ときは **プロパティの「モデル」を「軽量 (dp2)」** にしてください（負荷が約 1/3）。

- ノート PC をバッテリーで使っている（電源プランを「高パフォーマンス」に）
- 映像エンコードを CPU（x264）で行っている、他のプラグインも重い
- CPU の世代が古い / コア数が少ない

この判定はプラグイン側でも見ていて、次のことをします（ログにも出ます）。

- **起動時（音を出す前）**: フィルタを作ったときに、実際に何ホップか回して
  1 ホップの平均を実測します。予算の **75 %** を超えていれば **最初から軽量
  モデルで開始**します（`起動時の実測が予算に収まらないため軽量モデルで開始します:
  12.4 ms / 予算 10.00 ms (124 %)`）。パネルの表示は、後述の監視スレッドが
  落としたときと同じ `model dp2 (負荷により自動切替)` です。実測はマシンが
  空いているときに取るので実運用より小さく出る（実測 dp8 5.5 ms ↔ 実運用
  6.7 ms = 約 1.2 倍）ため、実行中の警告水準 (95 %) より低い 75 % で判定します。
  「モデル」を明示指定しているとき（自動以外）と `DPDFNET_MODEL` で固定して
  いるとき（検証時）は何もしません。
- 起動から約 1 秒後（1 回だけ）: `音声処理の負荷: 1 ホップ平均 7.25 ms / 予算 10.00 ms (73 %)、最大 8.80 ms`
- 間に合っていないとき（平均が予算の **95 %** 超、または予算超過のホップが
  **25 %** 以上）: `音声処理がリアルタイムに間に合っていません (…)` を
  60 秒に 1 回まで WARNING で
- **間に合っていない状態が 3 秒以上続いたとき**（平均が予算 **100 %** 超、または
  超過 **50 %** 以上。かつ「モデル」が**自動**で、軽量モデルが同梱されている
  とき）: 1 度だけ**自動で軽量モデルへ切り替え**、その旨を WARNING とパネル
  最下段（`model dp2 (負荷により自動切替)`）に出します。設定は書き換えないので、
  プロパティでモデルを**選び直す**と元の指定に戻ります（高品質や軽量を**明示
  指定**しているときは切り替えません。警告だけ出します）。これは**起動時には
  間に合っていたのに、あとから負荷が上がった**場合の保険です。

単発で 10 ms を少し超えるホップ（実測で速い CPU でも 5 % 前後ある）は、
OBS が 21 ms ぶんをまとめて処理するので吸収されます。だからしきい値は
「**平均**が予算を使い切っているか」を主に見ています。

### ログ（エラー / 警告 / 起動情報）

不具合を後から追えるように、プラグイン専用のログファイルを**常に**書きます
（設定はありません）。OBS 標準のログ（`%APPDATA%\obs-studio\logs\*.txt`）にも
同じ内容が `[obs-noise-suppressor]` 付きで出ます。

```text
%APPDATA%\obs-studio\plugin_config\obs_noise_suppressor\obs_noise_suppressor.log
```

実際の中身（このマシンで OBS を **96 kHz** にして 25 秒流した例。v0.3.0 の実測。
パネルを開いたときの行は省略）:

```text
2026-10-18 19:56:55.078 [INFO] OBS Noise Suppressor v0.3.0 を読み込みました
2026-10-18 19:56:55.078 [INFO] module loaded (libobs api 0x20020002, obs_source_info 424 bytes, ort api 17)
2026-10-18 19:56:55.078 [INFO] dll: C:\ProgramData\obs-studio\plugins\obs_noise_suppressor\bin\64bit
2026-10-18 19:56:55.079 [INFO] log file: C:\Users\<user>\AppData\Roaming\obs-studio/plugin_config/obs_noise_suppressor/obs_noise_suppressor.log
2026-10-18 19:56:55.183 [INFO] filter created (ambience 0.50, voice 0.00 dB, reverb 0.00)
2026-10-18 19:56:55.213 [INFO] onnxruntime 1.31.0 (…\plugins\obs_noise_suppressor\data\dpdfnet_onnxruntime.dll) を読み込みました
2026-10-18 19:56:55.514 [INFO] status: 96 kHz / ch=2 / latency 2880 samples (60 ms) / dpdfnet8_48khz_hr_2out.onnx | n_fft=960 hop=480 state=90228 outputs=2
2026-10-18 19:56:56.520 [INFO] 音声処理の負荷: 1 ホップ平均 7.25 ms / 予算 10.00 ms (73 %)、最大 8.80 ms
2026-10-18 19:57:21.646 [INFO] filter destroyed
2026-10-18 19:57:22.312 [INFO] module unloaded
```

| 項目 | 内容 |
| --- | --- |
| 記録するレベル | `ERROR` / `WARNING` / `INFO`。`LOG_DEBUG` は OBS 標準ログにだけ出します |
| 起動情報 | バージョン / libobs API / ONNX Runtime の版とパス / 読み込んだ DLL / 使ったモデル / サンプルレート / 遅延 |
| 負荷 | 音声が流れ始めて約 1 秒後に 1 回だけ「1 ホップの平均 / 予算」。間に合っていないときは WARNING（60 秒に 1 回まで）、自動で軽量へ落としたときはその旨 |
| 世代管理 | 1 MB を超えていたら、開くときに `obs_noise_suppressor.log.1` へ退避（残すのは最新 2 つ） |
| 同じ警告の連続 | 間引きます（2 / 4 / 8 … 回目だけ記録）。毎フレーム同じ警告が出ても溢れません |
| 書けないとき | 音声処理は止めません（そのままパススルーで動きます） |
| ON/OFF | 常時 ON（プロパティに設定はありません） |

ただのテキストなので、OBS を起動していなくてもメモ帳で開けます。不具合を
報告するときは、このファイル（または OBS 標準ログ）を添えてください。

## ファイル構成

```text
obs_plugin/
  build.ps1 / install.ps1        ビルドとインストール（開発時）
  uninstall.ps1                  アンインストーラー
  installer/
    install.bat / uninstall.bat  ダブルクリック用ランチャー（ASCII のみ）
    install-gpu.bat / .ps1       追加パック（GPU / DirectML）のランチャーと中身
  vendor/
    dpdfnet_onnxruntime.dll      同梱する ONNX Runtime（MIT）
    dpdfnet_onnxruntime_dml.dll  GPU 用（追加パックが使う。本体には入れない）
    DirectML.dll / onnxruntime_providers_shared.dll   同じく GPU 用
  src/
    lib.rs                     obs_module_load など OBS モジュールのエクスポート
    ffi.rs                     手書きの libobs FFI（bindgen なし）
    filter.rs                  obs_source_info コールバック本体 / スレッドモデル
    ui.rs                      自前 Win32 パネル（GDI で全部描く）+ スナップショットテスト
    meter.rs                   針 / ピークホールド / GR の弾道（実機 VU 準拠）
    preset.rs                  メモリー 1 〜 8 のデータ層（依存なしの自前 JSON）
    log.rs                     専用ログファイル（エラー / 警告 / 起動情報。世代退避つき）
    load.rs                    リアルタイムに間に合っているかの判定（純関数 + テスト）
    paths.rs                   自分の DLL / ORT / モデルの探索
    dump.rs                    開発用: 実機の入出力を WAV に落とす（環境変数で有効化）
    dsp/
      engine.rs                ホップスケジューラ + メーター + パラメータ適用
      net.rs                   ONNX Runtime セッション（IoBinding、事前確保バッファ）
      ort.rs                   同梱 ONNX Runtime の動的ロード
      stft.rs                  Vorbis 窓の STFT（完全再構成）
      resample.rs              48 kHz 以外のための有理比ポリフェーズ変換（内部 48 kHz）
      meta.rs                  モデルメタデータの検証
      params.rs                パラメータ定義
      offline_tests.rs         OBS を起動しない検証（遅延・一致・決定性）
  tools/
    make_package.ps1           配布パッケージ（install.bat 同梱）を作る（-GpuPack で GPU 追加パック）
    make_verify_collection.py  実機検証用のシーンコレクション生成
    run_obs_verify.ps1         実機で回して数値で確認するハーネス（-SampleRate で 48 kHz 以外も）
    compare_dumps.py           実機ダンプ 2 つ（CPU 実行 / GPU 実行など）を突き合わせる
    gen_obs_lib.ps1            vendor\obs.lib を obs.dll から作り直す
```

## 検証

実機での確認手順は [`OBS_VERIFY.md`](OBS_VERIFY.md) を参照してください。
OBS を起動せずに済む DSP の検証は `build.ps1 -Tests` で回せます。

## 旧 VST3 版からの移行

- VST3 版（`clear_re/plugin/`）はもう保守しません。OBS ではこのプラグインを使います。
- 旧 VST3 の同梱 ORT とモデルはそのまま流用できます（探索先に入れてあります）。
  atkAudio の VST3 検索パス設定は不要になります。

## ライセンス / クレジット

本プラグインが利用・同梱しているものは次のとおりです。**同梱物のライセンスは
それぞれの配布元の条項に従います。**

| コンポーネント | 用途 | ライセンス | 配布元 |
| --- | --- | --- | --- |
| DPDFNet ONNX モデル（`models/*.onnx`） | 音声強調の推論 | Apache-2.0 | `Ceva-IP/DPDFNet`（Hugging Face） |
| ONNX Runtime 1.31.0（`dpdfnet_onnxruntime.dll`） | 推論エンジン | MIT | Microsoft |
| OBS Studio / libobs | ホスト。`obs.dll` の C API を利用 | GPL-2.0-or-later | OBS Project |

- モデルを再配布する場合は Apache-2.0 の条件（著作権表示・ライセンス文の同梱、
  変更の明示）を満たしてください。
- OBS 本体は GPL-2.0-or-later です。libobs の API にリンクして配布するプラグインは、
  互換ライセンス（例: GPL-2.0-or-later）を付けるのが安全です。
- 本リポジトリの自作コード（Rust のプラグイン本体・PowerShell / Python の
  ツール類）は **GPL-2.0-or-later**（Copyright (C) 2026 TanceMoyashi）です。全文は
  リポジトリ直下の [`LICENSE`](../../LICENSE) にあります。`install.bat` で入る
  配布パッケージにも `LICENSE` を同梱しています。
- `LICENSE` は GPL-2.0 の正規のテキストだけを置いています。冒頭に著作権表示などの
  前書きを足すと GitHub のライセンス判定（licensee は正規テキストとの語彙一致 98% 以上を
  要求します）が外れて「Other」表示になるため、著作権表示は README 側に書いています。

