"""extension/lib/pfml3.js と同じ組み立てを Python で持つ。

ブリッジは拡張機能が作ったプロンプトを中継するだけでも動くが、
curl で直接叩くときのために同じ規則をこちらにも置く。
両方のテストが同じ期待値を共有する。
"""

from __future__ import annotations

import re

TAG = "<|plamo:tag|>"
MSG = "<|plamo:msg|>"
KEY = "<|plamo:key|>"
VAL = "<|plamo:val|>"
INDEX = "<|plamo:index|>"
ITEM = "<|plamo:item|>"

STOP_TOKENS = [TAG, "<|plamo:bos|>"]

LANGUAGES = {
    "ja", "en", "zh-hans", "zh-hant", "ko", "ar", "az", "be", "bg", "bn", "ca", "ceb",
    "de", "es", "et", "eu", "fa", "fr", "gu", "he", "hi", "hr", "hu", "hy", "id", "it",
    "jv", "ka", "kk", "km", "lt", "lv", "ml", "mr", "ms", "my", "ne", "nl", "pa", "pl",
    "ru", "si", "sk", "sl", "sr", "sw", "ta", "te", "th", "tl", "ur", "uz", "vi",
}

CONTROL_RE = re.compile(r"<\|plamo:[a-z0-9_:.\-]+\|>", re.IGNORECASE)


def strip_control_tokens(value: object) -> str:
    text = "" if value is None else str(value)
    previous = None
    while previous != text:
        previous = text
        text = CONTROL_RE.sub("", text)
    return text


def _clean(value: object) -> str | None:
    text = strip_control_tokens(value).strip()
    return text or None


def _lang_attr(code: str | None) -> str:
    return f"{KEY}lang{VAL}{code}" if code in LANGUAGES else ""


def _input_element(text: str, source_lang: str | None) -> str:
    return f"{TAG}input{_lang_attr(source_lang)}{MSG}{strip_control_tokens(text).strip()}"


def _output_element(text: str, target_lang: str, form: str | None = None) -> str:
    style = f"{KEY}form{VAL}{form}" if form and target_lang == "ja" else ""
    return f"{TAG}output{KEY}lang{VAL}{target_lang}{style}{MSG}{strip_control_tokens(text).strip()}"


def _glossary_element(glossary: object) -> str:
    pairs = []
    for pair in glossary if isinstance(glossary, list) else []:
        if not isinstance(pair, dict):
            continue
        source = _clean(pair.get("source"))
        target = _clean(pair.get("target"))
        if source and target:
            pairs.append((source, target))
    if not pairs:
        return ""
    body = "".join(f"{INDEX}{source}{ITEM}{target}" for source, target in pairs)
    return f"{TAG}glossary{body}"


def build_prompt(
    text: object,
    target_lang: str | None,
    source_lang: str | None = None,
    context: object = None,
    glossary: object = None,
    form: str | None = None,
    history: object = None,
) -> str:
    source = _clean(text)
    if not source:
        raise ValueError("訳す文が空です。")
    if target_lang not in LANGUAGES:
        raise ValueError(f"訳先の言語が対応表にありません: {target_lang}")
    if source_lang and source_lang not in LANGUAGES:
        raise ValueError(f"原文の言語が対応表にありません: {source_lang}")
    if form and form not in ("plain", "polite"):
        raise ValueError(f"文体は plain か polite です: {form}")

    prompt = f"{TAG}dataset{MSG}translation"
    context_text = _clean(context)
    if context_text:
        prompt += f"{TAG}context{MSG}{context_text}"
    for pair in history if isinstance(history, list) else []:
        if not isinstance(pair, dict):
            continue
        pair_source = _clean(pair.get("source"))
        pair_translation = _clean(pair.get("translation"))
        if not pair_source or not pair_translation:
            continue
        prompt += _input_element(pair_source, source_lang)
        prompt += _output_element(pair_translation, target_lang)
    prompt += _glossary_element(glossary)
    prompt += _input_element(source, source_lang)
    prompt += _output_element("", target_lang, form)
    return prompt


def clean_translation(raw: object) -> str:
    text = "" if raw is None else str(raw)
    match = CONTROL_RE.search(text)
    if match:
        text = text[: match.start()]
    return text.strip()
