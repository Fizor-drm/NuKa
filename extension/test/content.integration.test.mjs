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
