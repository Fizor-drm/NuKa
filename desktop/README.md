# NuKa

NuKa は、手元の PC だけで動くデスクトップ翻訳アプリです。入力した文章を 53 言語のあいだで
翻訳し、原文と訳文は外部サービスへ送信しません。翻訳モデルには
[PLaMo 3 Translate](https://huggingface.co/pfnet/plamo-3-translate-0.8b)
(PFN のオープンウェイトモデル) を使っています。

実行時は llama.cpp の `llama-server` を子プロセスとして起動し、ループバック
(127.0.0.1) の HTTP でやり取りします。モデルの読み込みが終わるまで UI は
「モデルを読み込んでいます」を表示します。

## 特長

- ローカル完結。原文と訳文が PC の外に出ません。
- 53 言語 (簡体字と繁体字は別扱い)。
- 日本語向けの文体指定 (常体 / 敬体)、用語集、自動翻訳。
- ライト / ダーク / システムのテーマ切り替え。
- ランタイムとモデルを設定画面からダウンロード (アプリ本体は小容量)。
- アプリ内アップデーター (GitHub Releases の最新版を確認して更新)。

## インストール (配布版)

配布パッケージ (`NuKa_1.1.0_x64-setup.exe`) を実行すると、アプリ本体だけが
インストールされます。ランタイムとモデルは初回起動後に設定画面から取得します。

1. NuKa を起動し、右上の歯車 (設定) を開きます。
2. 「ランタイム」の「インストール」を押します。llama.cpp b12136 (CUDA 12.4) の
   2 つの zip (合計 約 650 MB) をダウンロードして展開します。
3. 「モデル」から使いたいサイズと量子化を選んでインストールします。
   インストール先は「既定の場所」または「別の場所」から選べます。
4. モデルをインストールすると自動的に使用するモデルになり、エンジンが起動します。

モデルのライセンスは PLaMo Community License です。個人利用と非商用は無償で、
商用利用には登録が必要です。設定画面の「ライセンス」から案内ページを開けます。

ダウンロードは設定画面からいつでも取消できます。失敗・キャンセル時は `.part` と
ダウンロードした zip を残しません。

## アップデート

右上のダウンロードアイコンから GitHub の最新リリースを確認できます。更新があると
アイコンに印が付きます。確認後は次の流れで更新します。

1. 「v1.x.x に更新できます」を確認して「更新する」を押します。
2. インストーラー (`NuKa_*_x64-setup.exe`、約 3 MB) をダウンロードし、SHA-256 を
   GitHub の digest と照合します。
3. ダウンロード後にインストーラーを起動し、NuKa は終了します。ウィザードに従って
   インストールしてください。

起動時の自動確認は既定で ON です。確認は起動時に GitHub へ 1 回アクセスするだけで、
IP アドレスは GitHub に伝わります。設定画面の「起動時にアップデートを確認」で
OFF にできます。確認・ダウンロードは HTTPS のみで、取得先は
`https://github.com/Fizor-drm/NuKa/releases/download/` に限定しています。

## 必要条件

- Windows 10/11 (WebView2 ランタイム)
- ランタイム: 設定画面からインストール (llama.cpp b12136 / CUDA 12.4)
- モデル: 設定画面からインストール (PFN 公式の GGUF)

手動で配置する場合:

1. exe と同じ階層、または 1 つ上の階層に `runtime\llama\llama-server.exe` と
   `runtime\models\plamo-3-translate-0.8b-Q4_K_M.gguf` を置く
   (探索はこの固定ファイル名だけを見ます。別名の GGUF は 2 か 3 の方法で指定してください)
2. 設定ファイル (`%APPDATA%\app.nuka.translate\settings.json`) の
   `model_path` / `server_path` を編集する
3. 環境変数 `PLAMO_MODEL_PATH` / `PLAMO_LLAMA_SERVER` で指定する

解決の優先順位:

- モデル: 設定画面で管理しているモデル (使用中) → `model_path` → 環境変数 → exe の探索
- ランタイム: 設定画面で管理しているランタイム → `server_path` → 既定の保存先 →
  環境変数 → exe の探索

探索は開発ビルドでは上位 6 階層まで、リリースビルドでは exe の隣と 1 つ上だけです。

## ランタイムの検証

設定画面からのランタイム導入では、次の 2 つの zip を
[ggml-org/llama.cpp の b12136 リリース](https://github.com/ggml-org/llama.cpp/releases/tag/b12136)
から HTTPS で取得し、**サイズと SHA-256 を固定値と照合してから** 展開します。
一致しない場合はダウンロードファイルを破棄して失敗します。

| ファイル | サイズ (バイト) | SHA-256 |
| --- | ---: | --- |
| `llama-b12136-bin-win-cuda-12.4-x64.zip` | 288,912,954 | `A7F383874199086EB6AE88344A5CBFAD9204AE02D6A5FD6D1C0A84298BA94CC0` |
| `cudart-llama-bin-win-cuda-12.4-x64.zip` | 391,443,627 | `8C79A9B226DE4B3CACFD1F83D24F962D0773BE79F1E7B75C6AF4DED7E32AE1D6` |

公式リリースはチェックサムを公表していないため、公式 b12136 から取得した
検証済み zip をもとに固定しています。展開は `runtime\llama.staging` に対して行い、
必須ファイルの検証が通ってから `runtime\llama` と入れ替えます。

## 保存先

- 設定: `%APPDATA%\app.nuka.translate\settings.json`
- ログ: `%LOCALAPPDATA%\app.nuka.translate\logs\llama-server.log`
- ランタイム (既定): `%LOCALAPPDATA%\app.nuka.translate\runtime\llama\`
- モデル (既定): `%LOCALAPPDATA%\app.nuka.translate\models\`

## 開発

```powershell
npm install
npm run tauri dev
```

`npm run tauri dev` は Vite の開発サーバー (http://localhost:1420) とアプリを
同時に起動します。Rust とフロントエンドの両方を変更したときは、このコマンドで
確認してください。Rust だけを変えたときは `npm run build` を挟まずに
`cargo build` でも構いませんが、画面は Vite 経由で読み込まれます。

## テスト

```powershell
# Rust の単体テスト (desktop/src-tauri)
cargo test

# 型チェックとフロントエンドのビルド (desktop)
npx tsc --noEmit
npm run build
```

## 単体で起動するビルド

```powershell
npm run build
cargo build --features tauri/custom-protocol
```

Tauri CLI を使う場合:

```powershell
npx tauri build --no-bundle
```

リリースビルドの exe は `desktop/src-tauri/target/release/nuka.exe` にできます。

注意:

- 素の `cargo build` は Tauri の開発モードになり、Vite (http://localhost:1420) が
  動いていないと画面が表示されません。配布用には
  `--features tauri/custom-protocol` を付けてください。
- フロントエンドを変更したあと `cargo build` だけではアセットが再埋め込みされない
  ことがあります。`cargo clean -p nuka` してからビルドしてください。

## 成果物の配置要件

- リリースビルドの exe は、`runtime\` と同じ階層、または 1 つ上に `runtime\` がある
  配置にしてください (リリースでは探索が浅いため)。
- それ以外の場所に置く場合は、`settings.json` か環境変数で `llama-server` と
  モデルのパスを指定します。
- 設定ファイルとログの保存先:
  - 設定: `%APPDATA%\app.nuka.translate\settings.json`
  - ログ: `%LOCALAPPDATA%\app.nuka.translate\logs\llama-server.log`

## ライセンス

- 本アプリのライセンスは、リポジトリ直下の `LICENSE` を参照してください。
- モデルと llama.cpp はリポジトリに含まれません。それぞれの配布元のライセンスに
  従います。
  - PLaMo 3 Translate (PFN): https://huggingface.co/pfnet/plamo-3-translate-0.8b
  - llama.cpp: https://github.com/ggml-org/llama.cpp
- デスクトップアプリは Tauri (MIT または Apache-2.0) の上に実装されています。
