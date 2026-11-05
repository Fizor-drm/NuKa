import assert from "node:assert/strict";
import test from "node:test";
import {
  buildPrompt,
  cleanTranslation,
  joinTranslations,
  splitText,
  stripControlTokens,
  STOP_TOKENS,
} from "../lib/pfml3.js";

const TAG = "<|plamo:tag|>";

test("単文の英訳プロンプトは公式例と同じ区切りになる", () => {
  const prompt = buildPrompt({
    text: "会議は来週の月曜日に変更されました。",
    targetLang: "en",
  });
  assert.equal(
    prompt,
    "<|plamo:tag|>dataset<|plamo:msg|>translation" +
      "<|plamo:tag|>input<|plamo:msg|>会議は来週の月曜日に変更されました。" +
      "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
  );
});

test("履歴、文脈、用語、文体を公式の順序で並べる", () => {
  const prompt = buildPrompt({
    text: "オーナーでフィルタリングすることもできます。",
    sourceLang: "ja",
    targetLang: "en",
    context: "プロジェクト管理ツールのヘルプページです。",
    glossary: [{ source: "オーナー", target: "assignee" }],
    history: [
      { source: "プロジェクトの管理画面を開きます。", translation: "Open the project dashboard." },
      { source: "進行中のタスクがすべて表示されます。", translation: "It lists all active tasks." },
    ],
  });
  assert.equal(
    prompt,
    "<|plamo:tag|>dataset<|plamo:msg|>translation" +
      "<|plamo:tag|>context<|plamo:msg|>プロジェクト管理ツールのヘルプページです。" +
      "<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>プロジェクトの管理画面を開きます。" +
      "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Open the project dashboard." +
      "<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>進行中のタスクがすべて表示されます。" +
      "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>It lists all active tasks." +
      "<|plamo:tag|>glossary<|plamo:index|>オーナー<|plamo:item|>assignee" +
      "<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>オーナーでフィルタリングすることもできます。" +
      "<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
  );
});

test("日本語への普通体だけ form を付ける", () => {
  const plain = buildPrompt({ text: "This system is convenient.", targetLang: "ja", form: "plain" });
  assert.match(plain, /<\|plamo:key\|>form<\|plamo:val\|>plain<\|plamo:msg\|>$/);
  const english = buildPrompt({ text: "これは便利です。", targetLang: "en", form: "plain" });
  assert.equal(english.includes("form"), false);
});

test("制御トークンは入れ子でも残らない", () => {
  const nested = "会議<|plamo:<|plamo:tag|>tag|>は明日です。";
  assert.equal(stripControlTokens(nested), "会議は明日です。");
  const prompt = buildPrompt({ text: nested, targetLang: "en" });
  assert.equal(prompt.split(TAG).length - 1, 3);
  assert.equal(prompt.includes("output<|plamo:key|>lang"), true);
  assert.equal(prompt.includes("会議は明日です。"), true);
});

test("制御トークンだけで空になる文は送らない", () => {
  assert.throws(() => buildPrompt({ text: "<|plamo:tag|>", targetLang: "en" }), /空/);
  assert.throws(() => buildPrompt({ text: "   ", targetLang: "en" }));
});

test("未知の言語コードと文体は拒む", () => {
  assert.throws(() => buildPrompt({ text: "hello", targetLang: "" }));
  assert.throws(() => buildPrompt({ text: "hello", targetLang: "xx" }));
  assert.throws(() => buildPrompt({ text: "hello", targetLang: "ja", form: "casual" }));
  assert.throws(() => buildPrompt({ text: "hello", targetLang: "en", sourceLang: "xx" }));
});

test("長い原文は句点で分け、略語のピリオドでは割らない", () => {
  assert.deepEqual(splitText(""), []);
  assert.deepEqual(splitText("短い文。"), ["短い文。"]);

  const long = "あ。".repeat(300);
  const parts = splitText(long, 400);
  assert.ok(parts.length > 1);
  assert.ok(parts.every((part) => Array.from(part).length <= 400));
  assert.equal(parts.join(""), long);

  const english = `${"word ".repeat(90)}Mr. Smith arrived.`;
  const chunks = splitText(english, 100);
  assert.ok(chunks.some((chunk) => chunk.includes("Mr.") === false));
  assert.ok(chunks.join(" ").includes("Mr. Smith"));
  assert.equal(chunks.some((chunk) => chunk.endsWith("Mr.")), false);
});

test("訳文の連結は日本語なら空白を入れない", () => {
  assert.equal(joinTranslations(["あ", "い"], "ja"), "あい");
  assert.equal(joinTranslations(["a", "b"], "en"), "a b");
  assert.equal(joinTranslations(["", "b"], "en"), "b");
});

test("停止トークン以降を落として訳文だけ残す", () => {
  assert.equal(cleanTranslation(" The meeting is Monday. \n<|plamo:tag|>"), "The meeting is Monday.");
  assert.equal(cleanTranslation("訳文です。<|plamo:bos|>extra"), "訳文です。");
  assert.deepEqual(STOP_TOKENS, ["<|plamo:tag|>", "<|plamo:bos|>"]);
});
