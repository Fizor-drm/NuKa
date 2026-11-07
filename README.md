# NuKa

手元の PC だけで完結する翻訳デスクトップアプリ。入力したテキストを、ローカルで動く llama.cpp と [PLaMo 3 Translate](https://huggingface.co/pfnet) モデルが 53 言語で翻訳します。テキストはどこにも送信されません。

- ダウンロード: https://github.com/Fizor-drm/NuKa/releases/latest
- 使い方・ビルド手順の詳細: [desktop/README.md](desktop/README.md)

## 構成

```
desktop/       NuKa 本体 (Tauri 2 + Rust + WebView2)
scripts/       開発用ランタイムの取得スクリプト
runtime/       開発用の llama.cpp と GGUF (git には入れない)
```

ランタイムとモデルは、アプリの設定画面からインストールするのが普通の使い方です。`scripts/fetch-runtime.ps1` は、開発時に `runtime/` を用意するためのものです。

## 開発

```powershell
cd desktop
npm install
npm run tauri:dev     # 開発モードで起動
cargo test            # 単体テスト (desktop/src-tauri で実行)
```

## ライセンス

コードは MIT (`LICENSE`)。モデルの重みは [PLaMo Community License](https://plamo.preferredai.jp/info/plamo-community-license-ja) です。個人利用と、学術利用を含む非商用は登録なしで使えます。売上 10 億円以下の商用は登録フォームの提出が必要で、それを超える場合は別契約です。
