//! PFML3 プロンプトの実装。
//!
//! 並びは PLaMo 3 Translate のモデルカードの Completions API 例に合わせ、
//! 制御トークンの落とし方、長文の分割、訳文の連結の規則を定める。
//!
//! 原文はページ由来なので、区切りを偽装されうる。制御トークンは名前を問わず落とし、
//! 落とした結果が空になったら訳さない。

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const TAG: &str = "<|plamo:tag|>";
pub const MSG: &str = "<|plamo:msg|>";
pub const KEY: &str = "<|plamo:key|>";
pub const VAL: &str = "<|plamo:val|>";
pub const INDEX: &str = "<|plamo:index|>";
pub const ITEM: &str = "<|plamo:item|>";

/// 生成を止めるトークン。モデルカードの例と同じ 2 つ。
pub const STOP_TOKENS: [&str; 2] = [TAG, "<|plamo:bos|>"];

/// 1 リクエストに載せる原文の上限 (文字数)。
pub const CHUNK_LIMIT: usize = 400;

/// 受け付ける言語コード。
/// 出典: https://huggingface.co/pfnet/plamo-3-translate-0.8b
/// 簡体字と繁体字を別コードで数えるので 53。
pub const LANGUAGES: [&str; 53] = [
    "ja", "en", "zh-hans", "zh-hant", "ko", "ar", "az", "be", "bg", "bn", "ca", "ceb", "de", "es",
    "et", "eu", "fa", "fr", "gu", "he", "hi", "hr", "hu", "hy", "id", "it", "jv", "ka", "kk", "km",
    "lt", "lv", "ml", "mr", "ms", "my", "ne", "nl", "pa", "pl", "ru", "si", "sk", "sl", "sr", "sw",
    "ta", "te", "th", "tl", "ur", "uz", "vi",
];

pub fn is_supported_language(code: &str) -> bool {
    LANGUAGES.contains(&code)
}

/// `<|plamo:xxx|>` をすべて対象にする。予約トークンは `<|plamo:reserved:0x106|>` のように
/// コロンを含むので、名前の文字種を狭めない。
static CONTROL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<\|plamo:[a-z0-9_:.\-]+\|>").expect("制御トークンの正規表現は固定")
});

/// JS の `/\p{Lu}/u` と同じ判定。
static UPPERCASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Lu}").expect("大文字判定の正規表現は固定"));

/// 欧文のピリオドを文末と取り違えないための略語。
const ABBREVIATIONS: [&str; 24] = [
    "mr", "mrs", "ms", "dr", "prof", "st", "jr", "sr", "vs", "etc", "inc", "ltd", "co", "no",
    "fig", "eq", "ref", "approx", "dept", "est", "min", "max", "vol", "ex",
];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PromptError {
    #[error("訳す文が空です。")]
    EmptyText,
    #[error("訳先の言語が対応表にありません: {0}")]
    UnsupportedTarget(String),
    #[error("原文の言語が対応表にありません: {0}")]
    UnsupportedSource(String),
    #[error("文体は plain か polite です: {0}")]
    InvalidForm(String),
}

/// glossary の 1 対。IPC でもそのまま受ける。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryPair {
    pub source: String,
    pub target: String,
}

/// 直前に訳した文の対。文書順 (古いものが先)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryPair {
    pub source: String,
    pub translation: String,
}

/// `build_prompt` の入力。省略できるものは既定値 (None / 空スライス) で埋める。
#[derive(Debug, Clone, Default)]
pub struct PromptOptions<'a> {
    pub text: &'a str,
    pub target_lang: &'a str,
    pub source_lang: Option<&'a str>,
    pub context: Option<&'a str>,
    pub glossary: &'a [GlossaryPair],
    /// `"plain"` / `"polite"`。訳先が `ja` のときだけ効く。
    pub form: Option<&'a str>,
    pub history: &'a [HistoryPair],
}

