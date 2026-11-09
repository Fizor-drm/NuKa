//! アプリ設定の永続化。
//!
//! `{app_config_dir}/settings.json` に置く。壊れていたら既定値で作り直す。
//! エンジンの起動引数に関わる値 (モデルの場所・ngl・context) は、ここを直したあと
//! `restart_engine` を呼ばないと反映されない。
//!
//! 実行パス (`model_path` / `server_path`) は IPC からは変更できない。WebView から
//! 実行バイナリを差し替えられないよう、変更は settings.json の手編集と環境変数
//! (`PLAMO_MODEL_PATH` / `PLAMO_LLAMA_SERVER`) だけに限定する。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::prompt::GlossaryPair;

pub const MIN_CONTEXT_TOKENS: u32 = 512;
pub const MAX_CONTEXT_TOKENS: u32 = 32768;
pub const MAX_GPU_LAYERS: u32 = 1000;

/// 保存する用語集の上限 (対)。
///
/// 1 対は 2 つの短い文字列なので 200 対でも settings.json は数十 KB に収まる。
/// 手編集や UI 側の不具合で無限に増えないよう、保存時にここで頭打ちにする。
pub const MAX_GLOSSARY_PAIRS: usize = 200;

/// UI 表示言語。既定は日本語。
pub const UI_LANGUAGE_DEFAULT: &str = "ja";
pub const UI_LANGUAGES: [&str; 3] = ["ja", "en", "zh"];

pub fn is_supported_ui_language(value: &str) -> bool {
    UI_LANGUAGES.contains(&value)
}

/// 保存用の一時ファイル名に付ける連番。同じプロセス内の同時保存でも衝突させない。
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Light,
    Dark,
    System,
}

/// 設定画面から導入した llama.cpp ランタイム。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRuntime {
    pub server_path: String,
    pub version: String,
    /// Unix 秒。
    pub installed_at: u64,
}

/// 設定画面から導入したモデル。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub file: String,
    pub repo: String,
    pub bytes: u64,
    pub path: String,
    /// Unix 秒。
    pub installed_at: u64,
}

/// フローティング翻訳ウィンドウ (Phase 1) の設定。
///
/// `#[serde(default)]` を付けてあるので、古い settings.json にこの項目が無くても
/// 既定 (有効 / Ctrl+Shift+T / 専用モデルなし) で読み込める。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FloatingWindowSettings {
    /// グローバルショートカットでフローティングウィンドウを出すか。
    pub enabled: bool,
    /// ホットキー。Ctrl / Alt / Shift + 英数字 1 文字。
    pub hotkey: String,
    /// フローティング用エンジンが使うモデル (`installed_models` の file)。
    ///
    /// `None` ならアクティブモデルと同じ (従来動作)。小型モデルを指定すると、
    /// 常時起動しても VRAM を抑えられる。
    pub model: Option<String>,
}

impl Default for FloatingWindowSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: crate::floating::DEFAULT_HOTKEY.to_string(),
            model: None,
        }
    }
}

/// バックグラウンド実行 (トレイ常駐) の設定。
///
/// `#[serde(default)]` を付けてあるので、古い settings.json にこの項目が無くても
/// 既定 (常駐する / フローティング用エンジンは常時起動) で読み込める。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackgroundSettings {
    /// メイン画面を閉じてもプロセスを残し、トレイに常駐するか。
    pub enabled: bool,
    /// フローティング用エンジンを、アプリ起動中ずっと起動しておくか。
    ///
    /// 既定は true。ただし実際に常時起動するのは「フローティング専用モデル」が
    /// 選ばれているときだけ (`floating_engine_resident`)。未指定ならアクティブ
    /// モデルの二重ロードを避けるためオンデマンドになる。専用モデルがアクティブ
    /// と同じファイルなら 2 本目は起こさず、共有しているメイン用エンジン 1 本を
    /// 維持する (`engine::resident_target`)。メイン用エンジンは、この設定が ON で
    /// 共有モデルを保持しているときを除き、メイン画面を閉じたら止める
    /// (VRAM の解放)。
    pub resident_engine: bool,
}

