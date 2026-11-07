//! 原文の言語を推定する。
//!
//! whatlang の判定を 53 言語のコードに写す。対応表に無い言語は `None` を返し、
//! 訳先の判断はモデルに任せる (原文の lang 属性を付けない)。
//!
//! 中国語は whatlang が簡体字 / 繁体字を区別しない (`Lang::Cmn`) ので、
//! 字面で分ける。

use serde::Serialize;
use whatlang::{detect, Lang};

use crate::prompt::is_supported_language;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectedLanguage {
    pub code: String,
    pub confidence: f64,
}

/// 53 言語のどれかなら code と確度を返す。対応表の外なら `None`。
pub fn detect_language(text: &str) -> Option<DetectedLanguage> {
    let source = text.trim();
    if source.is_empty() {
        return None;
    }
    let info = detect(source)?;
    let code = language_code(info.lang(), source)?;
    if !is_supported_language(code) {
        return None;
    }
    Some(DetectedLanguage {
        code: code.to_string(),
        confidence: info.confidence(),
    })
}

/// whatlang の `Lang` を PLaMo 3 Translate の言語コードに写す。
fn language_code(lang: Lang, text: &str) -> Option<&'static str> {
    let code = match lang {
        Lang::Jpn => "ja",
        Lang::Eng => "en",
        Lang::Cmn => return Some(chinese_code(text)),
        Lang::Kor => "ko",
        Lang::Ara => "ar",
        Lang::Aze => "az",
        Lang::Bel => "be",
        Lang::Bul => "bg",
        Lang::Ben => "bn",
        Lang::Cat => "ca",
        Lang::Deu => "de",
        Lang::Spa => "es",
        Lang::Est => "et",
        Lang::Pes => "fa",
        Lang::Fra => "fr",
        Lang::Guj => "gu",
        Lang::Heb => "he",
        Lang::Hin => "hi",
        Lang::Hrv => "hr",
        Lang::Hun => "hu",
        Lang::Hye => "hy",
        Lang::Ind => "id",
        Lang::Ita => "it",
        Lang::Jav => "jv",
        Lang::Kat => "ka",
        Lang::Khm => "km",
        Lang::Lit => "lt",
        Lang::Lav => "lv",
        Lang::Mal => "ml",
        Lang::Mar => "mr",
        Lang::Mya => "my",
        Lang::Nep => "ne",
        Lang::Nld => "nl",
        Lang::Pan => "pa",
        Lang::Pol => "pl",
        Lang::Rus => "ru",
        Lang::Sin => "si",
        Lang::Slk => "sk",
        Lang::Slv => "sl",
        Lang::Srp => "sr",
        Lang::Tam => "ta",
        Lang::Tel => "te",
        Lang::Tha => "th",
        Lang::Tgl => "tl",
        Lang::Urd => "ur",
        Lang::Uzb => "uz",
        Lang::Vie => "vi",
        // 対応表に無い言語 (ウクライナ語・ノルウェー語など) と、
        // whatlang が持っていない言語 (セブアノ語・バスク語・カザフ語・マレー語・スワヒリ語)。
        _ => return None,
    };
    Some(code)
}

/// 繁体字でしか使わない字。簡体字と共通の字 (里・后・云・面 など) は入れない。
const TRADITIONAL_HINTS: &[char] = &[
    '們', '這', '說', '開', '個', '為', '來', '時', '會', '對', '國', '學', '過', '裡', '麼', '樣',
    '發', '現', '見', '還', '進', '買', '賣', '讀', '寫', '聽', '話', '語', '錯', '點', '應', '該',
    '記', '認', '覺', '讓', '給', '從', '無', '與', '東', '車', '長', '門', '問', '間', '關', '陳',
    '陽', '電', '頭', '氣', '體', '數', '網', '歐', '亞', '龍', '業', '產', '親', '眾', '優', '衛',
    '歷', '廣', '義', '樂', '書', '億', '僅', '倉', '儀', '價', '偉', '傳', '傷', '倫', '傑', '兩',
    '嚴', '鳳', '劉', '動', '務', '勝', '區', '華', '單', '壓', '參', '雙', '變', '響', '聲', '頁',
    '風', '飛', '馬', '鳥', '魚', '齊', '齒', '龜', '貝', '農', '邊', '運', '達', '遠', '適', '選',
    '遺', '郵', '鄉', '醫', '釋', '鐘', '鐵', '銀', '錢', '鍵', '閉', '閱', '際', '陸', '隨', '險',
    '隱', '雜', '難', '靈', '靜', '頂', '項', '順', '須', '預', '領', '題', '顏', '願', '類', '顯',
    '飯', '飲', '館', '駕', '驗', '麗', '黃', '舉', '興', '習',
];