/// 公式カードと同じ並びでプロンプトを組み立てる。
///
/// `dataset` → `context?` → 履歴の `input` / `output` → `glossary?` → 今回の `input` →
/// 空の `output`。
pub fn build_prompt(options: &PromptOptions<'_>) -> Result<String, PromptError> {
    let source = clean(Some(options.text)).ok_or(PromptError::EmptyText)?;
    if !is_supported_language(options.target_lang) {
        return Err(PromptError::UnsupportedTarget(
            options.target_lang.to_string(),
        ));
    }
    if let Some(source_lang) = options
        .source_lang
        .filter(|code| !is_supported_language(code))
    {
        return Err(PromptError::UnsupportedSource(source_lang.to_string()));
    }
    if let Some(form) = options
        .form
        .filter(|form| *form != "plain" && *form != "polite")
    {
        return Err(PromptError::InvalidForm(form.to_string()));
    }

    let mut prompt = format!("{TAG}dataset{MSG}translation");

    if let Some(context) = clean(options.context) {
        prompt.push_str(&format!("{TAG}context{MSG}{context}"));
    }

    for pair in options.history {
        let pair_source = clean(Some(&pair.source));
        let pair_translation = clean(Some(&pair.translation));
        if let (Some(pair_source), Some(pair_translation)) = (pair_source, pair_translation) {
            prompt.push_str(&input_element(&pair_source, options.source_lang));
            prompt.push_str(&output_element(
                &pair_translation,
                options.target_lang,
                None,
            ));
        }
    }

    prompt.push_str(&glossary_element(options.glossary));
    prompt.push_str(&input_element(&source, options.source_lang));
    prompt.push_str(&output_element("", options.target_lang, options.form));
    Ok(prompt)
}

/// 生成結果から制御トークン以降を落とし、前後の空白だけ取る。
/// 改行は訳文の一部なので残す。
pub fn clean_translation(raw: &str) -> String {
    match CONTROL_RE.find(raw) {
        Some(found) => trim_js(&raw[..found.start()]).to_string(),
        None => trim_js(raw).to_string(),
    }
}

/// `<|plamo:...|>` を、入れ子になっていても平文だけが残るまで落とす。
pub fn strip_control_tokens(value: &str) -> String {
    let mut text = value.to_string();
    loop {
        let stripped = CONTROL_RE.replace_all(&text, "").into_owned();
        if stripped == text {
            return text;
        }
        text = stripped;
    }
}

