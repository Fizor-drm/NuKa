/**
 * PFML3 プロンプトを組み立てる。
 * 公式カードの Completions API 例と同じ区切りを使う。
 * https://huggingface.co/pfnet/plamo-3-translate-0.8b
 *
 * 並び:
 *   dataset → context? → 履歴の input/output → glossary? → 今回の input → 空の output
 * 生成は最後の <|plamo:msg|> の直後から始まり、モデルは <|plamo:tag|> か <|plamo:bos|> で止まる。
 *
 * 原文はページ由来なので、区切りを偽装されうる。制御トークンは名前を問わず落とし、
 * 落とした結果が空になったら訳さない。
 */

import { isSupportedLanguage } from "./languages.js";

const TAG = "<|plamo:tag|>";
const MSG = "<|plamo:msg|>";
const KEY = "<|plamo:key|>";
const VAL = "<|plamo:val|>";
const INDEX = "<|plamo:index|>";
const ITEM = "<|plamo:item|>";

export const STOP_TOKENS = [TAG, "<|plamo:bos|>"];

// <|plamo:xxx|> をすべて対象にする。埋め込みで <|plamo:<|plamo:tag|>tag|> のような
// 入れ子を書かれても、除去を繰り返せば平文だけが残る。
const CONTROL_RE = /<\|plamo:[a-z0-9_]+\|>/gi;

export function stripControlTokens(value) {
  let text = String(value ?? "");
  let previous;
  do {
    previous = text;
    text = text.replace(CONTROL_RE, "");
  } while (text !== previous);
  return text;
}

/** モデルへ渡せる文字列だけを返す。空になったら null。 */
function clean(value) {
  const text = stripControlTokens(value).trim();
  return text || null;
}

function langAttr(code) {
  return code && isSupportedLanguage(code) ? `${KEY}lang${VAL}${code}` : "";
}

function inputElement(text, sourceLang) {
  return `${TAG}input${langAttr(sourceLang)}${MSG}${stripControlTokens(text).trim()}`;
}

function outputElement(text, targetLang, form) {
  const style = form && targetLang === "ja" ? `${KEY}form${VAL}${form}` : "";
  return `${TAG}output${KEY}lang${VAL}${targetLang}${style}${MSG}${stripControlTokens(text).trim()}`;
}

function glossaryElement(glossary) {
  const pairs = (Array.isArray(glossary) ? glossary : [])
    .map((pair) => ({
      source: pair && typeof pair === "object" ? clean(pair.source) : null,
      target: pair && typeof pair === "object" ? clean(pair.target) : null,
    }))
    .filter((pair) => pair.source && pair.target);
  if (pairs.length === 0) return "";
  const body = pairs.map((pair) => `${INDEX}${pair.source}${ITEM}${pair.target}`).join("");
  return `${TAG}glossary${body}`;
}

/**
 * @param {object} options
 * @param {string} options.text 今回訳す原文。
 * @param {string} options.targetLang 必須。
 * @param {string} [options.sourceLang]
 * @param {string} [options.context]
 * @param {{source: string, target: string}[]} [options.glossary]
 * @param {"plain"|"polite"} [options.form] 訳先が ja のときだけ有効。
 * @param {{source: string, translation: string}[]} [options.history] 文書順。古いものから。
 */
export function buildPrompt(options) {
  const { text, targetLang, sourceLang, context, glossary, form, history } = options || {};
  const source = clean(text);
  if (!source) {
    throw new Error("訳す文が空です。");
  }
  if (!isSupportedLanguage(targetLang)) {
    throw new Error(`訳先の言語が対応表にありません: ${targetLang}`);
  }
  if (sourceLang && !isSupportedLanguage(sourceLang)) {
    throw new Error(`原文の言語が対応表にありません: ${sourceLang}`);
  }
  if (form && form !== "plain" && form !== "polite") {
    throw new Error(`文体は plain か polite です: ${form}`);
  }

  let prompt = `${TAG}dataset${MSG}translation`;

  const contextText = clean(context);
  if (contextText) {
    prompt += `${TAG}context${MSG}${contextText}`;
  }

  for (const pair of Array.isArray(history) ? history : []) {
    if (!pair || typeof pair !== "object") continue;
    const pairSource = clean(pair.source);
    const pairTranslation = clean(pair.translation);
    if (!pairSource || !pairTranslation) continue;
    prompt += inputElement(pairSource, sourceLang);
    prompt += outputElement(pairTranslation, targetLang);
  }

  prompt += glossaryElement(glossary);
  prompt += inputElement(source, sourceLang);
  prompt += outputElement("", targetLang, form);
  return prompt;
}

