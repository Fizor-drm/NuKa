import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { JSDOM } from "jsdom";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "..", "content", "content.js"), "utf8");

function setup(html) {
  const dom = new JSDOM(`<!doctype html><html><body>${html}</body></html>`, {
    url: "https://example.test/",
    runScripts: "outside-only",
  });
  const { window } = dom;
  const listeners = [];
  const posted = [];
  const calls = [];

  window.chrome = {
    runtime: {
      onMessage: { addListener: (fn) => listeners.push(fn) },
      sendMessage: async (message) => {
        if (message.type === "plamo:status") {
          posted.push(message.status);
          return { ok: true };
        }
        if (message.type === "plamo:translate") {
          calls.push(message);
          return { ok: true, translation: translate(message.text) };
        }
        return { ok: true };
      },
    },
    storage: {
      // このテストの偽モデルは英訳なので、訳先も英語にしておく。
      local: { get: (_defaults, callback) => callback({ targetLang: "en" }) },
      onChanged: { addListener: () => {} },
    },
  };

  /** モデルの代わり。目印は保ち、地の文だけ大文字にする。 */
  function translate(text) {
    return text
      .split(/(\u27E6[^\u27E7]*\u27E7)/)
      .map((chunk) => (chunk.startsWith("\u27E6") ? chunk : chunk.toUpperCase()))
      .join("");
  }

  vm.runInContext(source, dom.getInternalVMContext());
  return { window, document: window.document, listeners, posted, calls };
}

function send(listeners, message) {
  return new Promise((resolve) => {
    for (const listener of listeners) {
      listener(message, { id: "test" }, resolve);
    }
  });
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 60));

test("訳して、原文に戻せる。要素は置き換えない", async () => {
  const { document, listeners, posted } = setup(`
    <h1>Hello world</h1>
    <p>The <a href="#">dashboard</a> is ready.</p>
    <pre>const keep = 1</pre>
  `);

  const before = document.body.innerHTML;
  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(document.querySelector("h1").textContent, "HELLO WORLD");
  assert.ok(document.querySelector("p a"), "リンク要素は残る");
  assert.equal(document.querySelector("pre").textContent, "const keep = 1");
  assert.equal(posted.at(-1).phase, "done");

  const restored = await send(listeners, { type: "plamo:page-restore" });
  assert.ok(restored.count > 0);
  assert.equal(document.body.innerHTML, before);
});

test("インライン要素をまたぐ段落は 1 回のリクエストで訳し、語順を保つ", async () => {
  const { document, listeners, calls } = setup(
    `<p>The <a href="/d">dashboard</a> lists every task.</p>`,
  );

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(calls.length, 1, "段落は 1 回で訳す");
  assert.match(calls[0].text, /\u27E6/);
  assert.equal(document.querySelector("p").textContent.trim(), "THE DASHBOARD LISTS EVERY TASK.");
  assert.ok(document.querySelector("p a"));
});

test("目印が崩れたらノードごとの訳に戻す", async () => {
  const dom = setup(`<p>The <a href="/d">dashboard</a> lists every task.</p>`);
  const original = dom.window.chrome.runtime.sendMessage;
  dom.window.chrome.runtime.sendMessage = async (message) => {
    if (message.type === "plamo:status") return original(message);
    // 目印を落とすモデルを模す
    return { ok: true, translation: message.text.replace(/\u27E6[^\u27E7]*\u27E7/g, "").toUpperCase() };
  };

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  // 段落全体の訳は使わず、ノードごとに訳してリンクを残す
  assert.equal(dom.document.querySelector("p a").textContent, "DASHBOARD");
  assert.ok(dom.document.querySelector("p").textContent.includes("THE"));
  assert.equal(dom.posted.at(-1).phase, "done");
});

