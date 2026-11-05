/**
 * 翻訳リクエストはサービスワーカーが持つ。
 * コンテンツスクリプトから 127.0.0.1 へ直接出すと、ページ側の CSP や
 * Private Network Access の確認で弾かれるページがあるため。
 */

import { buildPrompt, cleanTranslation, joinTranslations, splitText, STOP_TOKENS } from "./lib/pfml3.js";
import { isSupportedLanguage } from "./lib/languages.js";

const ENDPOINT = "http://127.0.0.1:8788";

const DEFAULTS = {
  targetLang: "ja",
  sourceLang: "",
  form: "",
  glossary: [],
};

export async function loadSettings() {
  const stored = await chrome.storage.local.get(DEFAULTS);
  const settings = { ...DEFAULTS, ...stored };
  if (!isSupportedLanguage(settings.targetLang)) settings.targetLang = DEFAULTS.targetLang;
  if (settings.sourceLang && !isSupportedLanguage(settings.sourceLang)) settings.sourceLang = "";
  if (settings.form !== "plain" && settings.form !== "polite") settings.form = "";
  if (!Array.isArray(settings.glossary)) settings.glossary = [];
  return settings;
}

async function translateOne(text, settings, history) {
  const prompt = buildPrompt({
    text,
    targetLang: settings.targetLang,
    sourceLang: settings.sourceLang || undefined,
    form: settings.targetLang === "ja" ? settings.form || undefined : undefined,
    glossary: settings.glossary,
    history,
  });

  const response = await fetch(`${ENDPOINT}/v1/completions`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    signal: AbortSignal.timeout(120000),
    body: JSON.stringify({
      model: "plamo-3-translate",
      prompt,
      temperature: 0,
      // 漢字 1 文字で英単語 1 つ以上になることがあるので、文字数より広めに取る。
      max_tokens: Math.min(2048, Math.max(192, Array.from(text).length * 4)),
      stop: STOP_TOKENS,
    }),
  });

  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(`ブリッジが ${response.status} を返しました。${detail.slice(0, 180)}`);
  }

  const payload = await response.json();
  const translation = cleanTranslation(payload?.choices?.[0]?.text ?? "");
  if (!translation) throw new Error("ブリッジから空の訳が返りました。");
  return translation;
}

async function translateText(text, settings, history) {
  const parts = splitText(text);
  if (parts.length === 0) throw new Error("訳す文が空です。");

  const translations = [];
  let context = history;
  for (const part of parts) {
    const translation = await translateOne(part, settings, context);
    translations.push(translation);
    context = context.concat([{ source: part, translation }]).slice(-2);
  }
  return joinTranslations(translations, settings.targetLang);
}

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (!message || message.type !== "plamo:translate") return false;

  (async () => {
    try {
      const settings = await loadSettings();
      const translation = await translateText(message.text, settings, message.history || []);
      sendResponse({ ok: true, translation });
    } catch (error) {
      sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
    }
  })();

  return true;
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (!message || message.type !== "plamo:health") return false;

  (async () => {
    try {
      const response = await fetch(`${ENDPOINT}/health`, { signal: AbortSignal.timeout(4000) });
      const payload = await response.json();
      sendResponse({ ok: true, health: payload });
    } catch (error) {
      sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
    }
  })();

  return true;
});
