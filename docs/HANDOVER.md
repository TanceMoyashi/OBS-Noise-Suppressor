# 引き継ぎ資料（Antigravity 向け）

このリポジトリの開発・配布・GitHub 管理を引き継ぐための資料です。
最終更新: 2026-11-01 / 対象: `main` = `00b3968`（v0.3.2 リリース済み）

---

## 0. これは何か

**OBS Studio 用のノイズ抑制 + 残響除去フィルタ（Windows x64）** と、同じ DSP を使う
**VST3 プラグイン（DAW 用）** の 2 製品を 1 リポジトリで管理しています。

- 言語: Rust（DSP / プラグイン）、Python（モデルの学習・検証）
- 推論: ONNX Runtime（同梱。追加ダウンロード不要）
- モデル: DPDFNet（Apache-2.0）の 48 kHz 版を 3 段（軽量 / 中間 / 品質）同梱
- 遅延: 48 kHz で **2880 サンプル = 60 ms 固定**（内容遅延。プラグインが遅延を溜めない）
- ライセンス: プラグイン本体は **GPL-2.0**、モデルは **Apache-2.0**（別ファイルで同梱）

## 1. 現在の状態（2026-11-01）

| 項目 | 値 |
| --- | --- |
| ブランチ | `main`（既定）/ `00b3968` |
| バージョン | 4 クレートとも `0.3.2` |
| 最新リリース | **v0.3.2**（Latest、アセット 3 本） |
| テスト | **obs_plugin 18 / dpdfnet 46 / panel_ui 32 / vst3 9 = 105**、警告 0 |
| 既定モデル | **dp4（中間）**。重い環境では自動で dp2（軽量）へ退避 |
| 未解決 Issue / PR | なし |

配布物（`dist/` と GitHub Releases の両方にある）:

| ファイル | サイズ | SHA256 |
| --- | ---: | --- |
| `OBS-Noise-Suppressor-0.3.2-win64.zip` | 64,569,098 B | `9db3f302299a2009280ff47a1e0eae23f7302a75ed6a461a7864de0c1b03f416` |
| `DPDFNet-Voice-0.3.2-vst3.zip` | 41,584,870 B | `d7b5c56b1330873f05816b328c270edc1c646e6a404e69b11221bcb0f0ad24b7` |
| `OBS-Noise-Suppressor-0.3.2-gpu-pack.zip` | 15,543,817 B | `d7883207275ed00f147a6e9e46e8ff5018efa77b0469a6efa09ca8191f5bf17d` |

> ハッシュは **ZIP を作り直すと変わります**（中身が同じでも時刻で変わる）。リリースに
> 添付したものと一致するかは `certutil -hashfile <file> SHA256` で確認してください。

## 2. 構成

```
D:\Users\Syntac\Documents\OBS-Noise-Suppressor
├─ clear_re\
│  ├─ dpdfnet\        DSP 共通コア（モデル/ORT 探索、推論、負荷監視、キャリブレーション）
│  ├─ panel_ui\       OBS 版と VST3 版で共有する Win32 パネル（描画・操作・プリセット）
│  ├─ obs_plugin\     OBS ネイティブプラグイン（cdylib）
│  ├─ vst3\           VST3 プラグイン（nih-plug）
│  └─ plugin\         モデル本体と学習・検証ツール（製品コードではない）
├─ dist\              ビルド済み配布物（.gitignore 対象）
├─ docs\              画像・この資料
└─ .github\           Issue テンプレート
```

### クレートの責務（ここを混ぜない）

