/**
 * PLaMo 3 Translate が受け付ける言語コード。
 * 出典: https://huggingface.co/pfnet/plamo-3-translate-0.8b
 * 簡体字と繁体字は別コード。Hugging Face の表示は中国語を1つにまとめるため 52 と出るが、実際は 53。
 */
export const LANGUAGES = [
  { code: "ja", label: "日本語", native: "日本語" },
  { code: "en", label: "英語", native: "English" },
  { code: "zh-hans", label: "中国語 (簡体字)", native: "中文（简体）" },
  { code: "zh-hant", label: "中国語 (繁体字)", native: "中文（繁體）" },
  { code: "ko", label: "韓国語", native: "한국어" },
  { code: "ar", label: "アラビア語", native: "العربية" },
  { code: "az", label: "アゼルバイジャン語", native: "Azərbaycan" },
  { code: "be", label: "ベラルーシ語", native: "Беларуская" },
  { code: "bg", label: "ブルガリア語", native: "Български" },
  { code: "bn", label: "ベンガル語", native: "বাংলা" },
  { code: "ca", label: "カタルーニャ語", native: "Català" },
  { code: "ceb", label: "セブアノ語", native: "Cebuano" },
  { code: "de", label: "ドイツ語", native: "Deutsch" },
  { code: "es", label: "スペイン語", native: "Español" },
  { code: "et", label: "エストニア語", native: "Eesti" },
  { code: "eu", label: "バスク語", native: "Euskara" },
  { code: "fa", label: "ペルシア語", native: "فارسی" },
  { code: "fr", label: "フランス語", native: "Français" },
  { code: "gu", label: "グジャラート語", native: "ગુજરાતી" },
  { code: "he", label: "ヘブライ語", native: "עברית" },
  { code: "hi", label: "ヒンディー語", native: "हिन्दी" },
  { code: "hr", label: "クロアチア語", native: "Hrvatski" },
  { code: "hu", label: "ハンガリー語", native: "Magyar" },
  { code: "hy", label: "アルメニア語", native: "Հայերեն" },
  { code: "id", label: "インドネシア語", native: "Indonesia" },
  { code: "it", label: "イタリア語", native: "Italiano" },
  { code: "jv", label: "ジャワ語", native: "Basa Jawa" },
  { code: "ka", label: "ジョージア語", native: "ქართული" },
  { code: "kk", label: "カザフ語", native: "Қазақ" },
  { code: "km", label: "クメール語", native: "ខ្មែរ" },
  { code: "lt", label: "リトアニア語", native: "Lietuvių" },
  { code: "lv", label: "ラトビア語", native: "Latviešu" },
  { code: "ml", label: "マラヤーラム語", native: "മലയാളം" },
  { code: "mr", label: "マラーティー語", native: "मराठी" },
  { code: "ms", label: "マレー語", native: "Melayu" },
  { code: "my", label: "ビルマ語", native: "မြန်မာ" },
  { code: "ne", label: "ネパール語", native: "नेपाली" },
  { code: "nl", label: "オランダ語", native: "Nederlands" },
  { code: "pa", label: "パンジャブ語", native: "ਪੰਜਾਬੀ" },
  { code: "pl", label: "ポーランド語", native: "Polski" },
  { code: "ru", label: "ロシア語", native: "Русский" },
  { code: "si", label: "シンハラ語", native: "සිංහල" },
  { code: "sk", label: "スロバキア語", native: "Slovenčina" },
  { code: "sl", label: "スロベニア語", native: "Slovenščina" },
  { code: "sr", label: "セルビア語", native: "Српски" },
  { code: "sw", label: "スワヒリ語", native: "Kiswahili" },
  { code: "ta", label: "タミル語", native: "தமிழ்" },
  { code: "te", label: "テルグ語", native: "తెలుగు" },
  { code: "th", label: "タイ語", native: "ไทย" },
  { code: "tl", label: "タガログ語", native: "Tagalog" },
  { code: "ur", label: "ウルドゥー語", native: "اردو" },
  { code: "uz", label: "ウズベク語", native: "Oʻzbek" },
  { code: "vi", label: "ベトナム語", native: "Tiếng Việt" },
];

const CODES = new Set(LANGUAGES.map((language) => language.code));

export function isSupportedLanguage(code) {
  return CODES.has(code);
}

export function languageByCode(code) {
  return LANGUAGES.find((language) => language.code === code) || null;
}