impl Default for BackgroundSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            resident_engine: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// llama-server に渡す GGUF。空 (null) なら自動探索。
    /// IPC では変更できず、settings.json の手編集と環境変数だけで指定できる。
    pub model_path: Option<String>,
    /// llama-server.exe。空 (null) なら自動探索。
    /// IPC では変更できず、settings.json の手編集と環境変数だけで指定できる。
    pub server_path: Option<String>,
    /// `-ngl` に渡すレイヤ数。99 は実質すべての層。
    pub gpu_layers: u32,
    /// `-c` に渡すコンテキスト長。
    pub context_tokens: u32,
    pub theme: Theme,
    pub auto_translate: bool,
    /// 起動時に GitHub の最新リリースを確認するか (既定 true)。
    pub auto_update_check: bool,
    /// UI の表示言語 ("ja" / "en" / "zh")。
    pub ui_language: String,
    /// モデルの保存先。null なら `{app_local_data_dir}/models`。
    /// IPC では設定画面のフォルダ選択からだけ変更できる。
    pub models_dir: Option<String>,
    /// 設定画面から導入したランタイム。
    pub installed_runtime: Option<InstalledRuntime>,
    /// 設定画面から導入したモデル一覧。
    pub installed_models: Vec<InstalledModel>,
    /// 使用するモデルのファイル名 (installed_models の file)。
    pub active_model: Option<String>,
    /// フローティング翻訳ウィンドウ。
    pub floating_window: FloatingWindowSettings,
    /// バックグラウンド実行 (トレイ常駐)。
    pub background: BackgroundSettings,
    /// 翻訳に使う用語集 (単一セット)。
    ///
    /// 構造体側の `#[serde(default)]` により、1.3.5 以前の settings.json
    /// (glossary が無い) は空の用語集として読める。保存する形は
    /// [`normalize_glossary`] で整える (空行は保存しない・前後の空白は落とす)。
    /// 用語集は翻訳プロンプトだけに効き、エンジンの起動引数には関与しない。
    pub glossary: Vec<GlossaryPair>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model_path: None,
            server_path: None,
            gpu_layers: 99,
            context_tokens: 8192,
            theme: Theme::Light,
            auto_translate: true,
            auto_update_check: true,
            ui_language: UI_LANGUAGE_DEFAULT.to_string(),
            models_dir: None,
            installed_runtime: None,
            installed_models: Vec::new(),
            active_model: None,
            floating_window: FloatingWindowSettings::default(),
            background: BackgroundSettings::default(),
            glossary: Vec::new(),
        }
    }
}

impl Settings {
    /// フローティング用エンジンを常時起動する設定か。
    ///
    /// **フローティング専用モデル (`floating_window.model`) が選ばれていること**が
    /// 条件。未指定のまま常時起動すると、アクティブモデルをメイン用と二重に
    /// 読み込んで VRAM を倍使いするため、その場合は従来どおりオンデマンドにする。
    ///
    /// トレイ常駐 (`background.enabled`) を切っているときは「起動中ずっと」の
    /// 意味が無いので含める。
    ///
    /// この predicate が常時起動の設定条件。保持先 (メイン用 1 本かフローティング
    /// 用か) の決定は `engine::resident_target` が行い、起動・設定変更・メイン
    /// 非表示・フローティング終了のすべての経路でそれを使う。
    pub fn floating_engine_resident(&self) -> bool {
        self.background.enabled
            && self.background.resident_engine
            && self.floating_window.enabled
            && self.floating_window.model.is_some()
    }
}

/// `set_settings` が受けるバックグラウンド設定の差分。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct BackgroundSettingsPatch {
    pub enabled: Option<bool>,
    pub resident_engine: Option<bool>,
}

impl BackgroundSettingsPatch {
    fn is_empty(self) -> bool {
        self.enabled.is_none() && self.resident_engine.is_none()
    }
}

/// `set_settings` が受ける差分。省略したフィールドは今の値を保つ。
///
/// 実行パス (`model_path` / `server_path`) は意図的に含めない。IPC から
/// 実行バイナリのパスを差し替えられないようにするため。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SettingsPatch {
    pub gpu_layers: Option<u32>,
    pub context_tokens: Option<u32>,
    pub theme: Option<Theme>,
    pub auto_translate: Option<bool>,
    pub auto_update_check: Option<bool>,
    pub ui_language: Option<String>,
    pub background: Option<BackgroundSettingsPatch>,
    /// 用語集。省略すると今の値を保ち、空配列を渡すと全部消す。
    ///
    /// 保存前に [`normalize_glossary`] で整列するので、UI の空行が
    /// settings.json に残ることはない。
    pub glossary: Option<Vec<GlossaryPair>>,
}

