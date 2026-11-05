# PLaMo 3 Translate ページ翻訳

開いているページの本文を、手元の [PLaMo 3 Translate 0.8B](https://huggingface.co/pfnet/plamo-3-translate-0.8b) で訳して差し替える Chrome / Edge 拡張機能です。

0.8B はブラウザの中では動きません。重みは約 0.57GB (Q4_K_M) で、ページの外にある llama.cpp が訳します。拡張機能は `127.0.0.1` のブリッジに文を渡し、返ってきた訳を元の位置へ戻します。ページの文が外へ出ることはありません。

## 構成

```
extension/     Chrome / Edge 拡張機能 (Manifest V3)
bridge/        127.0.0.1:8788。拡張機能と llama-server のあいだ
runtime/       llama.cpp と GGUF。git には入れない
```

訳し方はモデルカードの Completions API 例に合わせています。プロンプトは PFML3 です。

```
<|plamo:tag|>dataset<|plamo:msg|>translation
<|plamo:tag|>input<|plamo:msg|>原文
<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>
```

同じ段落の直前 2 文は履歴として渡します。モデルカードが推奨している、段落ごとの訳語合わせです。ページ全体は履歴にしません。学習時の上限は入出力あわせて 32,768 トークンで、ページ全体を渡すとすぐ超えます。

用語集と、日本語訳の普通体 / 丁寧体もポップアップから渡せます。文体は訳先が日本語のときだけプロンプトに入ります。

## 段落の中にリンクがあるとき

`<p>The <a>dashboard</a> lists every task.</p>` をテキストノードごとに訳すと、語順の違う言語では文が崩れます。そこで段落全体を目印付きで 1 回に送ります。

```
⟦0⟧The ⟦/0⟧⟦1⟧dashboard⟦/1⟧⟦2⟧ lists every task.⟦/2⟧
```

モデルが目印を保っていれば、戻ってきた訳を目印の位置で各ノードへ配ります。リンクも語順も残ります。目印が落ちたときは、その段落だけノードごとの訳に切り替えます。語順は崩れますが、リンクは残り、訳が抜けたままにもなりません。

## 何を訳さないか

- `script`、`style`、`code`、`pre`、`kbd`、`samp`、`var`
- `svg`、`math`、`canvas`、`iframe` などの埋め込み
- `input`、`textarea`、`select`、`option` などの入力欄
- `hidden` 属性、`aria-hidden="true"`、`display: none`、`visibility: hidden`、開いていない `dialog`、`inert`
- `translate="no"` と `.notranslate`
- `white-space: pre` 系で整形されている本文

テキストノードの値だけを書き換え、要素は置き換えません。`<span>` を挿入すると flex / grid の割り付けが変わり、React などが持つノード参照も壊れるためです。

## 準備

この PC (GTX 1660, 6GB) では CUDA 12.4 ビルドを使います。CUDA 13 のビルドは Ada 以降向けなので使いません。

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\fetch-runtime.ps1
powershell -ExecutionPolicy Bypass -File .\start-bridge.ps1
```

`fetch-runtime.ps1` が取るもの:

- llama.cpp b12136 の Windows CUDA 12.4 ビルドと、同梱の CUDA ランタイム
- `pfnet/plamo-3-translate-0.8b-GGUF` の `plamo-3-translate-0.8b-Q4_K_M.gguf`

ダウンロードは展開先の必須 DLL と、配布元の `Content-Length` に照らして確かめます。途中で止まった `.part` は使いません。

ブリッジは `http://127.0.0.1:8788`、llama-server は `http://127.0.0.1:8090` です。`start-bridge.ps1` はモデルの読み込みを最大 5 分待ちます。初回は GTX 1660 で 1 分 20 秒ほどかかりました。2 文目以降は 1 文あたり 0.1〜0.5 秒です。

## 拡張機能の読み込み

1. Chrome か Edge で `chrome://extensions` (Edge は `edge://extensions`) を開く。
2. デベロッパー モードをオンにする。
3. 「パッケージ化されていない拡張機能を読み込む」で `extension/` を選ぶ。
4. 訳したいページを開き、ツールバーの印をクリックする。
5. 訳先を選んで「ページを訳す」。

権限は `activeTab`、`scripting`、`storage` と、`http://127.0.0.1:8788/*` への接続だけです。コンテンツスクリプトは常駐させず、ポップアップを開いて操作したときに、そのタブへだけ注入します。

原文はタブのメモリに残します。ページの属性には書きません。そのため原文に戻せるのは、再読み込みするまでの同じタブの中だけです。ブリッジが止まっていると、ポップアップの先頭にその旨が出ます。

## テスト

```powershell
npm install
npm test
py -3 -m unittest discover -s bridge -p "test_*.py"
```

拡張機能の 17 件は、PFML3 の組み立て、制御トークンの除去、長文の分割、そして jsdom 上で `content.js` を実際に動かす統合テストです。統合テストでは次を確かめます。

- 訳して原文に戻すと、HTML が元と完全に一致する
- リンクを含む段落が 1 リクエストで訳され、`<a>` が残る
- 目印を落とすモデルでは、ノードごとの訳に切り替わる
- `svg`、`math`、`option`、`code`、非表示要素が訳されない
- 二度押しで増殖せず、復元してから再度訳せる
- 失敗した文は原文のまま残り、他の文は訳される
- `body` が無いページでも固まらない

ブリッジの 19 件は、PFML3 の組み立てと、偽の上流に対する HTTP 面の検証です。壊れた JSON、不正な UTF-8、辞書でない `glossary` / `history`、数値でない `max_tokens`、未知の言語コードで 400 を返し、ハンドラを落とさないこと。CORS を拡張機能のオリジンにだけ返すこと。上流がループバックでなければ起動しないことを確かめます。

実モデルでの確認は、ブリッジを起動したあとで:

```powershell
node tools/e2e-bridge.mjs
```

ページを組み立て、実際に llama.cpp で訳し、訳し終えたページと、原文に戻した結果を表示します。

## セキュリティ上の前提

- ブリッジはループバックからの接続だけを受けます。`PLAMO_BRIDGE_HOST` は `127.0.0.1` 以外を受け付けません。
- 上流 (`PLAMO_UPSTREAM`) もループバックでなければ起動しません。ページの本文が外部へ出る経路を作らないためです。
- CORS は `chrome-extension://` のオリジンにだけ返します。
- 原文に `<|plamo:...|>` が混ざっていても、名前を問わず取り除きます。入れ子で書かれても、除去を繰り返して平文だけを残します。取り除いた結果が空になる文は送りません。
- 訳先・原文の言語コードは対応表にあるものだけを受け付けます。

## ライセンス

拡張機能とブリッジのコードは MIT です。`LICENSE` を見てください。

モデルの重みは [PLaMo Community License](https://plamo.preferredai.jp/info/plamo-community-license-ja) です。個人利用と、学術利用を含む非商用は登録なしで使えます。売上 10 億円以下の商用は登録フォームの提出が必要で、それを超える場合は別契約です。詳細はモデルカードの License を見てください。

## 意図的にしていないこと

- ページ内での推論。0.8B の GGUF でも 0.57GB あり、WebGPU 向けの配布物がありません。
- 画像の翻訳。`alt` や `title` のような見えない文字列は、利用者が結果を確かめられないので触りません。
- 動的に追加された文の追従。訳した時点の DOM を対象にします。SPA が後から描画した文は、もう一度「ページを訳す」を押したときに訳します。
- 複数タブの同時翻訳。GPU は 1 つの llama-server を共有するので、進捗も 1 タブずつ扱います。
