//! ランタイム (llama.cpp) とモデル (GGUF) のダウンロードと配置。
//!
//! - 同時に走るダウンロードは 1 本だけ (DownloadManager)。
//! - 途中ファイルは `.part`。サイズを検証してから rename する。
//! - キャンセル時は `.part` を消す。
//! - 進行状況は `download:progress` / `download:done` / `download:error` で流す。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::catalog::{self, CatalogEntry};
use crate::engine;
use crate::settings::{InstalledModel, InstalledRuntime, Settings, SettingsStore};

pub const RUNTIME_VERSION: &str = "b12136";
pub const RUNTIME_LABEL: &str = "b12136 (CUDA 12.4)";
const LLAMA_ZIP_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b12136/llama-b12136-bin-win-cuda-12.4-x64.zip";
const CUDART_ZIP_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b12136/cudart-llama-bin-win-cuda-12.4-x64.zip";

/// 展開後にそろっているべきファイル (fetch-runtime.ps1 と同じ)。
pub const REQUIRED_RUNTIME_FILES: [&str; 4] = [
    "llama-server.exe",
    "ggml-cuda.dll",
    "cudart64_12.dll",
    "cublas64_12.dll",
];

const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
const READ_TIMEOUT: Duration = Duration::from_secs(90);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Serialize)]
struct ProgressEvent {
    kind: &'static str,
    id: String,
    downloaded: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct DoneEvent {
    kind: &'static str,
    id: String,
}

#[derive(Clone, Serialize)]
struct ErrorEvent {
    kind: &'static str,
    id: String,
    message: String,
}

fn emit_progress(app: &AppHandle, kind: &'static str, id: &str, downloaded: u64, total: u64) {
    let payload = ProgressEvent {
        kind,
        id: id.to_string(),
        downloaded,
        total,
    };
    if let Err(error) = app.emit("download:progress", payload) {
        eprintln!("download:progress を送れません: {error}");
    }
}

fn emit_done(app: &AppHandle, kind: &'static str, id: &str) {
    let payload = DoneEvent {
        kind,
        id: id.to_string(),
    };
    if let Err(error) = app.emit("download:done", payload) {
        eprintln!("download:done を送れません: {error}");
    }
}

fn emit_error(app: &AppHandle, kind: &'static str, id: &str, message: &str) {
    let payload = ErrorEvent {
        kind,
        id: id.to_string(),
        message: message.to_string(),
    };
    if let Err(error) = app.emit("download:error", payload) {
        eprintln!("download:error を送れません: {error}");
    }
}

// ---------------------------------------------------------------------------
// 同時 1 本のジョブ管理
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct DownloadManager {
    inner: Arc<Mutex<Option<ActiveDownload>>>,
}

struct ActiveDownload {
    id: String,
    cancel: Arc<AtomicBool>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 新しいダウンロードを始める。既に走っていればエラー。
    fn begin(&self, id: &str) -> Result<Arc<AtomicBool>, String> {
        let mut slot = self.lock();
        if let Some(active) = slot.as_ref() {
            return Err(format!(
                "別のダウンロードが進行中です ({}). 完了するか取消してから再試行してください。",
                active.id
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(ActiveDownload {
            id: id.to_string(),
            cancel: cancel.clone(),
        });
        Ok(cancel)
    }

    fn finish(&self, id: &str) {
        let mut slot = self.lock();
        if slot.as_ref().is_some_and(|active| active.id == id) {
            *slot = None;
        }
    }

    /// 進行中のダウンロードに取消を要求する。対象が違えば何もしない。
    pub fn cancel(&self, id: &str) -> bool {
        match self.lock().as_ref() {
            Some(active) if active.id == id => {
                active.cancel.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    pub fn active_id(&self) -> Option<String> {
        self.lock().as_ref().map(|active| active.id.clone())
    }

    fn lock(&self) -> MutexGuard<'_, Option<ActiveDownload>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// ダウンロード本体
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum DownloadError {
    Cancelled,
    Failed(String),
}

impl DownloadError {
    fn message(&self) -> String {
        match self {
            Self::Cancelled => "ダウンロードを中止しました。".to_string(),
            Self::Failed(message) => message.clone(),
        }
    }
}

fn failed(message: impl Into<String>) -> DownloadError {
    DownloadError::Failed(message.into())
}

fn download_client() -> Result<reqwest::Client, DownloadError> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| failed(format!("ダウンロード用クライアントを作れません: {error}")))
}

/// 配布元の Content-Length を読む。
///
/// GitHub / Hugging Face の配布元は HEAD に対して `Content-Length: 0` を返すことがある。
/// そのため GET を開始してヘッダだけ読み、本体は読まずに捨てる。
async fn remote_size(client: &reqwest::Client, url: &str) -> Result<u64, DownloadError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| failed(format!("配布元に接続できません ({url}): {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(failed(format!("配布元が {status} を返しました ({url})")));
    }
    match response.content_length() {
        Some(size) if size > 0 => Ok(size),
        _ => Err(failed(format!(
            "配布元がサイズを返しません ({url})。ネットワークを確認してください。"
        ))),
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    dest.with_file_name(format!("{name}.part"))
}

fn remove_if_exists(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

/// `dest` へダウンロードする。`.part` に書いてサイズ検証してから rename する。
///
/// `on_progress` には (このファイルで書き込んだバイト数, 全体の合計バイト数) を渡す。
async fn download_file<F>(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: Option<u64>,
    total: u64,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<u64, DownloadError>
where
    F: FnMut(u64, u64),
{
    let part = part_path(dest);
    remove_if_exists(&part);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            failed(format!(
                "保存先を作れません ({}): {error}",
                parent.display()
            ))
        })?;
    }

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| failed(format!("ダウンロードを開始できません ({url}): {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(failed(format!("配布元が {status} を返しました ({url})")));
    }
    let content_length = response.content_length().filter(|length| *length > 0);
    if let (Some(expected), Some(actual)) = (expected.filter(|size| *size > 0), content_length) {
        if expected != actual {
            return Err(failed(format!(
                "配布元のサイズが想定と違います ({actual} / {expected} バイト) {url}"
            )));
        }
    }
    let expected = expected.filter(|size| *size > 0).or(content_length);

    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|error| failed(format!("保存先を作れません ({}): {error}", part.display())))?;
    let mut stream = response.bytes_stream();
    let mut written: u64 = 0;
    let mut last_emit = Instant::now();
    on_progress(0, total);

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = file.flush().await;
            drop(file);
            remove_if_exists(&part);
            return Err(DownloadError::Cancelled);
        }
        let item = tokio::time::timeout(READ_TIMEOUT, stream.next()).await;
        let bytes = match item {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(error))) => {
                drop(file);
                remove_if_exists(&part);
                return Err(failed(format!("ダウンロードを読めません: {error}")));
            }
            Ok(None) => break,
            Err(_) => {
                drop(file);
                remove_if_exists(&part);
                return Err(failed("ダウンロードがタイムアウトしました。"));
            }
        };
        file.write_all(&bytes)
            .await
            .map_err(|error| failed(format!("ダウンロードを書き込めません: {error}")))?;
        written += bytes.len() as u64;
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            on_progress(written, total);
            last_emit = Instant::now();
        }
    }
    file.flush()
        .await
        .map_err(|error| failed(format!("ダウンロードを書き込めません: {error}")))?;
    drop(file);