#[derive(Clone)]
pub struct SettingsStore {
    /// `None` なら永続化しない (保存先が使えない環境向けのメモリ内ストア)。
    path: Option<PathBuf>,
    settings: Arc<Mutex<Settings>>,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        let settings = load_or_default(&path);
        Self {
            path: Some(path),
            settings: Arc::new(Mutex::new(settings)),
        }
    }

    /// 保存先が使えない環境向けのストア。変更はプロセス内にだけ残り、再起動で消える。
    pub fn in_memory() -> Self {
        Self {
            path: None,
            settings: Arc::new(Mutex::new(Settings::default())),
        }
    }

    /// `{app_config_dir}/settings.json` を使うストアを作る。
    pub fn for_app(app: &AppHandle) -> Result<Self, String> {
        let directory = app
            .path()
            .app_config_dir()
            .map_err(|error| format!("設定の保存先を取得できません: {error}"))?;
        Ok(Self::new(directory.join("settings.json")))
    }

    /// 保存先。メモリ内ストアなら `None`。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn get(&self) -> Settings {
        self.lock().clone()
    }

    /// 差分を適用する。
    ///
    /// lock を取ったまま「読む → 差分適用 → 検証 → 保存 → メモリ反映」まで行う。
    /// 並行して `set_settings` が呼ばれても更新が混ざらない (lost update を防ぐ)。
    pub fn apply(&self, patch: SettingsPatch) -> Result<Settings, String> {
        self.update(|settings| {
            if let Some(value) = patch.gpu_layers {
                settings.gpu_layers = value;
            }
            if let Some(value) = patch.context_tokens {
                settings.context_tokens = value;
            }
            if let Some(value) = patch.theme {
                settings.theme = value;
            }
            if let Some(value) = patch.auto_translate {
                settings.auto_translate = value;
            }
            if let Some(value) = patch.auto_update_check {
                settings.auto_update_check = value;
            }
            if let Some(value) = patch.ui_language {
                settings.ui_language = value;
            }
            if let Some(background) = patch.background.filter(|value| !value.is_empty()) {
                if let Some(enabled) = background.enabled {
                    settings.background.enabled = enabled;
                }
                if let Some(resident) = background.resident_engine {
                    settings.background.resident_engine = resident;
                }
            }
            if let Some(glossary) = patch.glossary {
                // UI の空行や前後の空白は保存しない。メモリもファイルと同じ形にする。
                settings.glossary = normalize_glossary(glossary);
            }
            Ok(())
        })
    }

    /// 任意の変更を lock 内で行い、検証して保存する。
    /// インストール系のコマンドはここを通して設定を更新する。
    pub fn update<F>(&self, change: F) -> Result<Settings, String>
    where
        F: FnOnce(&mut Settings) -> Result<(), String>,
    {
        let mut current = self.lock();
        let mut next = current.clone();
        change(&mut next)?;
        validate(&next)?;
        if let Some(path) = &self.path {
            save(path, &next)?;
        }
        *current = next.clone();
        Ok(next)
    }

    fn lock(&self) -> MutexGuard<'_, Settings> {
        self.settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn normalize_optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 用語集を保存できる形へ整える。
///
/// - 原文・訳語の前後の空白を落とす
/// - どちらかが空になった対は捨てる (UI の未入力行は保存しない)
/// - [`MAX_GLOSSARY_PAIRS`] を超えたぶんは後ろから切り捨てる
///
/// `set_settings` の保存経路と、settings.json の手編集を読む経路の両方で通す。
fn normalize_glossary(pairs: Vec<GlossaryPair>) -> Vec<GlossaryPair> {
    pairs
        .into_iter()
        .filter_map(|pair| {
            let source = pair.source.trim();
            let target = pair.target.trim();
            if source.is_empty() || target.is_empty() {
                return None;
            }
            Some(GlossaryPair {
                source: source.to_string(),
                target: target.to_string(),
            })
        })
        .take(MAX_GLOSSARY_PAIRS)
        .collect()
}

fn validate(settings: &Settings) -> Result<(), String> {
    if !(MIN_CONTEXT_TOKENS..=MAX_CONTEXT_TOKENS).contains(&settings.context_tokens) {
        return Err(format!(
            "context_tokens は {MIN_CONTEXT_TOKENS}〜{MAX_CONTEXT_TOKENS} の範囲で指定してください。"
        ));
    }
    if settings.gpu_layers > MAX_GPU_LAYERS {
        return Err(format!(
            "gpu_layers は 0〜{MAX_GPU_LAYERS} の範囲で指定してください。"
        ));
    }
    if !is_supported_ui_language(&settings.ui_language) {
        return Err("ui_language は ja / en / zh のいずれかで指定してください。".to_string());
    }
    Ok(())
}

/// 読んだ値が壊れていても起動できるよう、範囲外は丸める。
fn sanitize(settings: &mut Settings) {
    settings.context_tokens = settings
        .context_tokens
        .clamp(MIN_CONTEXT_TOKENS, MAX_CONTEXT_TOKENS);
    settings.gpu_layers = settings.gpu_layers.min(MAX_GPU_LAYERS);
    if !is_supported_ui_language(&settings.ui_language) {
        settings.ui_language = UI_LANGUAGE_DEFAULT.to_string();
    }
    settings.model_path = settings.model_path.take().and_then(normalize_optional);
    settings.server_path = settings.server_path.take().and_then(normalize_optional);
    settings.models_dir = settings.models_dir.take().and_then(normalize_optional);
    settings.active_model = settings.active_model.take().and_then(normalize_optional);
    settings.floating_window.model = settings
        .floating_window
        .model
        .take()
        .and_then(normalize_optional);
    settings.installed_runtime = settings.installed_runtime.take().and_then(|mut runtime| {
        runtime.server_path = normalize_optional(runtime.server_path)?;
        Some(runtime)
    });
    // 手編集で壊れたホットキーは既定へ戻す。空文字も既定にする。
    settings.floating_window.hotkey =
        crate::floating::normalize_hotkey(&settings.floating_window.hotkey)
            .unwrap_or_else(|| FloatingWindowSettings::default().hotkey);
    settings
        .installed_models
        .retain(|model| !model.file.trim().is_empty() && !model.path.trim().is_empty());
    for model in &mut settings.installed_models {
        model.file = model.file.trim().to_string();
        model.path = model.path.trim().to_string();
    }
    // 用語集も手編集で壊れうる。保存経路と同じ整列を通してから書き戻す
    // (load_or_default が差分を見て settings.json を整えた形へ更新する)。
    settings.glossary = normalize_glossary(std::mem::take(&mut settings.glossary));
}

