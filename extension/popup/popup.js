import { LANGUAGES } from "../lib/languages.js";

const bridge = document.querySelector("#bridge");
const target = document.querySelector("#target");
const source = document.querySelector("#source");
const formField = document.querySelector("#form-field");
const glossary = document.querySelector("#glossary");
const count = document.querySelector("#count");
const translateButton = document.querySelector("#translate");
const stopButton = document.querySelector("#stop");
const restoreButton = document.querySelector("#restore");
const progress = document.querySelector("#progress");
const errorBox = document.querySelector("#error");

let tabId = null;
const FORMS = new Set(["", "plain", "polite"]);

function option(language, selected) {
  const element = document.createElement("option");
  element.value = language.code;
  element.textContent = `${language.label} (${language.native})`;
  element.selected = language.code === selected;
  return element;
}

function glossaryToText(pairs) {
  return (pairs || []).map((pair) => `${pair.source} = ${pair.target}`).join("\n");
}

function textToGlossary(text) {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const splitAt = line.indexOf("=");
      if (splitAt < 1) return null;
      const sourceTerm = line.slice(0, splitAt).trim();
      const targetTerm = line.slice(splitAt + 1).trim();
      return sourceTerm && targetTerm ? { source: sourceTerm, target: targetTerm } : null;
    })
    .filter(Boolean);
}

function showError(message) {
  errorBox.hidden = !message;
  errorBox.textContent = message || "";
}

function syncFormField() {
  const intoJapanese = target.value === "ja";
  formField.disabled = !intoJapanese;
  formField.hidden = !intoJapanese;
}

function setRunning(running) {
  translateButton.disabled = running;
  stopButton.disabled = !running;
}

async function activeTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id) throw new Error("開いているタブが見つかりません。");
  if (!tab.url || !/^https?:/i.test(tab.url)) {
    throw new Error("このページには注入できません。http か https のページで開いてください。");
  }
  return tab;
}

/**
 * 開いたときにだけ注入する。content_scripts で常時注入すると
 * すべてのサイトへのアクセス権限が必要になり、閲覧していないページまで対象になる。
 * content.js は二重注入を自分で弾く。
 */
async function ensureContentScript(id) {
  await chrome.scripting.executeScript({ target: { tabId: id }, files: ["content/content.js"] });
}

/**
 * 操作のたびに、そのタブに content.js があることを確かめてから送る。
 * ポップアップを開いたままタブが遷移・再読み込みされると、前の注入は消えている。
 * content.js は二重注入を自分で弾くので、毎回入れても安全。
 */
async function sendToPage(message) {
  const tab = await activeTab();
  tabId = tab.id;
  await ensureContentScript(tabId);
  return chrome.tabs.sendMessage(tabId, message);
}

async function checkBridge() {
  const health = await chrome.runtime.sendMessage({ type: "plamo:health" });
  if (!health?.ok) {
    bridge.dataset.state = "down";
    bridge.textContent = "ブリッジが止まっています。start-bridge.ps1 を先に実行してください。";
    return;
  }
  if (health.health?.ready) {
    bridge.dataset.state = "ready";
    bridge.textContent = "ブリッジは応答しています";
    return;
  }
  bridge.dataset.state = "down";
  bridge.textContent = health.health?.detail
    ? `モデルがまだ読めていません。${String(health.health.detail).slice(0, 80)}`
    : "モデルがまだ読めていません";
}

async function load() {
  for (const language of LANGUAGES) target.append(option(language, "ja"));
  for (const language of LANGUAGES) source.append(option(language, ""));

  const stored = await chrome.storage.local.get({
    targetLang: "ja",
    sourceLang: "",
    form: "",
    glossary: [],
  });
  target.value = stored.targetLang;
  source.value = stored.sourceLang;
  glossary.value = glossaryToText(stored.glossary);
  // 保存値は自由に書き換えられるので、選択肢にある値だけを反映する。
  const formInput = FORMS.has(stored.form) ? formField.querySelector(`input[value="${stored.form}"]`) : null;
  if (formInput) formInput.checked = true;
  syncFormField();

  await checkBridge();

  try {
    const page = await sendToPage({ type: "plamo:page-count" });
    if (page?.running) {
      setRunning(true);
      progress.textContent = "前回の翻訳が続いています";
    }
    count.textContent = page?.count ? `このページで訳せる文: ${page.count}` : "訳せる文が見つかりません。";
  } catch (error) {
    count.textContent = error instanceof Error ? error.message : String(error);
  }
}

async function save() {
  const form = new FormData(document.querySelector("#form"));
  const formValue = String(form.get("form") || "");
  await chrome.storage.local.set({
    targetLang: target.value,
    sourceLang: source.value,
    form: FORMS.has(formValue) ? formValue : "",
    glossary: textToGlossary(glossary.value),
  });
}

target.addEventListener("change", () => {
  syncFormField();
  save();
});
source.addEventListener("change", save);
glossary.addEventListener("change", save);
formField.addEventListener("change", save);

translateButton.addEventListener("click", async () => {
  showError("");
  progress.textContent = "翻訳を始めています";
  setRunning(true);
  try {
    await save();
    await sendToPage({ type: "plamo:page-translate" });
  } catch (error) {
    showError(error instanceof Error ? error.message : String(error));
    setRunning(false);
  }
});

stopButton.addEventListener("click", async () => {
  try {
    await sendToPage({ type: "plamo:page-stop" });
    progress.textContent = "止まり次第、途中までの訳を残します";
  } catch (error) {
    showError(error instanceof Error ? error.message : String(error));
  } finally {
    stopButton.disabled = true;
  }
});

restoreButton.addEventListener("click", async () => {
  showError("");
  try {
    const result = await sendToPage({ type: "plamo:page-restore" });
    progress.textContent = result?.count ? `${result.count} か所を原文に戻しました` : "戻す訳がありません";
    setRunning(false);
  } catch (error) {
    showError(error instanceof Error ? error.message : String(error));
  }
});

chrome.runtime.onMessage.addListener((message, sender) => {
  if (message?.type !== "plamo:status") return;
  // 別のタブの進捗をこのポップアップに出さない。
  if (sender?.tab?.id != null && tabId != null && sender.tab.id !== tabId) return;

  const status = message.status || {};
  if (status.phase === "start") {
    setRunning(true);
    progress.textContent = `${status.total} 文を訳します`;
  } else if (status.phase === "progress") {
    progress.textContent = `${status.done} / ${status.total}` + (status.failed ? `、失敗 ${status.failed}` : "");
  } else if (status.phase === "done") {
    progress.textContent = `訳し終わりました。${status.done} 文` + (status.failed ? `、失敗 ${status.failed}` : "");
    setRunning(false);
  } else if (status.phase === "stopped") {
    progress.textContent = `止めました。${status.done} 文まで訳しています`;
    setRunning(false);
  } else if (status.phase === "empty") {
    progress.textContent = "訳せる文が見つかりません";
    setRunning(false);
  } else if (status.phase === "busy") {
    progress.textContent = "翻訳が進行中です。止めるか、終わるのを待ってください。";
    setRunning(true);
  } else if (status.phase === "error") {
    showError(status.error);
    setRunning(false);
  } else if (status.phase === "item-error") {
    showError(status.error);
  }
});

load().catch((error) => showError(error instanceof Error ? error.message : String(error)));
