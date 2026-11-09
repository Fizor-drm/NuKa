//! バックエンドからフロントエンドへ渡す、翻訳可能なメッセージ。
//!
//! `key` と `params` をフロントの辞書で解決し、辞書に無い場合は `fallback`
//! (日本語) をそのまま表示する。文字列そのものをイベントに載せないことで、
//! UI の表示言語を切り替えてもバックエンド由来の文言を追従できる。

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalizedText {
    pub key: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// 辞書に key が無いときのフォールバック (日本語)。
    pub fallback: String,
}

impl LocalizedText {
    pub fn new(key: impl Into<String>, fallback: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            params: BTreeMap::new(),
            fallback: fallback.into(),
        }
    }

    pub fn param(mut self, name: &str, value: impl ToString) -> Self {
        self.params.insert(name.to_string(), value.to_string());
        self
    }
}

/// 短く書くためのヘルパー。
pub fn text(key: &str, fallback: impl Into<String>) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

impl From<String> for LocalizedText {
    /// 既存の文字列をそのままフォールバックとして包む (コード未分類の経路用)。
    fn from(fallback: String) -> Self {
        Self::new("error.raw", fallback)
    }
}

impl From<&str> for LocalizedText {
    fn from(fallback: &str) -> Self {
        Self::new("error.raw", fallback)
    }
}

/// ネイティブ UI (トレイメニューなど、フロントエンドの辞書を通らない場所) 向けに、
/// 設定言語 (`settings.ui_language`) に合わせて三言語から 1 つ選ぶ。
///
/// 未知の言語コードは既定 (日本語) に落とす。WebView 側の `localize()` と同じ
/// 規則で、表示言語の切替に追従させるための関数。
pub fn render<'a>(language: &str, ja: &'a str, en: &'a str, zh: &'a str) -> &'a str {
    match language {
        "en" => en,
        "zh" => zh,
        _ => ja,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_labels_follow_the_ui_language() {
        assert_eq!(render("ja", "終了", "Quit", "退出"), "終了");
        assert_eq!(render("en", "終了", "Quit", "退出"), "Quit");
        assert_eq!(render("zh", "終了", "Quit", "退出"), "退出");
        // 未知・空は日本語 (既定) に落とす。
        assert_eq!(render("", "終了", "Quit", "退出"), "終了");
        assert_eq!(render("fr", "終了", "Quit", "退出"), "終了");
    }

    #[test]
    fn localized_text_carries_key_params_and_fallback() {
        let message = text("tray.quit", "終了").param("detail", "x");
        assert_eq!(message.key, "tray.quit");
        assert_eq!(message.fallback, "終了");
        assert_eq!(message.params.get("detail").map(String::as_str), Some("x"));
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["key"], "tray.quit");
        assert_eq!(value["fallback"], "終了");
        assert_eq!(value["params"]["detail"], "x");
    }
}