| クレート | 役割 | 触るときの注意 |
| --- | --- | --- |
| `dpdfnet` | ホスト非依存の DSP。**OBS 版と VST3 版が共有** | 片方だけ直すと 2 製品がずれる。必ず両方のテストを回す |
| `panel_ui` | パネルの描画・状態・プリセット | OBS 版の `ui.rs` は薄いアダプタ。ロジックはここ |
| `obs_plugin` | OBS のフィルタとしての結線、設定保存、ログ | `obs_log!` 経由で OBS 標準ログに出す |
| `vst3` | VST3 のパラメータ、`Editor`、オフライン検証 | class ID は変更禁止（下記） |
| `plugin` | モデル・学習ツール・実測記録 | 製品のビルドには不要。`models\` だけが配布物の元 |

## 3. 壊してはいけない契約（技術）

1. **遅延**: 48 kHz で `2880` サンプル（60 ms）。他のレートは有理比ポリフェーズ SRC を挟み、
   遅延は決定的（例: 96 kHz = `5988`）。**この値を変えると OBS の音ズレ報告が再発する。**
2. **ONNX の I/O 契約**（モデルを差し替えるときも不変）:
   - 入力 `spec`: float32 `[1, 1, 481, 2]`（`n_fft=960` の rfft、実部/虚部）
   - 入力 `state_in`: float32 `[state_size]`（モデルごとに違う: dp2 56436 / dp4 67700 / dp8 90228）
   - 出力 `spec_e`: `[1, 1, 481, 2]`、`state_out`: `[state_size]`、2 出力版は `spec_ns` も
   - メタデータ `n_fft` / `hop_length` / `window_length` / `window_type=vorbis` /
     `state_size` / `erb_norm_state_size` / `spec_norm_state_size` / 正規化の初期値
   - 1 ホップ = 480 サンプル
3. **VST3 の識別子**: name `DPDFNet Voice` / vendor `TanceMoyashi` /
   class ID `*b"DPDFNetVoiceVST3"`。**変更するとホスト側のプロジェクトが壊れる。**
4. **負荷のしきい値**（`clear_re/dpdfnet/src/load.rs`）:
   - 警告: 平均が予算の 95 % 超、または予算超過ホップが 25 % 以上（60 秒に 1 回）
   - 自動フォールバック: 平均 100 % 超、または超過 50 % 以上が 3 秒連続（1 フィルタ 1 回）
   - 起動時キャリブレーション: `0.75`（実行中の警告より厳しい）を超えたら dp2 で開始
   - 落とし先は常に **dp2**。しきい値を変えるときは `OBS_VERIFY.md` の記録も更新する
5. **`LICENSE` は GPL-2.0 の正規テキストのみ**。GitHub の判定は語彙一致 98 % 以上で、
   前書きを足すと `NOASSERTION` に落ちる（実測 86.6 %）。著作権表示は README 側に書く。
6. **README の AI 著者開示**（DeepSeek V4.1 Flash Free）は**維持**する。

## 4. 開発の回し方

```powershell
# OBS 版（Release ビルド + 全テスト）
cd D:\Users\Syntac\Documents\OBS-Noise-Suppressor\clear_re\obs_plugin
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\build.ps1 -Configuration Release -Tests