/// 簡体字でしか使わない字。
const SIMPLIFIED_HINTS: &[char] = &[
    '们', '这', '说', '开', '个', '为', '来', '时', '会', '对', '国', '学', '过', '么', '样', '发',
    '现', '见', '还', '进', '买', '卖', '读', '写', '听', '话', '语', '错', '点', '应', '该', '记',
    '认', '觉', '让', '给', '从', '无', '与', '东', '车', '长', '门', '问', '间', '关', '陈', '阳',
    '电', '头', '气', '体', '数', '网', '欧', '亚', '龙', '业', '产', '亲', '众', '优', '卫', '历',
    '广', '义', '乐', '书', '亿', '仅', '仓', '仪', '价', '伟', '传', '伤', '伦', '杰', '两', '严',
    '凤', '刘', '动', '务', '胜', '区', '华', '单', '压', '参', '双', '变', '响', '声', '页', '风',
    '飞', '马', '鸟', '鱼', '齐', '齿', '龟', '贝', '农', '边', '运', '达', '远', '适', '选', '遗',
    '邮', '乡', '医', '释', '钟', '铁', '银', '钱', '键', '闭', '阅', '际', '陆', '随', '险', '隐',
    '杂', '难', '灵', '静', '顶', '项', '顺', '须', '预', '领', '题', '颜', '愿', '类', '显', '饭',
    '饮', '馆', '驾', '验', '丽', '黄', '举', '兴', '习',
];

/// 繁体字の字面が多ければ繁体字。判定材料が無ければ簡体字として扱う。
fn chinese_code(text: &str) -> &'static str {
    if count_hits(text, TRADITIONAL_HINTS) > count_hits(text, SIMPLIFIED_HINTS) {
        "zh-hant"
    } else {
        "zh-hans"
    }
}

fn count_hits(text: &str, hints: &[char]) -> usize {
    text.chars()
        .filter(|character| hints.contains(character))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_japanese() {
        let detected = detect_language(
            "会議は来週の月曜日に変更されました。資料は前日までに共有してください。",
        )
        .expect("日本語を判定できる");
        assert_eq!(detected.code, "ja");
        assert!((0.0..=1.0).contains(&detected.confidence));
    }

    #[test]
    fn detects_english() {
        let detected = detect_language(
            "The project meeting has been moved to next Monday morning. Please share the slides.",
        )
        .expect("英語を判定できる");
        assert_eq!(detected.code, "en");
    }

    #[test]
    fn detects_korean() {
        let detected = detect_language(
            "회의가 다음 주 월요일 오전으로 변경되었습니다. 자료는 전날까지 공유해 주세요.",
        )
        .expect("韓国語を判定できる");
        assert_eq!(detected.code, "ko");
    }

    #[test]
    fn splits_chinese_by_character_forms() {
        assert_eq!(
            chinese_code("會議已經改到下週一上午。請分享資料。"),
            "zh-hant"
        );
        assert_eq!(
            chinese_code("会议已经改到下周一上午。请分享资料。"),
            "zh-hans"
        );
        // 判定材料が無いときは簡体字に寄せる
        assert_eq!(chinese_code("中文"), "zh-hans");
    }

    #[test]
    fn chinese_detection_returns_a_variant_inside_the_table() {
        let simplified = detect_language("会议已经改到下周一上午，请提前分享资料。")
            .expect("中国語を判定できる");
        assert!(
            simplified.code == "zh-hans" || simplified.code == "zh-hant",
            "{}",
            simplified.code
        );
        let traditional = detect_language("會議已經改到下週一上午，請提前分享資料。")
            .expect("中国語を判定できる");
        assert_eq!(traditional.code, "zh-hant", "{}", traditional.code);
    }

    #[test]
    fn unsupported_language_returns_none() {
        // ウクライナ語は whatlang が判定できるが、53 言語の対応表には無い。
        assert_eq!(
            detect_language("Зустріч перенесено на наступний понеділок."),
            None
        );
    }

    #[test]
    fn empty_text_returns_none() {
        assert_eq!(detect_language(""), None);
        assert_eq!(detect_language("   \n\t "), None);
    }

    #[test]
    fn every_detected_code_is_supported() {
        let samples = [
            "Bonjour, la réunion est reportée à lundi prochain.",
            "Die Besprechung wurde auf nächsten Montag verschoben.",
            "La reunión se ha trasladado al próximo lunes.",
            "会議は来週の月曜日に変更されました。",
        ];
        for sample in samples {
            if let Some(detected) = detect_language(sample) {
                assert!(is_supported_language(&detected.code), "{}", detected.code);
            }
        }
    }
}
