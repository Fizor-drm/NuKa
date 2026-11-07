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