# VST3 版
cd D:\Users\Syntac\Documents\OBS-Noise-Suppressor\clear_re\vst3
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\build.ps1 -Tests
```

- **警告 0 でないと失敗扱い**。`NativeCommandError` の表示は PowerShell の見た目の問題なので
  `$LASTEXITCODE` を見る
- 期待値: `obs_plugin 18 / dpdfnet 46 / panel_ui 32` と `vst3 9`（合計 105）
- テスト内の `error:` / `warning:` ログ行は**意図的な出力**。コンパイラ警告ではない
- オフライン DSP 検証（モデルを差し替えたとき）:
  ```powershell
  $env:DPDFNET_ORT = "<repo>\clear_re\obs_plugin\vendor"
  $env:DPDFNET_MODEL = "<repo>\clear_re\plugin\models\dpdfnet4_48khz_hr_2out.onnx"
  cd <repo>\clear_re\dpdfnet
  & "$env:USERPROFILE\.cargo\bin\cargo.exe" test --release --lib offline -- --nocapture
  ```
- 実機確認（**管理者権限が必要**。OBS を前面で起動する）:
  ```powershell
  cd <repo>\clear_re\obs_plugin
  .\tools\run_obs_verify.ps1 -WorkDir <空の作業ディレクトリ> -Model auto
  ```
  期待値: `lag = 2880 samples`、`参照 vs 実機: max diff = 0e0`、終了コード 0

## 5. モデル

| モデル | ファイル | 速度（ステレオ 1 ホップ） | 位置づけ |
| --- | --- | ---: | --- |
| 軽量 dp2 | `dpdfnet2_48khz_hr_2out.onnx` | 1.74 ms | 非力な PC / フォールバック先 |
| **中間 dp4（既定）** | `dpdfnet4_48khz_hr_2out.onnx` | **2.62 ms** | 既定。実機 平均 3.61 ms（予算の 36 %） |
| 品質 dp8 | `dpdfnet8_48khz_hr_2out.onnx` | 4.77 ms | 品質最優先 |

品質（LibriSpeech test-clean × MUSAN eval split、32 条件の平均）:

| モデル | SI-SDR | STOI | PESQ-WB |
| --- | ---: | ---: | ---: |
| dp2 | 2.06 dB | 0.944 | 2.49 |
| **dp4** | **3.89 dB** | 0.944 | **2.59** |
| dp8 | 2.15 dB | 0.948 | 2.53 |

- dp4 は上流 DPDFNet の `dprnn_num_blocks=4`。**上流の定義そのもの**で、追加の部品はない
- 学習・書き出し・評価の全手順と実測記録: `clear_re/plugin/MODEL_TRAINING.md`
- 学習済み重み・データ・中間生成物は `clear_re/plugin/build/`（**gitignore**）。
  再現するには `MODEL_TRAINING.md` §8 の順に実行（torch + CUDA が必要）
- ライセンス: モデルは Apache-2.0（`clear_re/plugin/models/LICENSE-DPDFNet.txt` /
  `NOTICE-DPDFNet.txt`）。学習データは LibriSpeech（CC BY 4.0）と MUSAN（CC BY 4.0）

## 6. リリース手順

1. 4 つの `Cargo.toml` の `version` を揃える（`dpdfnet` / `panel_ui` / `obs_plugin` / `vst3`）
2. テストを緑にする（§4）
3. 実機確認を回す（§4。`-Model auto` と、必要なら `-Model dp4`）
4. パッケージを作る:
   ```powershell
   cd <repo>\clear_re\obs_plugin
   .\tools\make_package.ps1            # OBS-Noise-Suppressor-<版>-win64.zip
   .\tools\make_package.ps1 -GpuPack   # OBS-Noise-Suppressor-<版>-gpu-pack.zip
   cd <repo>\clear_re\vst3
   .\tools\make_package.ps1            # DPDFNet-Voice-<版>-vst3.zip
   ```
5. `dist\` の ZIP の SHA256 を控える（`certutil -hashfile <file> SHA256`）
6. リリースノートを書く（日本語。前版の文体に合わせ、**実測値だけ**を載せる）
7. 公開:
   ```powershell
   git push origin main
   gh release create v0.3.3 --title "<名前>" --notes-file <notes.md> --latest `
       "dist\OBS-Noise-Suppressor-0.3.3-win64.zip" `
       "dist\DPDFNet-Voice-0.3.3-vst3.zip" `
       "dist\OBS-Noise-Suppressor-0.3.3-gpu-pack.zip"
   ```
   - 両製品を含む版は `--latest` を付ける（片方だけの版は `--latest=false`。
     前例: v0.3.1 は VST3 単体だったので `--latest=false`）
   - アセット名は**版番号入り**にする（上書きしない）
8. `gh release view v<版> --json assets --jq '.assets[]|.name'` で確認

## 7. GitHub 管理（重要）

| 項目 | 現在の設定 |
| --- | --- |
| リポジトリ | `TanceMoyashi/OBS-Noise-Suppressor`（**public**） |
| 既定ブランチ | `main` |
| コラボレータ | `TanceMoyashi`（**admin**）のみ |
| Issue | 有効。**誰でも起票できる**（blank issue も許可、テンプレートは bug / feature） |
| Discussions | 有効（質問・雑談の受け皿） |
| Wiki | 無効 |
| ライセンス判定 | `GPL-2.0`（`LICENSE` を正規テキストのまま維持すること） |
| タグ | `v0.1.0-knowngood` / `v0.1.1-knowngood` / `v0.2.0` / `v0.2.1` / `v0.3.0` / `v0.3.1` / `v0.3.2` |

### ブランチ保護（ルールセット）

- 名前: **`main の保護（承認した人だけが変更できる）`**（id `25340845`）
- 対象: 既定ブランチ `main`
- ルール: 削除禁止 / 強制 push 禁止 / **PR 必須**（承認 0 人でよい）
- **バイパス: `RepositoryRole` の admin が常時可**
  → 管理者は `git push origin main` を直接実行できる（現在の運用はこれ）
- つまり「**承認した人（admin）だけが変更できる / それ以外は PR 経由**」という設計。
  不具合報告・機能要望は Issue で誰でも受け付ける

### 認証（ローカル）

- `gh` は `TanceMoyashi` でログイン済み（`gho_` トークン、スコープ `gist` / `read:org` / `repo`）
- **トークンは共有しない**。引き継ぎ先は自分のアカウントで `gh auth login` する
- コミットの識別情報（**このまま維持**）:
  ```
  user.name  = TanceMoyashi
  user.email = 247910044+TanceMoyashi@users.noreply.github.com
  ```