test("コード、SVG、数式、フォーム、非表示は訳さない", async () => {
  const { document, listeners } = setup(`
    <p>Visible sentence.</p>
    <svg><text>Chart label</text><style>.t{fill:red}</style></svg>
    <math><mi>E</mi><mo>=</mo><mi>m</mi></math>
    <select><option>Apple</option></select>
    <p hidden>Hidden sentence</p>
    <p style="display:none">Display none</p>
    <p translate="no">Do not translate</p>
    <p><code>inline_code()</code> stays.</p>
  `);

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(document.querySelector("svg text").textContent, "Chart label");
  assert.equal(document.querySelector("svg style").textContent, ".t{fill:red}");
  assert.equal(document.querySelector("math mi").textContent, "E");
  assert.equal(document.querySelector("option").textContent, "Apple");
  assert.equal(document.querySelector("select").value, "Apple");
  assert.equal(document.querySelectorAll("span").length, 0, "span を挿入しない");
  assert.equal(document.querySelector("[hidden]").textContent, "Hidden sentence");
  assert.equal(document.querySelector('[style="display:none"]').textContent, "Display none");
  assert.equal(document.querySelector('[translate="no"]').textContent, "Do not translate");
  assert.equal(document.querySelector("code").textContent, "inline_code()");
  assert.equal(document.querySelector("body > p").textContent, "VISIBLE SENTENCE.");
});

test("二度押しても増殖せず、翻訳中の復元は上書きしない", async () => {
  const { document, listeners, posted } = setup(`<p>First sentence here.</p><p>Second one.</p>`);

  await send(listeners, { type: "plamo:page-translate" });
  await settle();
  const afterFirst = document.body.textContent;

  await send(listeners, { type: "plamo:page-translate" });
  await settle();
  assert.equal(document.body.textContent, afterFirst);

  // 復元してから再度訳せる
  await send(listeners, { type: "plamo:page-restore" });
  assert.equal(document.body.textContent, "First sentence here.Second one.");
  await send(listeners, { type: "plamo:page-translate" });
  await settle();
  assert.equal(document.body.textContent, afterFirst);
  assert.equal(posted.at(-1).phase, "done");
});

test("訳の途中で復元したら、遅れて返った訳で上書きしない", async () => {
  const dom = setup(`<p>The <a href="/d">dashboard</a> lists every task.</p>`);
  const original = dom.window.chrome.runtime.sendMessage;
  dom.window.chrome.runtime.sendMessage = async (message) => {
    if (message.type === "plamo:status") return original(message);
    await new Promise((resolve) => setTimeout(resolve, 15));
    // 目印を落とすので、ノードごとの訳（await を挟む経路）に入る
    return { ok: true, translation: message.text.replace(/\u27E6[^\u27E7]*\u27E7/g, "").toUpperCase() };
  };

  const running = send(dom.listeners, { type: "plamo:page-translate" });
  await new Promise((resolve) => setTimeout(resolve, 25));
  await send(dom.listeners, { type: "plamo:page-restore" });
  await running;
  await new Promise((resolve) => setTimeout(resolve, 120));

  assert.equal(dom.document.body.textContent, "The dashboard lists every task.");
});

test("訳の失敗はページを止めず、失敗数として出る", async () => {
  const dom = setup(`<p>First</p><p>Second</p>`);
  const original = dom.window.chrome.runtime.sendMessage;
  dom.window.chrome.runtime.sendMessage = async (message) => {
    if (message.type === "plamo:translate" && message.text === "First") {
      return { ok: false, error: "bridge down" };
    }
    return original(message);
  };

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.document.body.textContent.includes("SECOND"), true);
  assert.equal(dom.document.body.textContent.includes("First"), true, "失敗した文は原文のまま");
  assert.ok(dom.posted.some((status) => status.phase === "item-error"));
});

test("段落の中の非表示インライン要素は訳さず、文にも混ぜない", async () => {
  const { document, listeners, calls } = setup(`
    <p>Visible text <span hidden>Secret</span><span aria-hidden="true">★</span><span style="display:none">Tooltip</span> end.</p>
    <div contenteditable><p>Draft message</p></div>
    <div contenteditable="plaintext-only">Another draft</div>
  `);

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(document.querySelector("[hidden]").textContent, "Secret");
  assert.equal(document.querySelector('[aria-hidden="true"]').textContent, "★");
  assert.equal(document.querySelector('[style="display:none"]').textContent, "Tooltip");
  assert.equal(document.querySelector("[contenteditable]").textContent, "Draft message");
  assert.equal(document.querySelector('[contenteditable="plaintext-only"]').textContent, "Another draft");
  const sent = calls.map((call) => call.text).join(" ");
  assert.equal(sent.includes("Secret"), false);
  assert.equal(sent.includes("Tooltip"), false);
  assert.equal(sent.includes("Draft"), false);
});