fn load_or_default(path: &Path) -> Settings {
    let (mut settings, should_write) = match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Settings>(&text) {
            Ok(settings) => (settings, false),
            Err(error) => {
                eprintln!(
                    "設定ファイルを読めないので既定値で作り直します ({}): {error}",
                    path.display()
                );
                (Settings::default(), true)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Settings::default(), true),
        Err(error) => {
            eprintln!("設定ファイルを読めません ({}): {error}", path.display());
            (Settings::default(), false)
        }
    };
    let before = settings.clone();
    sanitize(&mut settings);
    if should_write || settings != before {
        if let Err(error) = save(path, &settings) {
            eprintln!("設定を保存できません ({}): {error}", path.display());
        }
    }
    settings
}

fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "設定ディレクトリを作れません ({}): {error}",
                parent.display()
            )
        })?;
    }
    let mut json = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("設定を JSON にできません: {error}"))?;
    json.push('\n');

    // 一時ファイルは固定名にしない。連続 set_settings や別プロセスと衝突しないよう、
    // pid と連番を付けた一意な名前にして、書き込み後に rename で atomic に差し替える。
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(
        ".{file_name}.{}.{sequence}.tmp",
        std::process::id()
    ));

    if let Err(error) = fs::write(&temporary, json) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "設定を書けません ({}): {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "設定を置き換えられません ({}): {error}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("plamo3-settings-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("一時ディレクトリを作れる");
        directory
    }

    #[test]
    fn defaults_match_the_contract() {
        let settings = Settings::default();
        assert!(settings.model_path.is_none());
        assert!(settings.server_path.is_none());
        assert_eq!(settings.gpu_layers, 99);
        assert_eq!(settings.context_tokens, 8192);
        assert_eq!(settings.theme, Theme::Light);
        assert!(settings.auto_translate);
        assert!(settings.auto_update_check);
        assert_eq!(settings.ui_language, UI_LANGUAGE_DEFAULT);
        assert!(settings.models_dir.is_none());
        assert!(settings.installed_runtime.is_none());
        assert!(settings.installed_models.is_empty());
        assert!(settings.active_model.is_none());
        assert!(settings.glossary.is_empty());
    }

    #[test]
    fn install_state_round_trips() {
        let directory = temp_dir("install-state");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let runtime = InstalledRuntime {
            server_path: "C:\\nuka\\runtime\\llama\\llama-server.exe".to_string(),
            version: "b12136".to_string(),
            installed_at: 42,
        };
        let models = vec![InstalledModel {
            file: "plamo-3-translate-0.8b-Q2_K.gguf".to_string(),
            repo: "pfnet/plamo-3-translate-0.8b-GGUF".to_string(),
            bytes: 410135168,
            path: "C:\\nuka\\models\\plamo-3-translate-0.8b-Q2_K.gguf".to_string(),
            installed_at: 43,
        }];
        store
            .update(|settings| {
                settings.installed_runtime = Some(runtime.clone());
                settings.installed_models = models.clone();
                settings.active_model = Some("plamo-3-translate-0.8b-Q2_K.gguf".to_string());
                settings.models_dir = Some("D:\\models".to_string());
                Ok(())
            })
            .expect("インストール状態を保存できる");

        let reloaded = SettingsStore::new(path).get();
        assert_eq!(reloaded.installed_runtime, Some(runtime));
        assert_eq!(reloaded.installed_models, models);
        assert_eq!(
            reloaded.active_model.as_deref(),
            Some("plamo-3-translate-0.8b-Q2_K.gguf")
        );
        assert_eq!(reloaded.models_dir.as_deref(), Some("D:\\models"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn blank_install_entries_are_dropped_on_load() {
        let directory = temp_dir("install-sanitize");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{
                "active_model": "   ",
                "installed_runtime": {"server_path": "  ", "version": "b12136", "installed_at": 1},
                "installed_models": [
                    {"file": " ", "repo": "pfnet/x", "bytes": 1, "path": "  ", "installed_at": 1},
                    {"file": "a.gguf", "repo": "pfnet/x", "bytes": 1, "path": " C:\\m\\a.gguf ", "installed_at": 1}
                ]
            }"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert!(settings.active_model.is_none());
        assert!(settings.installed_runtime.is_none());
        assert_eq!(settings.installed_models.len(), 1);
        assert_eq!(settings.installed_models[0].file, "a.gguf");
        assert_eq!(settings.installed_models[0].path, "C:\\m\\a.gguf");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn theme_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Theme::System).unwrap(), "\"system\"");
        assert!(serde_json::to_string(&Settings::default())
            .unwrap()
            .contains("\"theme\":\"light\""));
    }

    #[test]
    fn missing_file_is_created_with_defaults() {
        let directory = temp_dir("missing");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        assert_eq!(store.get(), Settings::default());
        assert!(path.is_file());
        let written: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written, Settings::default());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn partial_file_is_merged_with_defaults() {
        let directory = temp_dir("partial");
        let path = directory.join("settings.json");
        fs::write(&path, r#"{"theme":"dark","gpu_layers":12}"#).unwrap();
        let store = SettingsStore::new(path);
        let settings = store.get();
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.gpu_layers, 12);
        assert_eq!(settings.context_tokens, 8192);
        assert!(settings.auto_translate);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn corrupt_file_is_regenerated_with_defaults() {
        let directory = temp_dir("corrupt");
        let path = directory.join("settings.json");
        fs::write(&path, "{ これは JSON ではない ").unwrap();
        let store = SettingsStore::new(path.clone());
        assert_eq!(store.get(), Settings::default());
        let rewritten: Settings =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).expect("作り直されている");
        assert_eq!(rewritten, Settings::default());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let directory = temp_dir("clamp");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"context_tokens":10,"gpu_layers":99999,"model_path":"  "}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert_eq!(settings.context_tokens, MIN_CONTEXT_TOKENS);
        assert_eq!(settings.gpu_layers, MAX_GPU_LAYERS);
        assert!(settings.model_path.is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn manual_path_edits_are_normalized_on_load() {
        // 実行パスは IPC では変えられないが、settings.json の手編集は受け付ける。
        let directory = temp_dir("paths");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"model_path":"  C:\\models\\plamo.gguf  ","server_path":"   "}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert_eq!(
            settings.model_path.as_deref(),
            Some("C:\\models\\plamo.gguf")
        );
        assert!(settings.server_path.is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn apply_persists_and_reloads() {
        let directory = temp_dir("apply");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let updated = store
            .apply(SettingsPatch {
                gpu_layers: Some(20),
                context_tokens: Some(4096),
                theme: Some(Theme::Dark),
                auto_translate: Some(false),
                auto_update_check: Some(false),
                ui_language: Some("zh".to_string()),
                ..Default::default()
            })
            .expect("設定を保存できる");
        assert_eq!(updated.gpu_layers, 20);
        assert_eq!(updated.context_tokens, 4096);
        assert_eq!(updated.theme, Theme::Dark);
        assert!(!updated.auto_translate);
        assert!(!updated.auto_update_check);
        assert_eq!(updated.ui_language, "zh");

        let reloaded = SettingsStore::new(path).get();
        assert_eq!(reloaded, updated);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn apply_rejects_out_of_range_values() {
        let directory = temp_dir("reject");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path);
        let error = store
            .apply(SettingsPatch {
                context_tokens: Some(1),
                ..Default::default()
            })
            .expect_err("範囲外は拒む");
        assert!(error.contains("context_tokens"));
        assert_eq!(store.get().context_tokens, 8192);

        let error = store
            .apply(SettingsPatch {
                ui_language: Some("fr".to_string()),
                ..Default::default()
            })
            .expect_err("未知の言語コードは拒む");
        assert!(error.contains("ui_language"));
        assert_eq!(store.get().ui_language, UI_LANGUAGE_DEFAULT);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn ui_language_round_trips_and_unknown_values_fall_back() {
        let directory = temp_dir("ui-language");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        for code in ["en", "zh", "ja"] {
            store
                .apply(SettingsPatch {
                    ui_language: Some(code.to_string()),
                    ..Default::default()
                })
                .expect("保存できる");
            assert_eq!(SettingsStore::new(path.clone()).get().ui_language, code);
        }

        // 手編集で壊れた値は既定 (ja) に戻す
        fs::write(
            &path,
            r#"{"ui_language":"xx","gpu_layers":99,"context_tokens":8192}"#,
        )
        .unwrap();
        assert_eq!(SettingsStore::new(path).get().ui_language, "ja");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn in_memory_store_works_without_a_file() {
        let store = SettingsStore::in_memory();
        assert!(store.path().is_none());
        let updated = store
            .apply(SettingsPatch {
                theme: Some(Theme::System),
                ..Default::default()
            })
            .expect("メモリ内なら保存しない");
        assert_eq!(updated.theme, Theme::System);
        assert_eq!(store.get(), updated);
    }

    #[test]
    fn concurrent_apply_keeps_memory_and_file_in_sync() {
        let directory = temp_dir("concurrent");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());

        let mut handles = Vec::new();
        for index in 0..16u32 {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                store
                    .apply(SettingsPatch {
                        gpu_layers: Some(index),
                        ..Default::default()
                    })
                    .expect("並行 apply でも保存できる");
            }));
        }
        for handle in handles {
            handle.join().expect("スレッドが完了する");
        }

        // 保存とメモリ反映が同じ区間で行われるので、最後はどちらも同じ値になる。
        let memory = store.get();
        let disk = SettingsStore::new(path).get();
        assert_eq!(memory, disk);
        assert!(memory.gpu_layers < 16);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn floating_window_defaults_match_the_contract() {
        let settings = Settings::default();
        assert!(settings.floating_window.enabled);
        assert_eq!(
            settings.floating_window.hotkey,
            crate::floating::DEFAULT_HOTKEY
        );
        // 専用モデルの既定は None = アクティブモデルと同じ (従来動作)。
        assert!(settings.floating_window.model.is_none());
    }

    #[test]
    fn floating_window_round_trips() {
        let directory = temp_dir("floating-roundtrip");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let updated = store
            .update(|settings| {
                settings.floating_window.enabled = false;
                settings.floating_window.hotkey = "Ctrl+Alt+K".to_string();
                settings.floating_window.model =
                    Some("plamo-3-translate-0.8b-Q2_K.gguf".to_string());
                Ok(())
            })
            .expect("フローティング設定を保存できる");
        assert!(!updated.floating_window.enabled);
        assert_eq!(updated.floating_window.hotkey, "Ctrl+Alt+K");
        assert_eq!(
            updated.floating_window.model.as_deref(),
            Some("plamo-3-translate-0.8b-Q2_K.gguf")
        );
        assert_eq!(SettingsStore::new(path).get(), updated);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn floating_model_can_be_cleared_and_blank_values_are_dropped_on_load() {
        let directory = temp_dir("floating-model-clear");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        store
            .update(|settings| {
                settings.floating_window.model =
                    Some("plamo-3-translate-2b-Q4_K_M.gguf".to_string());
                Ok(())
            })
            .unwrap();
        assert!(SettingsStore::new(path.clone())
            .get()
            .floating_window
            .model
            .is_some());

        // 空文字を保存すると「アクティブモデルと同じ」(None) に戻る。
        store
            .update(|settings| {
                settings.floating_window.model = None;
                Ok(())
            })
            .unwrap();
        assert!(SettingsStore::new(path.clone())
            .get()
            .floating_window
            .model
            .is_none());

        // 手編集で空白だけになっていたら None に正規化する。
        fs::write(
            &path,
            r#"{"floating_window":{"enabled":true,"hotkey":"Ctrl+Shift+T","model":"   "}}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert!(settings.floating_window.model.is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn settings_without_a_floating_model_stay_compatible() {
        // 1.3.2 以前の settings.json (floating_window.model が無い) を読む想定。
        let directory = temp_dir("floating-model-missing");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"floating_window":{"enabled":false,"hotkey":"Ctrl+Alt+C"}}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert!(!settings.floating_window.enabled);
        assert!(settings.floating_window.model.is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn invalid_floating_hotkey_falls_back_to_the_default_on_load() {
        let directory = temp_dir("floating-invalid");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"floating_window":{"enabled":false,"hotkey":"Ctrl+Shift+F13"}}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert!(!settings.floating_window.enabled);
        assert_eq!(
            settings.floating_window.hotkey,
            crate::floating::DEFAULT_HOTKEY
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn settings_without_floating_window_stay_compatible() {
        // 1.2.0 以前の settings.json (項目そのものが無い) を読む想定。
        let directory = temp_dir("floating-missing");
        let path = directory.join("settings.json");
        fs::write(&path, r#"{"theme":"dark","gpu_layers":12}"#).unwrap();
        let settings = SettingsStore::new(path).get();
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.floating_window, FloatingWindowSettings::default());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn background_defaults_match_the_contract() {
        let settings = Settings::default();
        // 既定は「トレイに常駐する」「フローティング用エンジンは常時起動」。
        assert!(settings.background.enabled);
        assert!(settings.background.resident_engine);
    }

    #[test]
    fn resident_engine_requires_a_dedicated_floating_model() {
        // CR4-1: 専用モデルが無いのに常時起動すると、アクティブモデルを
        // メイン用と二重に読み込む。既定 (model=None) では常駐しない。
        let mut settings = Settings::default();
        assert!(!settings.floating_engine_resident());

        settings.floating_window.model = Some("plamo-3-translate-0.8b-Q2_K.gguf".to_string());
        assert!(settings.floating_engine_resident());

        // どれか 1 つでも欠ければ常駐しない (単一 predicate)。
        settings.floating_window.enabled = false;
        assert!(!settings.floating_engine_resident());
        settings.floating_window.enabled = true;

        settings.background.resident_engine = false;
        assert!(!settings.floating_engine_resident());
        settings.background.resident_engine = true;

        settings.background.enabled = false;
        assert!(!settings.floating_engine_resident());
        settings.background.enabled = true;

        assert!(settings.floating_engine_resident());
    }

    #[test]
    fn background_patch_round_trips_and_touches_only_given_fields() {
        let directory = temp_dir("background-roundtrip");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());

        let updated = store
            .apply(SettingsPatch {
                background: Some(BackgroundSettingsPatch {
                    enabled: Some(false),
                    resident_engine: Some(false),
                }),
                ..Default::default()
            })
            .expect("バックグラウンド設定を保存できる");
        assert!(!updated.background.enabled);
        assert!(!updated.background.resident_engine);
        assert_eq!(SettingsStore::new(path.clone()).get(), updated);

        // 片方だけの差分は、もう片方の値を保つ。
        let updated = store
            .apply(SettingsPatch {
                background: Some(BackgroundSettingsPatch {
                    enabled: Some(true),
                    resident_engine: None,
                }),
                ..Default::default()
            })
            .expect("片方だけでも保存できる");
        assert!(updated.background.enabled);
        assert!(!updated.background.resident_engine);
        assert_eq!(SettingsStore::new(path).get(), updated);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn empty_background_patch_leaves_settings_untouched() {
        let directory = temp_dir("background-empty");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        store
            .apply(SettingsPatch {
                background: Some(BackgroundSettingsPatch {
                    enabled: Some(false),
                    resident_engine: Some(false),
                }),
                ..Default::default()
            })
            .unwrap();
        let updated = store
            .apply(SettingsPatch {
                background: Some(BackgroundSettingsPatch::default()),
                ..Default::default()
            })
            .expect("空の差分でも保存できる");
        assert!(!updated.background.enabled);
        assert!(!updated.background.resident_engine);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn settings_without_background_stay_compatible() {
        // 1.3.1 以前の settings.json (background そのものが無い) を読む想定。
        let directory = temp_dir("background-missing");
        let path = directory.join("settings.json");
        fs::write(&path, r#"{"theme":"dark","gpu_layers":12}"#).unwrap();
        let settings = SettingsStore::new(path).get();
        assert_eq!(settings.background, BackgroundSettings::default());
        assert!(settings.background.resident_engine);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn legacy_keep_engine_is_ignored_and_resident_engine_defaults_on() {
        // 1.3.2 の settings.json は `background.keep_engine` を持つ。フィールド名を
        // `resident_engine` に変えたので old キーは未知フィールドとして無視され、
        // 新しい既定 (常時起動 ON) で読めること。
        let directory = temp_dir("background-legacy");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"background":{"enabled":true,"keep_engine":true}}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert!(settings.background.enabled);
        assert!(settings.background.resident_engine);

        // keep_engine = false だった旧設定でも、新既定 (ON) で読み込む。
        let path = directory.join("settings-off.json");
        fs::write(
            &path,
            r#"{"background":{"enabled":false,"keep_engine":false}}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path).get();
        assert!(!settings.background.enabled);
        assert!(settings.background.resident_engine);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn settings_without_glossary_stay_compatible() {
        // 1.3.5 以前の settings.json (glossary そのものが無い) を読む想定。
        // 旧 JSON に glossary が無くても Default (空) で読め、書き直しでも壊れない。
        let directory = temp_dir("glossary-missing");
        let path = directory.join("settings.json");
        fs::write(&path, r#"{"theme":"dark","gpu_layers":12}"#).unwrap();
        let settings = SettingsStore::new(path.clone()).get();
        assert_eq!(settings.theme, Theme::Dark);
        assert!(settings.glossary.is_empty());
        // 読み直しても同じ (missing の既定が空 vec であること)。
        assert!(SettingsStore::new(path).get().glossary.is_empty());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn glossary_round_trips_through_the_patch() {
        let directory = temp_dir("glossary-roundtrip");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let pairs = vec![
            GlossaryPair {
                source: "オーナー".to_string(),
                target: "assignee".to_string(),
            },
            GlossaryPair {
                source: "案件".to_string(),
                target: "project".to_string(),
            },
        ];

        let updated = store
            .apply(SettingsPatch {
                glossary: Some(pairs.clone()),
                ..Default::default()
            })
            .expect("用語集を保存できる");
        assert_eq!(updated.glossary, pairs);
        // 再起動しても残る (settings.json から読み直す)。
        assert_eq!(SettingsStore::new(path.clone()).get().glossary, pairs);
        // ほかの設定項目は触らない。
        assert_eq!(updated.theme, Theme::Light);
        assert_eq!(updated.context_tokens, 8192);
        assert_eq!(updated.background, BackgroundSettings::default());

        // 空配列は「全部消す」。
        let cleared = store
            .apply(SettingsPatch {
                glossary: Some(Vec::new()),
                ..Default::default()
            })
            .expect("空の用語集も保存できる");
        assert!(cleared.glossary.is_empty());
        assert!(SettingsStore::new(path.clone()).get().glossary.is_empty());

        // 省略したときは今の値を保つ (無関係な設定保存で用語集を消さない)。
        store
            .apply(SettingsPatch {
                glossary: Some(pairs.clone()),
                ..Default::default()
            })
            .unwrap();
        let kept = store
            .apply(SettingsPatch {
                theme: Some(Theme::Dark),
                ..Default::default()
            })
            .expect("用語集を指定しない保存でも壊れない");
        assert_eq!(kept.glossary, pairs);
        assert_eq!(SettingsStore::new(path).get().glossary, pairs);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn blank_glossary_rows_are_dropped_and_values_trimmed_on_save() {
        let directory = temp_dir("glossary-normalize");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let updated = store
            .apply(SettingsPatch {
                glossary: Some(vec![
                    // 空行 (UI の未入力行)
                    GlossaryPair::default(),
                    // 原文だけ
                    GlossaryPair {
                        source: "   ".to_string(),
                        target: "x".to_string(),
                    },
                    // 訳語だけ
                    GlossaryPair {
                        source: "y".to_string(),
                        target: "  ".to_string(),
                    },
                    // 前後の空白は落として保存する
                    GlossaryPair {
                        source: "  案件 ".to_string(),
                        target: " project  ".to_string(),
                    },
                ]),
                ..Default::default()
            })
            .expect("整列して保存できる");
        assert_eq!(
            updated.glossary,
            vec![GlossaryPair {
                source: "案件".to_string(),
                target: "project".to_string(),
            }]
        );
        // メモリとファイルが同じ整列結果になる。
        assert_eq!(
            SettingsStore::new(path.clone()).get().glossary,
            updated.glossary
        );
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("\"source\": \"\""));
        assert!(!raw.contains("\"target\": \"\""));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn blank_glossary_rows_are_dropped_and_values_trimmed_on_load() {
        // settings.json の手編集も保存と同じ整列を通す。
        let directory = temp_dir("glossary-load");
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"glossary":[
                {"source":"  ","target":"x"},
                {"source":"y","target":""},
                {"source":" 案件 ","target":" project "}
            ]}"#,
        )
        .unwrap();
        let settings = SettingsStore::new(path.clone()).get();
        assert_eq!(
            settings.glossary,
            vec![GlossaryPair {
                source: "案件".to_string(),
                target: "project".to_string(),
            }]
        );
        // 整列した形で書き戻される (次回起動では空行を読まない)。
        assert_eq!(SettingsStore::new(path).get().glossary, settings.glossary);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn glossary_is_capped_at_the_documented_limit() {
        // 境界: 200 対はそのまま、201 対は 200 対で止まる。
        // (UI 側も `glossary.ts` の MAX_GLOSSARY_PAIRS で 201 行目を追加させない)
        let pairs = |count: usize| -> Vec<GlossaryPair> {
            (0..count)
                .map(|index| GlossaryPair {
                    source: format!("source-{index}"),
                    target: format!("target-{index}"),
                })
                .collect()
        };
        let directory = temp_dir("glossary-cap");
        let path = directory.join("settings.json");
        let store = SettingsStore::new(path.clone());

        // ちょうど上限: 1 対も落とさない。
        let exact = store
            .apply(SettingsPatch {
                glossary: Some(pairs(MAX_GLOSSARY_PAIRS)),
                ..Default::default()
            })
            .expect("上限ちょうどは保存できる");
        assert_eq!(exact.glossary.len(), MAX_GLOSSARY_PAIRS);
        assert_eq!(
            SettingsStore::new(path.clone()).get().glossary,
            exact.glossary
        );

        // 上限 +1: 最後の 1 対だけを落とす (先頭から 200 対を残す)。
        let over = store
            .apply(SettingsPatch {
                glossary: Some(pairs(MAX_GLOSSARY_PAIRS + 1)),
                ..Default::default()
            })
            .expect("上限を超えても保存できる");
        assert_eq!(over.glossary.len(), MAX_GLOSSARY_PAIRS);
        assert_eq!(over.glossary[0].source, "source-0");
        assert_eq!(
            over.glossary[MAX_GLOSSARY_PAIRS - 1].source,
            format!("source-{}", MAX_GLOSSARY_PAIRS - 1)
        );
        // 切り捨て後の内容がそのまま永続化される (再起動で silently 消えない)。
        assert_eq!(SettingsStore::new(path).get().glossary, over.glossary);
        let _ = fs::remove_dir_all(&directory);
    }
}
