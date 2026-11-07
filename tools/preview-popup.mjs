/**
 * ポップアップの見た目を確認するための静的プレビュー。
 * Chrome 拡張の API は使えないので、HTML と CSS をそのまま読み、
 * 言語の選択肢だけを埋める。表示の確認用で、動作確認ではない。
 *
 *   node tools/preview-popup.mjs
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { LANGUAGES } from "../extension/lib/languages.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const html = readFileSync(join(root, "extension", "popup", "popup.html"), "utf8");
const css = readFileSync(join(root, "extension", "popup", "popup.css"), "utf8");

// 引数 light を付けると、OS の外観に関係なく明るい配色で描く。
const forceLight = process.argv.includes("light");
const lightVars = forceLight
  ? `<style>:root{color-scheme:light;--ink:#1c1915;--paper:#f6f1e7;--rule:#d9d0c2;--quiet:#6f675c;--seal:#9c2b2b;--seal-ink:#fff8f2;--accent-text:#9c2b2b;--focus:#1c1915}select,textarea{background:#fffdf8}</style>`
  : "";

const options = (selected) =>
  LANGUAGES.map(
    (language) =>
      `<option value="${language.code}"${language.code === selected ? " selected" : ""}>${language.label} (${language.native})</option>`,
  ).join("");

const preview = html
  .replace('<link rel="stylesheet" href="popup.css" />', `<style>\n${css}\n</style>${lightVars}`)
  .replace('<script type="module" src="popup.js"></script>', "")
  .replace(
    '<select id="target" name="targetLang"></select>',
    `<select id="target" name="targetLang">${options("ja")}</select>`,
  )
  .replace(
    '<select id="source" name="sourceLang">\n          <option value="">ページから判断する</option>\n        </select>',
    `<select id="source" name="sourceLang"><option value="">ページから判断する</option>${options("en")}</select>`,
  )
  .replace(
    '<p class="status" id="bridge" role="status">ブリッジを確認しています</p>',
    '<p class="status" id="bridge" role="status" data-state="ready">ブリッジは応答しています</p>',
  )
  .replace('<p class="count" id="count" aria-live="polite"></p>', '<p class="count" id="count">このページで訳せる文: 42</p>')
  .replace('<p class="progress" id="progress" role="status" aria-live="polite"></p>', '<p class="progress" id="progress">12 / 42</p>');

const out = join(root, "tools", forceLight ? "popup-preview-light.html" : "popup-preview.html");
writeFileSync(out, preview);
console.log(out);