/// モデルへ渡せる文字列だけを返す。空になったら `None`。
fn clean(value: Option<&str>) -> Option<String> {
    let text = strip_control_tokens(value.unwrap_or_default());
    let trimmed = trim_js(&text);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn lang_attr(code: Option<&str>) -> String {
    match code {
        Some(code) if is_supported_language(code) => format!("{KEY}lang{VAL}{code}"),
        _ => String::new(),
    }
}

fn input_element(text: &str, source_lang: Option<&str>) -> String {
    format!(
        "{TAG}input{}{MSG}{}",
        lang_attr(source_lang),
        trim_js(&strip_control_tokens(text))
    )
}

fn output_element(text: &str, target_lang: &str, form: Option<&str>) -> String {
    let style = match form {
        Some(form) if target_lang == "ja" => format!("{KEY}form{VAL}{form}"),
        _ => String::new(),
    };
    format!(
        "{TAG}output{KEY}lang{VAL}{target_lang}{style}{MSG}{}",
        trim_js(&strip_control_tokens(text))
    )
}

fn glossary_element(glossary: &[GlossaryPair]) -> String {
    let pairs: Vec<(String, String)> = glossary
        .iter()
        .filter_map(|pair| {
            let source = clean(Some(&pair.source))?;
            let target = clean(Some(&pair.target))?;
            Some((source, target))
        })
        .collect();
    if pairs.is_empty() {
        return String::new();
    }
    let body: String = pairs
        .iter()
        .map(|(source, target)| format!("{INDEX}{source}{ITEM}{target}"))
        .collect();
    format!("{TAG}glossary{body}")
}

/// JS の `\s` と同じ集合。Unicode の White_Space とは BOM の扱いだけが違う。
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// JS の `String.prototype.trim()` と同じ範囲を落とす。
fn trim_js(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn char_count(value: &str) -> usize {
    value.chars().count()
}

/// 1 リクエストに載せる原文の上限で分ける。
///
/// 句点 (`。！？!?`) と欧文のピリオドで文に切ってから、`limit` 文字まで詰める。
/// ピリオドは、次の語が大文字で始まり、直前の語が略語でもイニシャルでもないときだけ
/// 文末とみなす。それでも長すぎる文は `limit` 文字ずつに割る。
pub fn split_text(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let source = trim_js(text);
    if source.is_empty() {
        return Vec::new();
    }
    // JS の `source.length` は UTF-16 の符号単位で数える。ここだけは同じ基準に合わせる。
    if utf16_len(source) <= limit {
        return vec![source.to_string()];
    }

    let characters: Vec<char> = source.chars().collect();
    let mut sentences: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut last_space: Option<usize> = None;

    for (index, character) in characters.iter().enumerate() {
        if is_js_whitespace(*character) {
            last_space = Some(index);
        }
        if is_sentence_end(&characters, index, last_space) {
            sentences.push(characters[start..=index].iter().collect());
            start = index + 1;
        }
    }
    if start < characters.len() {
        sentences.push(characters[start..].iter().collect());
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for sentence in &sentences {
        if !current.is_empty() && char_count(&current) + char_count(sentence) > limit {
            let trimmed = trim_js(&current);
            if !trimmed.is_empty() {
                chunks.push(trimmed.to_string());
            }
            current = sentence.clone();
        } else {
            current.push_str(sentence);
        }
    }
    let trimmed = trim_js(&current);
    if !trimmed.is_empty() {
        chunks.push(trimmed.to_string());
    }

    let mut result: Vec<String> = Vec::new();
    for chunk in chunks {
        let letters: Vec<char> = chunk.chars().collect();
        if letters.len() <= limit {
            result.push(chunk);
            continue;
        }
        for slice in letters.chunks(limit) {
            result.push(slice.iter().collect());
        }
    }
    result
}

fn is_sentence_end(characters: &[char], index: usize, last_space: Option<usize>) -> bool {
    let character = characters[index];
    if "。！？".contains(character) || character == '!' || character == '?' {
        return true;
    }
    if character != '.' {
        return false;
    }
    let Some(next) = characters.get(index + 1) else {
        return true;
    };
    if !is_js_whitespace(*next) {
        // 小数やドメインは割らない
        return false;
    }
    let following: String = characters[index + 1..].iter().collect();
    let Some(following) = following
        .trim_start_matches(is_js_whitespace)
        .chars()
        .next()
    else {
        return false;
    };
    if !UPPERCASE_RE.is_match(&following.to_string()) {
        return false;
    }
    let from = last_space.map_or(0, |space| space + 1);
    if from > index {
        return false;
    }
    let word: String = characters[from..index].iter().collect();
    let word: String = word
        .to_lowercase()
        .chars()
        .filter(|character| character.is_ascii_lowercase() || *character == '.')
        .collect();
    if word.is_empty() {
        return false;
    }
    if ABBREVIATIONS.contains(&word.as_str()) {
        return false;
    }
    // イニシャル
    word.chars().count() != 1
}

/// 訳文を連結するときの区切り。日本語と中国語は語間の空白を入れない。
/// ストリーミング中にチャンクの頭へ前置する区切りもこれを使う。
pub fn translation_separator(target_lang: &str) -> &'static str {
    match target_lang {
        "ja" | "zh-hans" | "zh-hant" => "",
        _ => " ",
    }
}

/// 訳文を連結する。日本語と中国語は語間の空白を入れない。韓国語は分かち書きする。
pub fn join_translations(parts: &[String], target_lang: &str) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(translation_separator(target_lang))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(options: PromptOptions<'_>) -> String {
        build_prompt(&options).expect("プロンプトを組み立てられる")
    }

    #[test]
    fn single_sentence_matches_the_model_card() {
        assert_eq!(
            build(PromptOptions {
                text: "会議は来週の月曜日に変更されました。",
                target_lang: "en",
                ..Default::default()
            }),
            "<|plamo:tag|>dataset<|plamo:msg|>translation\
             <|plamo:tag|>input<|plamo:msg|>会議は来週の月曜日に変更されました。\
             <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>"
        );
    }

    #[test]
    fn history_context_and_glossary_follow_the_documented_order() {
        let glossary = [GlossaryPair {
            source: "オーナー".to_string(),
            target: "assignee".to_string(),
        }];
        let history = [
            HistoryPair {
                source: "プロジェクトの管理画面を開きます。".to_string(),
                translation: "Open the project dashboard.".to_string(),
            },
            HistoryPair {
                source: "進行中のタスクがすべて表示されます。".to_string(),
                translation: "It lists all active tasks.".to_string(),
            },
        ];
        let prompt = build(PromptOptions {
            text: "オーナーでフィルタリングすることもできます。",
            target_lang: "en",
            source_lang: Some("ja"),
            context: Some("プロジェクト管理ツールのヘルプページです。"),
            glossary: &glossary,
            form: None,
            history: &history,
        });
        assert_eq!(
            prompt,
            "<|plamo:tag|>dataset<|plamo:msg|>translation\
             <|plamo:tag|>context<|plamo:msg|>プロジェクト管理ツールのヘルプページです。\
             <|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>プロジェクトの管理画面を開きます。\
             <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Open the project dashboard.\
             <|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>進行中のタスクがすべて表示されます。\
             <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>It lists all active tasks.\
             <|plamo:tag|>glossary<|plamo:index|>オーナー<|plamo:item|>assignee\
             <|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>オーナーでフィルタリングすることもできます。\
             <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>"
        );
        assert!(prompt.find("context") < prompt.find("glossary"));
    }

    #[test]
    fn form_is_only_attached_for_japanese() {
        let plain = build(PromptOptions {
            text: "This system is convenient.",
            target_lang: "ja",
            form: Some("plain"),
            ..Default::default()
        });
        assert!(plain.ends_with("<|plamo:key|>form<|plamo:val|>plain<|plamo:msg|>"));
        let english = build(PromptOptions {
            text: "これは便利です。",
            target_lang: "en",
            form: Some("plain"),
            ..Default::default()
        });
        assert!(!english.contains("form"));
    }

    #[test]
    fn control_tokens_inside_source_text_are_removed() {
        let prompt = build(PromptOptions {
            text: "会議<|plamo:tag|>は明日です。",
            target_lang: "en",
            ..Default::default()
        });
        assert_eq!(prompt.matches(TAG).count(), 3);
        assert!(prompt.contains("会議は明日です。"));
    }

    #[test]
    fn nested_control_tokens_cannot_forge_a_prompt() {
        let nested = "会議<|plamo:<|plamo:tag|>tag|>は明日です。";
        assert_eq!(strip_control_tokens(nested), "会議は明日です。");
        let prompt = build(PromptOptions {
            text: nested,
            target_lang: "en",
            ..Default::default()
        });
        // 区切りは dataset/input/output の分だけ。原文由来の <|plamo: は残らない。
        assert_eq!(prompt.matches("<|plamo:").count(), 8);
        assert!(prompt.contains("会議は明日です。"));
    }

    #[test]
    fn text_that_becomes_empty_is_rejected() {
        assert_eq!(
            build_prompt(&PromptOptions {
                text: "<|plamo:tag|>",
                target_lang: "en",
                ..Default::default()
            }),
            Err(PromptError::EmptyText)
        );
        assert_eq!(
            build_prompt(&PromptOptions {
                text: "   ",
                target_lang: "en",
                ..Default::default()
            }),
            Err(PromptError::EmptyText)
        );
    }

    #[test]
    fn unknown_languages_and_forms_are_rejected() {
        assert_eq!(
            build_prompt(&PromptOptions {
                text: "hello",
                target_lang: "xx",
                ..Default::default()
            }),
            Err(PromptError::UnsupportedTarget("xx".to_string()))
        );
        assert_eq!(
            build_prompt(&PromptOptions {
                text: "hello",
                target_lang: "",
                ..Default::default()
            }),
            Err(PromptError::UnsupportedTarget(String::new()))
        );
        assert_eq!(
            build_prompt(&PromptOptions {
                text: "hello",
                target_lang: "en",
                source_lang: Some("xx"),
                ..Default::default()
            }),
            Err(PromptError::UnsupportedSource("xx".to_string()))
        );
        assert_eq!(
            build_prompt(&PromptOptions {
                text: "hello",
                target_lang: "ja",
                form: Some("casual"),
                ..Default::default()
            }),
            Err(PromptError::InvalidForm("casual".to_string()))
        );
    }

    #[test]
    fn malformed_glossary_and_history_are_skipped() {
        let glossary = [
            GlossaryPair::default(),
            GlossaryPair {
                source: String::new(),
                target: "x".to_string(),
            },
            GlossaryPair {
                source: "a".to_string(),
                target: "b".to_string(),
            },
        ];
        let history = [
            HistoryPair::default(),
            HistoryPair {
                source: " ".to_string(),
                translation: String::new(),
            },
        ];
        let prompt = build(PromptOptions {
            text: "hello",
            target_lang: "en",
            glossary: &glossary,
            history: &history,
            ..Default::default()
        });
        assert!(prompt.contains("<|plamo:index|>a<|plamo:item|>b"));
        assert_eq!(prompt.matches("<|plamo:tag|>input").count(), 1);
    }

    #[test]
    fn clean_translation_drops_the_stop_token() {
        assert_eq!(
            clean_translation(" 訳文です。\n<|plamo:tag|>extra"),
            "訳文です。"
        );
        assert_eq!(clean_translation("done<|plamo:bos|>"), "done");
        assert_eq!(
            clean_translation(" The meeting is Monday. \n<|plamo:tag|>"),
            "The meeting is Monday."
        );
        // 改行は訳文の一部なので残す
        assert_eq!(clean_translation("\n訳文\nです\n"), "訳文\nです");
    }

    #[test]
    fn stop_tokens_are_the_two_documented_ones() {
        assert_eq!(STOP_TOKENS, ["<|plamo:tag|>", "<|plamo:bos|>"]);
    }

    #[test]
    fn split_text_keeps_short_text_whole() {
        assert!(split_text("", 400).is_empty());
        assert!(split_text("   ", 400).is_empty());
        assert_eq!(split_text("短い文。", 400), vec!["短い文。".to_string()]);
    }

    #[test]
    fn split_text_breaks_at_sentence_marks_and_respects_the_limit() {
        let long = "あ。".repeat(300);
        let parts = split_text(&long, 400);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| char_count(part) <= 400));
        assert_eq!(parts.concat(), long);
    }

    #[test]
    fn split_text_does_not_break_abbreviations_or_initials() {
        let english = format!("{}Mr. Smith arrived.", "word ".repeat(90));
        let chunks = split_text(&english, 100);
        assert!(chunks.iter().all(|chunk| char_count(chunk) <= 100));
        assert!(chunks.iter().any(|chunk| chunk.contains("Mr. Smith")));
        assert!(!chunks.iter().any(|chunk| chunk.ends_with("Mr.")));
    }

    #[test]
    fn split_text_keeps_decimals_together() {
        let text = format!("The value is 3.14 and it stays. {}", "x".repeat(500));
        let chunks = split_text(&text, 60);
        assert!(chunks.iter().any(|chunk| chunk.contains("3.14")));
    }

    #[test]
    fn split_text_slices_overlong_sentences() {
        let text = "x".repeat(900);
        let chunks = split_text(&text, 400);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| char_count(chunk) <= 400));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn join_translations_omits_spaces_for_cjk() {
        let parts = vec!["あ".to_string(), "い".to_string()];
        assert_eq!(join_translations(&parts, "ja"), "あい");
        assert_eq!(join_translations(&parts, "zh-hant"), "あい");
        let parts = vec!["a".to_string(), "b".to_string()];
        assert_eq!(join_translations(&parts, "en"), "a b");
        let parts = vec![String::new(), "b".to_string()];
        assert_eq!(join_translations(&parts, "en"), "b");
    }

    #[test]
    fn every_supported_language_is_unique() {
        let mut codes = LANGUAGES.to_vec();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 53);
        assert!(is_supported_language("zh-hans"));
        assert!(!is_supported_language("zh"));
    }
}