test("前の段落の訳を履歴として次の段落へ渡す", async () => {
  const { listeners, calls } = setup(`
    <p>Open the project dashboard.</p>
    <p>It lists all active tasks.</p>
    <p>You can filter the list.</p>
    <p>Filters stay after reload.</p>
  `);

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  const histories = calls.map((call) => call.history.length);
  assert.deepEqual(histories, [0, 1, 2, 2], "文書順で直前 2 文まで渡る");
  assert.match(calls[3].history[0].source, /It lists/);
  assert.match(calls[3].history[1].source, /You can filter/);
  assert.match(calls[2].history[0].source, /Open the/);
  assert.match(calls[2].history[1].source, /It lists/);
  assert.match(calls[2].history[1].translation, /IT LISTS/);
});

test("目印付きの長い段落は、目印を割らずにノードごとへ切り替える", async () => {
  const long = "This is a fairly long sentence.".repeat(12);
  const { document, listeners, calls } = setup(
    `<p>${long} <a href="/d">dashboard</a> ${long}</p>`,
  );

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(calls.every((call) => !call.text.includes("\u27E6")), true);
  // 段落全体は一度も送らず、3 ノードを 1 回ずつ訳す。
  assert.equal(calls.length, 3);
  assert.ok(document.querySelector("p a"));
  assert.equal(document.querySelector("p a").textContent, "DASHBOARD");
});

test("非 CJK の訳先では、空白の無い断片の間に空白を補う", async () => {
  const dom = setup(`<p>これは<a href="/d">リンク</a>です。</p>`);
  const original = dom.window.chrome.runtime.sendMessage;
  dom.window.chrome.runtime.sendMessage = async (message) => {
    if (message.type !== "plamo:translate") return original(message);
    return { ok: true, translation: "\u27E60\u27E7This is\u27E6/0\u27E7\u27E61\u27E7a link\u27E6/1\u27E7\u27E62\u27E7.\u27E6/2\u27E7" };
  };

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.document.querySelector("p").textContent, "This is a link.");
  assert.equal(dom.document.querySelector("p a").textContent, "a link");

  await send(dom.listeners, { type: "plamo:page-restore" });
  assert.equal(dom.document.querySelector("p").innerHTML, `これは<a href="/d">リンク</a>です。`);
});

/** 目印付きなら marked、そうでなければ原文ごとの表で返す偽モデル。 */
function fakeModel(dom, { marked, table = {}, delay = 0 }) {
  const original = dom.window.chrome.runtime.sendMessage;
  dom.window.chrome.runtime.sendMessage = async (message) => {
    if (message.type !== "plamo:translate") return original(message);
    dom.calls.push(message);
    if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
    if (marked && message.text.includes("\u27E6")) return { ok: true, translation: marked };
    return { ok: true, translation: table[message.text] ?? message.text.toUpperCase() };
  };
}

test("380 字超の日本語段落を英訳しても、リンク前後の語が密着しない", async () => {
  const before = "最初に説明します。".repeat(25);
  const after = "詳しくは後で述べます。".repeat(20);
  const dom = setup(`<p>${before}<a href="/d">管理画面</a>${after}</p>`);
  fakeModel(dom, { table: { [before]: "Open the", "管理画面": "dashboard", [after]: "for details." } });

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.calls.length, 3, "ノードごとに 1 回ずつ");
  assert.equal(dom.document.querySelector("p").textContent, "Open the dashboard for details.");
  assert.equal(dom.document.querySelector("p a").textContent, "dashboard", "空白はリンクの外");
  assert.equal(dom.posted.at(-1).phase, "done");
});

test("リンク直後の空白は <a> の中に入れず、後ろのテキストの頭に付ける", async () => {
  const dom = setup(`<p>詳しくは<a href="/d">こちら</a>をご覧ください</p>`);
  fakeModel(dom, {
    marked: "\u27E60\u27E7For details, see\u27E6/0\u27E7\u27E61\u27E7this page\u27E6/1\u27E7\u27E62\u27E7for more\u27E6/2\u27E7",
  });

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.document.querySelector("p").textContent, "For details, see this page for more");
  assert.equal(dom.document.querySelector("p a").textContent, "this page");

  await send(dom.listeners, { type: "plamo:page-restore" });
  assert.equal(dom.document.querySelector("p").innerHTML, `詳しくは<a href="/d">こちら</a>をご覧ください`);
});

