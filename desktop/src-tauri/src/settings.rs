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

pub const MIN_CONTEXT_TOKENS: u32 = 512;
pub const MAX_CONTEXT_TOKENS: u32 = 32768;
pub const MAX_GPU_LAYERS: u32 = 1000;

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
        }
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
        let mut current = self.lock();
        let mut next = current.clone();
        if let Some(value) = patch.gpu_layers {
            next.gpu_layers = value;
        }
        if let Some(value) = patch.context_tokens {
            next.context_tokens = value;
        }
        if let Some(value) = patch.theme {
            next.theme = value;
        }
        if let Some(value) = patch.auto_translate {
            next.auto_translate = value;
        }
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
    Ok(())
}

/// 読んだ値が壊れていても起動できるよう、範囲外は丸める。
fn sanitize(settings: &mut Settings) {
    settings.context_tokens = settings
        .context_tokens
        .clamp(MIN_CONTEXT_TOKENS, MAX_CONTEXT_TOKENS);
    settings.gpu_layers = settings.gpu_layers.min(MAX_GPU_LAYERS);
    settings.model_path = settings.model_path.take().and_then(normalize_optional);
    settings.server_path = settings.server_path.take().and_then(normalize_optional);
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
            })
            .expect("設定を保存できる");
        assert_eq!(updated.gpu_layers, 20);
        assert_eq!(updated.context_tokens, 4096);
        assert_eq!(updated.theme, Theme::Dark);
        assert!(!updated.auto_translate);

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
}
