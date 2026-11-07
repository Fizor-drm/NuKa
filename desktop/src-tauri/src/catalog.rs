//! 設定画面に出す公式モデルカタログ (PFN の GGUF)。
//!
//! サイズは配布元 (Hugging Face) の LFS 情報から取得した目安で、表示専用。
//! ダウンロード時の検証は応答の Content-Length を使う (再アップロードで変わりうるため)。
//! 8b / 31b には Q4_0 が無いので 3 種ずつ、合計 14 種。

use serde::Serialize;

/// PLaMo Community License の案内ページ。
pub const LICENSE_URL: &str = "https://plamo.preferredai.jp/info/plamo-community-license-en";
pub const LICENSE_NAME: &str = "PLaMo Community License";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    /// `"0.8b-Q4_K_M"` のような一意な ID。
    pub id: String,
    /// `"0.8b"` / `"2b"` / `"8b"` / `"31b"`。
    pub size: String,
    /// `"Q2_K"` などの量子化名。
    pub quant: String,
    /// 表示用の目安バイト数。検証は GET 応答の Content-Length で行う。
    pub bytes: u64,
    pub url: String,
    /// `pfnet/plamo-3-translate-0.8b-GGUF`。
    pub repo: String,
    /// `plamo-3-translate-0.8b-Q4_K_M.gguf`。
    pub file: String,
}

/// (サイズ, 量子化, バイト数)。並び順がそのまま UI の並びになる。
const MODELS: [(&str, &str, u64); 14] = [
    ("0.8b", "Q2_K", 410_137_920),
    ("0.8b", "Q3_K_M", 486_384_960),
    ("0.8b", "Q4_0", 547_542_336),
    ("0.8b", "Q4_K_M", 573_125_952),
    ("2b", "Q2_K", 1_225_611_104),
    ("2b", "Q3_K_M", 1_495_885_664),
    ("2b", "Q4_0", 1_698_576_224),
    ("2b", "Q4_K_M", 1_807_202_144),
    ("8b", "Q2_K", 3_450_745_248),
    ("8b", "Q3_K_M", 4_310_217_120),
    ("8b", "Q4_K_M", 5_280_444_832),
    ("31b", "Q2_K", 12_609_923_616),
    ("31b", "Q3_K_M", 15_863_130_656),
    ("31b", "Q4_K_M", 19_526_593_056),
];

fn entry(size: &str, quant: &str, bytes: u64) -> CatalogEntry {
    CatalogEntry {
        id: format!("{size}-{quant}"),
        size: size.to_string(),
        quant: quant.to_string(),
        bytes,
        url: format!(
            "https://huggingface.co/pfnet/plamo-3-translate-{size}-GGUF/resolve/main/plamo-3-translate-{size}-{quant}.gguf"
        ),
        repo: format!("pfnet/plamo-3-translate-{size}-GGUF"),
        file: format!("plamo-3-translate-{size}-{quant}.gguf"),
    }
}

/// カタログ全件。
pub fn entries() -> Vec<CatalogEntry> {
    MODELS
        .iter()
        .map(|(size, quant, bytes)| entry(size, quant, *bytes))
        .collect()
}

/// ID (`"0.8b-Q4_K_M"`) から 1 件引く。
pub fn find(id: &str) -> Option<CatalogEntry> {
    entries().into_iter().find(|entry| entry.id == id)
}

/// 保存ファイル名からカタログを引く (削除時の整合性確認に使う)。
pub fn find_by_file(file: &str) -> Option<CatalogEntry> {
    entries().into_iter().find(|entry| entry.file == file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_14_entries_with_unique_ids() {
        let entries = entries();
        assert_eq!(entries.len(), 14);
        let mut ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 14);
    }

    #[test]
    fn known_sizes_match_the_distribution() {
        let entry = find("0.8b-Q4_K_M").expect("カタログにある");
        assert_eq!(entry.bytes, 573_125_952);
        assert_eq!(entry.file, "plamo-3-translate-0.8b-Q4_K_M.gguf");
        assert_eq!(entry.repo, "pfnet/plamo-3-translate-0.8b-GGUF");
        assert_eq!(
            entry.url,
            "https://huggingface.co/pfnet/plamo-3-translate-0.8b-GGUF/resolve/main/plamo-3-translate-0.8b-Q4_K_M.gguf"
        );
        assert_eq!(
            find("0.8b-Q2_K").expect("カタログにある").bytes,
            410_137_920
        );
        assert_eq!(
            find("31b-Q4_K_M").expect("カタログにある").bytes,
            19_526_593_056
        );
    }

    #[test]
    fn large_models_have_no_q4_0() {
        for size in ["8b", "31b"] {
            assert!(find(&format!("{size}-Q4_0")).is_none());
            assert!(find(&format!("{size}-Q4_K_M")).is_some());
        }
        // 0.8b と 2b には Q4_0 がある
        assert!(find("0.8b-Q4_0").is_some());
        assert!(find("2b-Q4_0").is_some());
    }

    #[test]
    fn unknown_id_is_none() {
        assert!(find("1b-Q4_K_M").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn find_by_file_matches_the_saved_name() {
        let entry = find_by_file("plamo-3-translate-2b-Q3_K_M.gguf").expect("カタログにある");
        assert_eq!(entry.id, "2b-Q3_K_M");
        assert!(find_by_file("unknown.gguf").is_none());
        assert!(find_by_file("").is_none());
    }
}