## 8. 引き継ぎの手順（チェックリスト）

### A. GitHub 側

1. 引き継ぎ先のアカウントを決める（個人アカウント or Organization）
2. **方法 1: コラボレータとして admin を付与**（移譲しない）
   ```powershell
   gh api -X PUT repos/TanceMoyashi/OBS-Noise-Suppressor/collaborators/<USER> -f permission=admin
   ```
   → ルールセットのバイパスは「admin ロール」なので、**追加設定なしで直接 push できる**
3. **方法 2: リポジトリを移譲**（Settings → General → Danger Zone → Transfer ownership）
   → ルールセット・タグ・リリース・Issue はそのまま移る。移譲先で
   **ルールセットのバイパス（admin）と Issue の許可設定**を再確認する
4. 必要なら Secrets / Actions の設定（現在はワークフローなし。ビルドは手元で行う）
5. 旧管理者の権限を外す（移譲後）

### B. ローカル（引き継ぎ先）

```powershell
gh auth login                      # 自分のアカウントで
git clone https://github.com/<owner>/OBS-Noise-Suppressor.git
cd OBS-Noise-Suppressor
git fetch --tags
git config user.name  "TanceMoyashi"
git config user.email "247910044+TanceMoyashi@users.noreply.github.com"
```

確認:

1. `gh auth status` に `repo` スコープがある
2. `.\clear_re\obs_plugin\build.ps1 -Configuration Release -Tests` が exit 0（105 テスト）
3. `.\clear_re\vst3\build.ps1 -Tests` が exit 0
4. `gh release list` に v0.3.2 が出る
5. 必要なら実機確認（§4。**管理者権限**と OBS のインストールが必要）

### C. 引き継ぎ時に必ず伝えること

- リポジトリは **public**。`dist/` と `build/` はコミットしない
- 既存の履歴・タグ・リリースは**書き換えない**（過去コミットのメール書き換えは方針として拒否済み）
- 既定モデルは dp4。**品質重視の人は dp8、非力な PC は dp2**（UI から選べる）
- 変更は「テスト緑 + 警告 0」を確認してから push（main は admin バイパスで直接 push できるが、
  外部コントリビュータは PR 経由）

## 9. 既知の制限・残タスク

### 既知の制限

- **DAW 実機での VST3 GUI 表示は未確認**（OBS 実機 + ホスト無しのオフライン検証まで）
- 品質評価のノイズは MUSAN の eval split。**8 kHz 超の帯域と実マイク録音は未評価**
- パネルは固定サイズ（リサイズ不可）。MODEL / DEV はクリックで巡回（ドロップダウンなし）
- プリセット（メモリー）は `%APPDATA%\DPDFNet Voice\presets.json`（プロジェクトとは別）
- モデル / デバイス変更時は一瞬音が途切れる（設計どおり）
- GPU (DirectML) は追加パック。**実測では CPU より遅い**ので既定は CPU
- 内容遅延 60 ms は OBS 側に補正がないため、フィルタ無しのソースと混ぜるとずれて聞こえる

### 残タスク（優先度順）

1. **別 PC の「約 3 秒遅れ + ブツ切れ」の原因確定** — 相手の OBS 標準ログ
   （`%APPDATA%\obs-studio\logs\`）待ち。見る行: `CPU Name:` /
   `Audio buffering started (XXX ms)` / `ended` / `skipped` / `lagged frames`。
   README に「相手に送る文面」あり
2. **8 kHz 超・実マイク録音での品質評価** — 48 kHz の実ノイズ（DNS-Challenge 等）で再評価。
   改善案は `MODEL_TRAINING.md` §8.3 / §8.4
3. **DAW 実機での VST3 GUI 確認**（表示・DPI・子ウィンドウの追従）
4. GPU パックの再評価（DirectML の速度は CPU に劣る。目的は CPU 使用率の削減）

## 10. 運用ルール

- コミットメッセージ・コメント・ドキュメントは**日本語**
- `git add` は**パス指定**（`target/` や `build/` を混ぜない）
- 大きいバイナリ（`.pth` / 学習データ / 中間 ONNX）はコミットしない。
  ただし**配布するモデル**（`clear_re/plugin/models/*.onnx`）は既存の慣例どおりコミットする
- 変更前に必ずテスト。**警告 0** を維持する
- 実測値を書くときは**条件**（サンプルレート・モデル・条件数）を添える。推測を書かない
- `LICENSE` は触らない（正規テキストのみ）
- README の AI 著者開示は残す