// 欧文のピリオドは、略語・小数・イニシャルを文末と取り違えないようにする。
const ABBREVIATIONS = new Set([
  "mr", "mrs", "ms", "dr", "prof", "st", "jr", "sr", "vs", "etc", "inc", "ltd",
  "co", "no", "fig", "eq", "ref", "approx", "dept", "est", "min", "max", "vol",
]);

/**
 * 1 リクエストに載せる原文の上限。学習時の上限は入出力あわせて 32,768 トークンだが、
 * ページ翻訳は文単位で流すので、句点で 400 字程度に分ける。
 * 欧文のピリオドは、次の語が大文字で始まり、直前が略語やイニシャルでないときだけ文末とみなす。
 */
export function splitText(text, limit = 400) {
  const source = String(text ?? "").trim();
  if (!source) return [];
  if (source.length <= limit) return [source];

  const characters = Array.from(source);
  const sentences = [];
  let start = 0;
  let lastSpace = -1;

  const isSentenceEnd = (index) => {
    const character = characters[index];
    if ("。！？".includes(character)) return true;
    if (character === "!" || character === "?") return true;
    if (character !== ".") return false;
    const next = characters[index + 1];
    if (next === undefined) return true;
    if (!/\s/.test(next)) return false; // 小数やドメインは割らない
    const following = characters.slice(index + 1).join("").trimStart()[0];
    if (!following || !/\p{Lu}/u.test(following)) return false;
    const word = characters
      .slice(lastSpace + 1, index)
      .join("")
      .toLowerCase()
      .replace(/[^a-z.]/g, "");
    if (!word) return false;
    if (ABBREVIATIONS.has(word)) return false;
    if (word.length === 1) return false; // イニシャル
    return true;
  };

  for (let index = 0; index < characters.length; index += 1) {
    if (/\s/.test(characters[index])) lastSpace = index;
    if (isSentenceEnd(index)) {
      sentences.push(characters.slice(start, index + 1).join(""));
      start = index + 1;
    }
  }
  if (start < characters.length) sentences.push(characters.slice(start).join(""));

  const chunks = [];
  let current = "";
  for (const sentence of sentences) {
    if (current && Array.from(current + sentence).length > limit) {
      chunks.push(current.trim());
      current = sentence;
    } else {
      current += sentence;
    }
  }
  if (current.trim()) chunks.push(current.trim());

  return chunks.flatMap((chunk) => {
    const letters = Array.from(chunk);
    if (letters.length <= limit) return [chunk];
    const slices = [];
    for (let index = 0; index < letters.length; index += limit) {
      slices.push(letters.slice(index, index + limit).join(""));
    }
    return slices;
  });
}

/** 訳文を連結する。日本語・中国語・韓国語は語間の空白を入れない。 */
export function joinTranslations(parts, targetLang) {
  const separator = ["ja", "zh-hans", "zh-hant", "ko"].includes(targetLang) ? "" : " ";
  return parts.filter(Boolean).join(separator);
}

/** 生成結果から停止トークン以降を落とし、前後の空白だけ取る。改行は訳文の一部なので残す。 */
export function cleanTranslation(raw) {
  let text = String(raw ?? "");
  const cut = text.search(CONTROL_RE);
  if (cut >= 0) text = text.slice(0, cut);
  return text.trim();
}