    // サイズ検証。合わなければ使わない。
    let actual = fs::metadata(&part)
        .map_err(|error| failed(format!("ダウンロードを確認できません: {error}")))?
        .len();
    if let Some(expected) = expected {
        if actual != expected {
            remove_if_exists(&part);
            return Err(failed(format!(
                "ダウンロードのサイズが一致しません ({actual} / {expected} バイト) {url}"
            )));
        }
    }
    if actual != written {
        remove_if_exists(&part);
        return Err(failed(format!(
            "ダウンロードの書き込み量が一致しません ({written} / {actual} バイト)"
        )));
    }

    if cancel.load(Ordering::SeqCst) {
        remove_if_exists(&part);
        return Err(DownloadError::Cancelled);
    }
    fs::rename(&part, dest).map_err(|error| {
        failed(format!(
            "ダウンロードを配置できません ({}): {error}",
            dest.display()
        ))
    })?;
    on_progress(written, total);
    Ok(written)
}

// ---------------------------------------------------------------------------
// ランタイム
// ---------------------------------------------------------------------------

/// llama.cpp の 2 つの zip をダウンロードして展開し、検証して記録する。
pub async fn install_runtime(
    app: &AppHandle,
    settings: &SettingsStore,
    downloads: &DownloadManager,
) -> Result<InstalledRuntime, String> {
    const KIND: &str = "runtime";
    const ID: &str = "runtime";
    let cancel = downloads.begin(ID)?;
    let result = install_runtime_inner(app, settings, cancel).await;
    downloads.finish(ID);
    match result {
        Ok(runtime) => {
            emit_done(app, KIND, ID);
            Ok(runtime)
        }
        Err(error) => {
            let message = error.message();
            eprintln!("ランタイムのインストールに失敗しました: {message}");
            emit_error(app, KIND, ID, &message);
            Err(message)
        }
    }
}

async fn install_runtime_inner(
    app: &AppHandle,
    settings: &SettingsStore,
    cancel: Arc<AtomicBool>,
) -> Result<InstalledRuntime, DownloadError> {
    const KIND: &str = "runtime";
    const ID: &str = "runtime";
    let client = download_client()?;
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|error| failed(format!("保存先を取得できません: {error}")))?;
    let runtime_root = local_data.join("runtime");
    let llama_dir = runtime_root.join("llama");
    let downloads_dir = runtime_root.join(".downloads");
    fs::create_dir_all(&llama_dir).map_err(|error| {
        failed(format!(
            "保存先を作れません ({}): {error}",
            llama_dir.display()
        ))
    })?;
    fs::create_dir_all(&downloads_dir).map_err(|error| {
        failed(format!(
            "保存先を作れません ({}): {error}",
            downloads_dir.display()
        ))
    })?;