test("翻訳中に復元したら、遅れて返った進捗や停止の通知を送らない", async () => {
  const dom = setup(`<p>First one.</p><p>Second one.</p>`);
  fakeModel(dom, { delay: 30 });

  const running = send(dom.listeners, { type: "plamo:page-translate" });
  await new Promise((resolve) => setTimeout(resolve, 10));
  await send(dom.listeners, { type: "plamo:page-restore" });
  const postedAtRestore = dom.posted.length;
  await running;
  await new Promise((resolve) => setTimeout(resolve, 100));

  assert.deepEqual(dom.posted.slice(postedAtRestore), [], "復元後に古い run の通知が来ない");
  assert.equal(dom.posted.at(-1).phase, "start");
  assert.equal(dom.document.body.textContent, "First one.Second one.");
});

test("復元直後に訳し直しても busy にならず、古い run の訳で二重に書かない", async () => {
  const dom = setup(`<p>First one.</p><p>Second one.</p>`);
  fakeModel(dom, { delay: 30 });

  const first = send(dom.listeners, { type: "plamo:page-translate" });
  await new Promise((resolve) => setTimeout(resolve, 10));
  await send(dom.listeners, { type: "plamo:page-restore" });
  const second = send(dom.listeners, { type: "plamo:page-translate" });
  await Promise.all([first, second]);
  await settle();

  assert.equal(dom.posted.some((status) => status.phase === "busy"), false);
  assert.equal(dom.posted.at(-1).phase, "done");
  assert.equal(dom.document.body.textContent, "FIRST ONE.SECOND ONE.");
  // 古い run の 1 回 + 新しい run の 2 回。古い run は 2 段落目へ進まない。
  assert.equal(dom.calls.length, 3);

  const restored = await send(dom.listeners, { type: "plamo:page-restore" });
  assert.equal(restored.count, 2, "書き込みは新しい run の 2 か所だけ");
  assert.equal(dom.document.body.textContent, "First one.Second one.");
});

test("履歴の訳文は実際に書いた段落で、目印が残らず語も密着しない", async () => {
  const dom = setup(`<p>詳しくは<a href="/d">こちら</a>をご覧ください</p><p>次の段落です。</p>`);
  fakeModel(dom, {
    marked: "\u27E60\u27E7For details, see\u27E6/0\u27E7\u27E61\u27E7this page\u27E6/1\u27E7\u27E62\u27E7for more\u27E6/2\u27E7",
  });

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  const history = dom.calls[1].history;
  assert.equal(history.length, 1);
  assert.equal(history[0].translation, "For details, see this page for more");
  assert.equal(history[0].source.includes("\u27E6"), false);
});

test("閉じた details は summary だけを訳し、入れ子でも中身は訳さない", async () => {
  const { document, listeners } = setup(`
    <details id="closed"><summary>Closed summary</summary><p>Closed body</p>
      <details open><summary>Inner summary</summary><p>Inner body</p></details>
    </details>
    <details open id="outer"><summary>Open summary</summary><p>Open body</p>
      <details><summary>Nested summary</summary><p>Nested body</p></details>
    </details>
  `);

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(document.querySelector("#closed > summary").textContent, "CLOSED SUMMARY");
  assert.equal(document.querySelector("#closed > p").textContent, "Closed body");
  assert.equal(document.querySelector("#closed details summary").textContent, "Inner summary");
  assert.equal(document.querySelector("#closed details p").textContent, "Inner body");
  assert.equal(document.querySelector("#outer > p").textContent, "OPEN BODY");
  assert.equal(document.querySelector("#outer details summary").textContent, "NESTED SUMMARY");
  assert.equal(document.querySelector("#outer details p").textContent, "Nested body");
});

test("中間の断片の訳が空で両隣に空白があっても、空白は 1 つにする", async () => {
  const dom = setup(`<p>See <span>the </span>dashboard</p>`);
  fakeModel(dom, { marked: "\u27E60\u27E7See\u27E6/0\u27E7 \u27E61\u27E7\u27E6/1\u27E7 \u27E62\u27E7dashboard\u27E6/2\u27E7" });

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.document.querySelector("p").textContent, "See dashboard");
  assert.equal(dom.calls[0].text.includes("\u27E6"), true, "目印経路を通っている");
});

