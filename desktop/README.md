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

## 必要条件

- Windows 10/11 (WebView2 ランタイム)
- `runtime\llama\llama-server.exe` (llama.cpp の CUDA ビルド)
- `runtime\models\plamo-3-translate-0.8b-Q4_K_M.gguf`

モデルと `llama-server` はリポジトリに含まれません。リポジトリ直下の `runtime\` を
配置するか、次のいずれかで場所を指定してください。

1. exe と同じ階層、または 1 つ上の階層に `runtime\` を置く
2. 設定ファイル (`%APPDATA%\app.nuka.translate\settings.json`) の
   `model_path` / `server_path` を編集する
3. 環境変数 `PLAMO_MODEL_PATH` / `PLAMO_LLAMA_SERVER` で指定する

優先順位は 設定ファイル、環境変数、exe からの探索の順です。探索は開発ビルドでは
上位 6 階層まで、リリースビルドでは exe の隣と 1 つ上だけです。

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