    let llama_zip = downloads_dir.join("llama-b12136-bin-win-cuda-12.4-x64.zip");
    let cudart_zip = downloads_dir.join("cudart-llama-bin-win-cuda-12.4-x64.zip");

    let llama_size = remote_size(&client, LLAMA_ZIP_URL).await?;
    let cudart_size = remote_size(&client, CUDART_ZIP_URL).await?;
    let total = llama_size + cudart_size;

    let first_app = app.clone();
    download_file(
        &client,
        LLAMA_ZIP_URL,
        &llama_zip,
        Some(llama_size),
        total,
        &cancel,
        |written, total| emit_progress(&first_app, KIND, ID, written, total),
    )
    .await
    .inspect_err(|_| remove_if_exists(&llama_zip))?;

    if cancel.load(Ordering::SeqCst) {
        remove_if_exists(&llama_zip);
        return Err(DownloadError::Cancelled);
    }

    let second_app = app.clone();
    let base = llama_size;
    download_file(
        &client,
        CUDART_ZIP_URL,
        &cudart_zip,
        Some(cudart_size),
        total,
        &cancel,
        move |written, total| emit_progress(&second_app, KIND, ID, base + written, total),
    )
    .await
    .inspect_err(|_| {
        remove_if_exists(&llama_zip);
        remove_if_exists(&cudart_zip);
    })?;

    if cancel.load(Ordering::SeqCst) {
        remove_if_exists(&llama_zip);
        remove_if_exists(&cudart_zip);
        return Err(DownloadError::Cancelled);
    }

    // 展開はブロッキングなので別スレッドで。
    for zip in [&llama_zip, &cudart_zip] {
        let source = zip.clone();
        let destination = llama_dir.clone();
        tokio::task::spawn_blocking(move || extract_zip(&source, &destination))
            .await
            .map_err(|error| failed(format!("展開処理が落ちました: {error}")))??;
        if cancel.load(Ordering::SeqCst) {
            remove_if_exists(&llama_zip);
            remove_if_exists(&cudart_zip);
            return Err(DownloadError::Cancelled);
        }
    }
    verify_runtime_dir(&llama_dir).map_err(DownloadError::Failed)?;

    remove_if_exists(&llama_zip);
    remove_if_exists(&cudart_zip);
    let _ = fs::remove_dir(&downloads_dir);

    let server_path = llama_dir.join("llama-server.exe");
    let runtime = InstalledRuntime {
        server_path: server_path.to_string_lossy().into_owned(),
        version: RUNTIME_VERSION.to_string(),
        installed_at: unix_seconds(),
    };
    settings
        .update(|next| {
            next.installed_runtime = Some(runtime.clone());
            Ok(())
        })
        .map_err(failed)?;
    Ok(runtime)
}

/// 必要なファイルが展開先にそろっているか。
pub fn verify_runtime_dir(dir: &Path) -> Result<(), String> {
    let missing: Vec<&str> = REQUIRED_RUNTIME_FILES
        .iter()
        .copied()
        .filter(|name| !dir.join(name).is_file())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "展開後に必要なファイルがありません: {}",
            missing.join(", ")
        ))
    }
}