test("中間の断片の訳が空で前後に空白が無くても、語はくっつかない", async () => {
  const dom = setup(`<p>See<span>the</span>dashboard</p>`);
  fakeModel(dom, { marked: "\u27E60\u27E7See\u27E6/0\u27E7\u27E61\u27E7\u27E6/1\u27E7\u27E62\u27E7dashboard\u27E6/2\u27E7" });

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.document.querySelector("p").textContent, "See dashboard");
});

test("空の断片で空白を潰したあとも、復元すると原文に完全一致する", async () => {
  const html = `<p>See <span>the </span>dashboard</p><p>Open <a href="/d">it </a> now</p>`;
  const dom = setup(html);
  fakeModel(dom, {
    marked: "\u27E60\u27E7See\u27E6/0\u27E7 \u27E61\u27E7\u27E6/1\u27E7 \u27E62\u27E7dashboard\u27E6/2\u27E7",
  });
  const before = dom.document.body.innerHTML;

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();
  assert.equal(dom.document.querySelectorAll("p")[0].textContent, "See dashboard");

  const restored = await send(dom.listeners, { type: "plamo:page-restore" });
  assert.ok(restored.count >= 3);
  assert.equal(dom.document.body.innerHTML, before);
});

test("目印の断片がすべて空なら、段落を消さずノードごとに訳す", async () => {
  const dom = setup(`<p>The <a href="/d">dashboard</a> lists every task.</p>`);
  const original = dom.window.chrome.runtime.sendMessage;
  dom.window.chrome.runtime.sendMessage = async (message) => {
    if (message.type !== "plamo:translate") return original(message);
    if (message.text.includes("\u27E6")) {
      return { ok: true, translation: "\u27E60\u27E7\u27E6/0\u27E7\u27E61\u27E7\u27E6/1\u27E7\u27E62\u27E7\u27E6/2\u27E7" };
    }
    return { ok: true, translation: message.text.toUpperCase() };
  };

  await send(dom.listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(dom.document.querySelector("p").textContent, "THE DASHBOARD LISTS EVERY TASK.");
});

test("visibility は子が上書きでき、表示の切り替えは次の走査で反映される", async () => {
  const { document, listeners } = setup(`
    <div style="visibility:hidden"><p>Hidden parent</p><p style="visibility:visible">Shown child</p></div>
    <p id="toggle">Toggle me</p>
  `);

  await send(listeners, { type: "plamo:page-translate" });
  await settle();
  assert.equal(document.querySelector("div p").textContent, "Hidden parent");
  assert.equal(document.querySelector('[style="visibility:visible"]').textContent, "SHOWN CHILD");
  assert.equal(document.querySelector("#toggle").textContent, "TOGGLE ME");

  await send(listeners, { type: "plamo:page-restore" });
  document.querySelector("#toggle").hidden = true;
  await send(listeners, { type: "plamo:page-translate" });
  await settle();
  assert.equal(document.querySelector("#toggle").textContent, "Toggle me", "前回の判定を使い回さない");
});

test("復元は、訳した後にページが書き換えた文を上書きしない", async () => {
  const { document, listeners } = setup(`<p>Original sentence.</p>`);
  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  const node = document.querySelector("p").firstChild;
  node.nodeValue = "Updated by SPA";

  const result = await send(listeners, { type: "plamo:page-restore" });
  assert.equal(result.count, 0);
  assert.equal(document.querySelector("p").textContent, "Updated by SPA");
});

test("復元は何度でもでき、往復で文が消えない", async () => {
  const { document, listeners } = setup(`<p>Alpha</p><p>Beta</p>`);
  await send(listeners, { type: "plamo:page-restore" });
  assert.equal(document.body.textContent, "AlphaBeta");

  await send(listeners, { type: "plamo:page-translate" });
  await settle();
  assert.equal(document.body.textContent, "ALPHABETA");

  await send(listeners, { type: "plamo:page-restore" });
  assert.equal(document.body.textContent, "AlphaBeta");
});

test("body が無いページでも固まらない", async () => {
  const { window, document, listeners, posted } = setup(`<p>Alpha</p>`);
  document.body.remove();

  await send(listeners, { type: "plamo:page-translate" });
  await settle();

  assert.equal(posted.at(-1).phase, "empty");
  const status = await send(listeners, { type: "plamo:page-count" });
  assert.equal(status.ok, true);
  assert.equal(status.running, false, "running が残らない");
  void window;
});
