/**
 * 実ブリッジ (http://127.0.0.1:8788) まで含めた通し確認。
 * jsdom のページで content.js を動かし、chrome.runtime.sendMessage を
 * ブリッジへの実リクエストに差し替える。ブリッジが動いているときだけ実行する。
 *
 *   node tools/e2e-bridge.mjs
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { JSDOM } from "jsdom";
import { buildPrompt, cleanTranslation, STOP_TOKENS } from "../extension/lib/pfml3.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = readFileSync(join(root, "extension", "content", "content.js"), "utf8");
const ENDPOINT = "http://127.0.0.1:8788";

const html = `
  <h1>Release notes</h1>
  <p>The dashboard now shows every <a href="/tasks">active task</a> in one place.</p>
  <p>You can filter the list by assignee, and the filter stays after a reload.</p>
  <ul><li>Faster loading</li><li>New keyboard shortcuts</li></ul>
  <pre>npm run build</pre>
`;

const dom = new JSDOM(`<!doctype html><html><body>${html}</body></html>`, {
  url: "https://example.test/",
  runScripts: "outside-only",
});
const { window } = dom;
const document = window.document;
const listeners = [];
const posted = [];

window.chrome = {
  runtime: {
    onMessage: { addListener: (fn) => listeners.push(fn) },
    sendMessage: async (message) => {
      if (message.type === "plamo:status") {
        posted.push(message.status);
        return { ok: true };
      }
      if (message.type !== "plamo:translate") return { ok: true };
      const prompt = buildPrompt({
        text: message.text,
        targetLang: "ja",
        form: "polite",
        history: message.history || [],
      });
      const response = await fetch(`${ENDPOINT}/v1/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ prompt, temperature: 0, max_tokens: 512, stop: STOP_TOKENS }),
      });
      if (!response.ok) return { ok: false, error: `bridge ${response.status}` };
      const payload = await response.json();
      return { ok: true, translation: cleanTranslation(payload.choices[0].text) };
    },
  },
};

vm.runInContext(source, dom.getInternalVMContext());

function send(message) {
  return new Promise((resolve) => {
    for (const listener of listeners) listener(message, { id: "e2e" }, resolve);
  });
}

const health = await fetch(`${ENDPOINT}/health`).catch(() => null);
if (!health || !health.ok) {
  console.error("ブリッジが動いていません。start-bridge.ps1 を先に実行してください。");
  process.exit(1);
}

const before = document.body.textContent;
const started = Date.now();
await send({ type: "plamo:page-translate" });
await new Promise((resolve) => setTimeout(resolve, 200));

const after = document.body.textContent;
console.log(`経過: ${Date.now() - started} ms`);
console.log("--- 訳したページ ---");
console.log(after.replace(/\n\s*\n/g, "\n").trim());
console.log("--- 進捗の最後 ---");
console.log(JSON.stringify(posted.at(-1)));

const kept = after.includes("npm run build");
console.log(`コードブロックは残った: ${kept}`);

const restored = await send({ type: "plamo:page-restore" });
console.log(`戻した箇所: ${restored.count}`);
console.log(`原文に一致: ${document.body.textContent === before}`);

if (!kept || document.body.textContent !== before) {
  process.exit(1);
}