/// zip を `dest` 直下へ展開する。ディレクトリは無視し、パス脱出する項目は捨てる。
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), DownloadError> {
    let file = fs::File::open(zip_path).map_err(|error| {
        failed(format!(
            "zip を開けません ({}): {error}",
            zip_path.display()
        ))
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        failed(format!(
            "zip を読めません ({}): {error}",
            zip_path.display()
        ))
    })?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| failed(format!("zip の項目を読めません: {error}")))?;
        if entry.is_dir() {
            continue;
        }
        let Some(enclosed) = entry.enclosed_name() else {
            continue;
        };
        let Some(file_name) = enclosed.file_name() else {
            continue;
        };
        let target = dest.join(file_name);
        let mut output = fs::File::create(&target).map_err(|error| {
            failed(format!(
                "展開先に書けません ({}): {error}",
                target.display()
            ))
        })?;
        std::io::copy(&mut entry, &mut output).map_err(|error| {
            failed(format!(
                "展開に失敗しました ({}): {error}",
                target.display()
            ))
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// モデル
// ---------------------------------------------------------------------------

/// カタログの 1 件を `models_dir` へダウンロードして記録する。
pub async fn install_model(
    app: &AppHandle,
    settings: &SettingsStore,
    downloads: &DownloadManager,
    catalog_id: &str,
) -> Result<InstalledModel, String> {
    const KIND: &str = "model";
    let entry = catalog::find(catalog_id)
        .ok_or_else(|| format!("カタログにないモデルです: {catalog_id}"))?;
    let cancel = downloads.begin(catalog_id)?;
    let result = install_model_inner(app, settings, cancel, &entry).await;
    downloads.finish(catalog_id);
    match result {
        Ok(model) => {
            emit_done(app, KIND, catalog_id);
            Ok(model)
        }
        Err(error) => {
            let message = error.message();
            eprintln!("モデルのインストールに失敗しました: {message}");
            emit_error(app, KIND, catalog_id, &message);
            Err(message)
        }
    }
}

async fn install_model_inner(
    app: &AppHandle,
    settings: &SettingsStore,
    cancel: Arc<AtomicBool>,
    entry: &CatalogEntry,
) -> Result<InstalledModel, DownloadError> {
    const KIND: &str = "model";
    let current = settings.get();
    let models_dir = models_dir_for(app, &current).map_err(failed)?;
    let dest = models_dir.join(&entry.file);
    if dest.is_file() {
        return Err(failed(format!(
            "既にインストールされています: {}",
            dest.display()
        )));
    }
    fs::create_dir_all(&models_dir).map_err(|error| {
        failed(format!(
            "保存先を作れません ({}): {error}",
            models_dir.display()
        ))
    })?;

    let client = download_client()?;
    let progress_app = app.clone();
    let id = entry.id.clone();
    download_file(
        &client,
        &entry.url,
        &dest,
        Some(entry.bytes),
        entry.bytes,
        &cancel,
        move |written, total| emit_progress(&progress_app, KIND, &id, written, total),
    )
    .await?;

    if cancel.load(Ordering::SeqCst) {
        remove_if_exists(&dest);
        return Err(DownloadError::Cancelled);
    }

    let model = InstalledModel {
        file: entry.file.clone(),
        repo: entry.repo.clone(),
        bytes: entry.bytes,
        path: dest.to_string_lossy().into_owned(),
        installed_at: unix_seconds(),
    };
    if let Err(error) = settings.update(|next| {
        next.installed_models.retain(|item| item.file != entry.file);
        next.installed_models.push(model.clone());
        Ok(())
    }) {
        remove_if_exists(&dest);
        return Err(failed(error));
    }
    Ok(model)
}

/// インストール済みモデルを削除する。使用中のものは拒否する。
pub fn delete_model(settings: &SettingsStore, file: &str) -> Result<Settings, String> {
    let current = settings.get();
    if current.active_model.as_deref() == Some(file) {
        return Err(
            "使用中のモデルは削除できません。別のモデルを選んでから削除してください。".to_string(),
        );
    }
    let Some(model) = current
        .installed_models
        .iter()
        .find(|model| model.file == file)
        .cloned()
    else {
        return Err(format!("インストール済みのモデルにありません: {file}"));
    };
    let path = PathBuf::from(&model.path);
    if path.is_file() {
        fs::remove_file(&path)
            .map_err(|error| format!("モデルを削除できません ({}): {error}", path.display()))?;
    }
    settings.update(|next| {
        next.installed_models.retain(|item| item.file != file);
        Ok(())
    })
}

/// 使用するモデルを切り替える。エンジンの再起動は呼び出し側で行う。
pub fn set_active_model(settings: &SettingsStore, file: &str) -> Result<Settings, String> {
    let current = settings.get();
    let Some(model) = current
        .installed_models
        .iter()
        .find(|model| model.file == file)
    else {
        return Err(format!("インストール済みのモデルにありません: {file}"));
    };
    if !Path::new(&model.path).is_file() {
        return Err(format!(
            "モデルファイルが見つかりません。再インストールしてください: {}",
            model.path
        ));
    }
    settings.update(|next| {
        next.active_model = Some(file.to_string());
        Ok(())
    })
}

/// モデルの保存先を変更する。空文字なら既定の場所に戻す。
pub fn set_models_dir(settings: &SettingsStore, path: &str) -> Result<Settings, String> {
    let trimmed = path.trim();
    let value = if trimmed.is_empty() {
        None
    } else {
        let candidate = PathBuf::from(trimmed);
        if !candidate.is_absolute() {
            return Err("フォルダは絶対パスで指定してください。".to_string());
        }
        fs::create_dir_all(&candidate)
            .map_err(|error| format!("フォルダを作れません ({}): {error}", candidate.display()))?;
        Some(candidate.to_string_lossy().into_owned())
    };
    settings.update(|next| {
        next.models_dir = value.clone();
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// 状態の取得
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeState {
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// ランタイムのディスク使用量 (バイト)。未導入なら省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupState {
    pub runtime: RuntimeState,
    pub models: Vec<InstalledModel>,
    pub active_model: Option<String>,
    pub models_dir: String,
}

/// 設定画面が使う現在の導入状態。
pub fn setup_state(app: &AppHandle, settings: &SettingsStore) -> SetupState {
    let current = settings.get();
    let default_server = engine::default_runtime_server_path(app);
    let resolved = engine::resolve_runtime_path(
        &current,
        default_server.as_deref(),
        &engine::exe_directory(),
    )
    .ok();
    let version = current.installed_runtime.as_ref().and_then(|runtime| {
        let managed = PathBuf::from(&runtime.server_path);
        (resolved.as_deref() == Some(managed.as_path())).then(|| runtime.version.clone())
    });
    let bytes = resolved
        .as_deref()
        .and_then(Path::parent)
        .map(dir_size)
        .filter(|size| *size > 0);
    SetupState {
        runtime: RuntimeState {
            installed: resolved.is_some(),
            server_path: resolved.map(|path| path.to_string_lossy().into_owned()),
            version,
            bytes,
        },
        models: current.installed_models.clone(),
        active_model: current.active_model.clone(),
        models_dir: models_dir_for(app, &current)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// ディレクトリ内のファイルサイズ合計 (サブディレクトリも含む)。
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            total += dir_size(&entry.path());
        } else if metadata.is_file() {
            total += metadata.len();
        }
    }
    total
}

/// モデルの保存先。設定値が無ければ `{app_local_data_dir}/models`。
pub fn models_dir_for(app: &AppHandle, settings: &Settings) -> Result<PathBuf, String> {
    let local_data = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("保存先を取得できません: {error}"))?;
    Ok(models_dir_from(&local_data, settings))
}

fn models_dir_from(local_data: &Path, settings: &Settings) -> PathBuf {
    match settings.models_dir.as_deref().map(str::trim) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => local_data.join("models"),
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir(label: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("nuka-installer-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("一時ディレクトリを作れる");
        directory
    }

    #[test]
    fn download_manager_allows_one_job_at_a_time() {
        let manager = DownloadManager::new();
        let first = manager.begin("0.8b-Q4_K_M").expect("1 本目は始められる");
        assert!(manager.begin("2b-Q4_K_M").is_err());
        assert_eq!(manager.active_id().as_deref(), Some("0.8b-Q4_K_M"));
        manager.cancel("0.8b-Q4_K_M");
        assert!(first.load(Ordering::SeqCst));
        manager.finish("other");
        assert_eq!(manager.active_id().as_deref(), Some("0.8b-Q4_K_M"));
        manager.finish("0.8b-Q4_K_M");
        assert!(manager.active_id().is_none());
        assert!(manager.begin("2b-Q4_K_M").is_ok());
    }

    #[test]
    fn dir_size_sums_files_recursively() {
        let directory = temp_dir("dir-size");
        fs::write(directory.join("a.bin"), vec![0u8; 10]).unwrap();
        fs::create_dir_all(directory.join("sub")).unwrap();
        fs::write(directory.join("sub").join("b.bin"), vec![0u8; 20]).unwrap();
        assert_eq!(dir_size(&directory), 30);
        assert_eq!(dir_size(&directory.join("missing")), 0);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn runtime_verification_requires_all_files() {
        let directory = temp_dir("runtime-verify");
        for name in REQUIRED_RUNTIME_FILES {
            fs::write(directory.join(name), b"x").unwrap();
        }
        assert!(verify_runtime_dir(&directory).is_ok());
        fs::remove_file(directory.join("ggml-cuda.dll")).unwrap();
        let error = verify_runtime_dir(&directory).expect_err("欠けていれば失敗する");
        assert!(error.contains("ggml-cuda.dll"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn part_path_appends_suffix() {
        let path = PathBuf::from("C:\\models\\a.gguf");
        assert_eq!(part_path(&path), PathBuf::from("C:\\models\\a.gguf.part"));
    }

    #[test]
    fn models_dir_uses_override_or_default() {
        let local = PathBuf::from("C:\\local");
        let mut settings = Settings::default();
        assert_eq!(models_dir_from(&local, &settings), local.join("models"));
        settings.models_dir = Some("D:\\models".to_string());
        assert_eq!(
            models_dir_from(&local, &settings),
            PathBuf::from("D:\\models")
        );
        settings.models_dir = Some("   ".to_string());
        assert_eq!(models_dir_from(&local, &settings), local.join("models"));
    }

    /// 決められたステータスと本文を返す小さなサーバ。
    async fn spawn_stub(body: Vec<u8>, declared_length: Option<usize>) -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("stub を bind できる");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut head = [0u8; 4096];
            let _ = socket.read(&mut head).await;
            let length = declared_length.unwrap_or(body.len());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        });
        port
    }

    #[tokio::test]
    async fn download_writes_file_and_verifies_size() {
        let directory = temp_dir("download-ok");
        let dest = directory.join("model.gguf");
        let body = vec![7u8; 4096];
        let port = spawn_stub(body.clone(), None).await;
        let client = download_client().expect("client");
        let cancel = AtomicBool::new(false);
        let mut progress = Vec::new();
        let written = download_file(
            &client,
            &format!("http://127.0.0.1:{port}/model.gguf"),
            &dest,
            Some(body.len() as u64),
            body.len() as u64,
            &cancel,
            |written, _| progress.push(written),
        )
        .await
        .expect("ダウンロードできる");
        assert_eq!(written, body.len() as u64);
        assert_eq!(fs::read(&dest).expect("ファイルがある"), body);
        assert!(!part_path(&dest).exists());
        assert!(!progress.is_empty());
        let _ = fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn download_rejects_size_mismatch() {
        let directory = temp_dir("download-mismatch");
        let dest = directory.join("model.gguf");
        let body = vec![1u8; 1024];
        let port = spawn_stub(body.clone(), Some(4096)).await;
        let client = download_client().expect("client");
        let cancel = AtomicBool::new(false);
        let error = download_file(
            &client,
            &format!("http://127.0.0.1:{port}/model.gguf"),
            &dest,
            Some(2048),
            2048,
            &cancel,
            |_, _| {},
        )
        .await
        .expect_err("サイズ不一致は失敗する");
        assert!(error.message().contains("サイズ"));
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn cancel_removes_the_part_file() {
        let directory = temp_dir("download-cancel");
        let dest = directory.join("model.gguf");
        let body = vec![2u8; 4096];
        let port = spawn_stub(body, None).await;
        let client = download_client().expect("client");
        let cancel = AtomicBool::new(true);
        let error = download_file(
            &client,
            &format!("http://127.0.0.1:{port}/model.gguf"),
            &dest,
            Some(4096),
            4096,
            &cancel,
            |_, _| {},
        )
        .await
        .expect_err("キャンセルされる");
        assert_eq!(error.message(), "ダウンロードを中止しました。");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = fs::remove_dir_all(&directory);
    }
}
