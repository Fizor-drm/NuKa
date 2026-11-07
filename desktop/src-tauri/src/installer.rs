//! ランタイム (llama.cpp) とモデル (GGUF) のダウンロードと配置。
//!
//! - 同時に走るダウンロードは 1 本だけ (DownloadManager)。
//! - 途中ファイルは `.part`。サイズを検証してから rename する。
//! - キャンセル時は `.part` を消す。
//! - 進行状況は `download:progress` / `download:done` / `download:error` で流す。

use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::catalog::{self, CatalogEntry};
use crate::engine;
use crate::settings::{InstalledModel, InstalledRuntime, Settings, SettingsStore};

pub const RUNTIME_VERSION: &str = "b12136";
pub const RUNTIME_LABEL: &str = "b12136 (CUDA 12.4)";

/// 配布 zip の固定情報。サイズと SHA-256 をコードに埋め込み、展開前に必ず照合する。
///
/// 公式リリースはチェックサムを公表していないため、公式 b12136 から取得した
/// 検証済み zip をもとに固定した (README に入手元と検証方式を記載)。
struct RuntimeArchive {
    url: &'static str,
    file_name: &'static str,
    bytes: u64,
    sha256: &'static str,
}

const LLAMA_ZIP: RuntimeArchive = RuntimeArchive {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12136/llama-b12136-bin-win-cuda-12.4-x64.zip",
    file_name: "llama-b12136-bin-win-cuda-12.4-x64.zip",
    bytes: 288_912_954,
    sha256: "A7F383874199086EB6AE88344A5CBFAD9204AE02D6A5FD6D1C0A84298BA94CC0",
};

const CUDART_ZIP: RuntimeArchive = RuntimeArchive {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12136/cudart-llama-bin-win-cuda-12.4-x64.zip",
    file_name: "cudart-llama-bin-win-cuda-12.4-x64.zip",
    bytes: 391_443_627,
    sha256: "8C79A9B226DE4B3CACFD1F83D24F962D0773BE79F1E7B75C6AF4DED7E32AE1D6",
};

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
    pub(crate) fn begin(&self, id: &str) -> Result<Arc<AtomicBool>, String> {
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

    pub(crate) fn finish(&self, id: &str) {
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
    let builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
    // テストではローカル HTTP のスタブに接続するため、テスト時だけ https 強制を外す。
    #[cfg(not(test))]
    let builder = builder.https_only(true);
    builder
        .build()
        .map_err(|error| failed(format!("ダウンロード用クライアントを作れません: {error}")))
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), DownloadError> {
    if cancel.load(Ordering::SeqCst) {
        Err(DownloadError::Cancelled)
    } else {
        Ok(())
    }
}

fn sha256_file(path: &Path) -> Result<String, DownloadError> {
    let mut file = fs::File::open(path).map_err(|error| {
        failed(format!(
            "SHA-256 の対象を開けません ({}): {error}",
            path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|error| {
        failed(format!(
            "SHA-256 を計算できません ({}): {error}",
            path.display()
        ))
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// 固定したサイズと SHA-256 に一致するか。一致しなければ展開しない。
fn verify_archive(path: &Path, archive: &RuntimeArchive) -> Result<(), DownloadError> {
    let size = fs::metadata(path)
        .map_err(|error| {
            failed(format!(
                "配布ファイルを確認できません ({}): {error}",
                path.display()
            ))
        })?
        .len();
    if size != archive.bytes {
        return Err(failed(format!(
            "配布ファイルのサイズが一致しません ({size} / {} バイト): {}",
            archive.bytes, archive.file_name
        )));
    }
    let actual = sha256_file(path)?;
    if !actual.eq_ignore_ascii_case(archive.sha256) {
        return Err(failed(format!(
            "配布ファイルの SHA-256 が一致しません: {} (取得値 {actual})",
            archive.file_name
        )));
    }
    Ok(())
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
        if let Err(error) = file.write_all(&bytes).await {
            drop(file);
            remove_if_exists(&part);
            return Err(failed(format!("ダウンロードを書き込めません: {error}")));
        }
        written += bytes.len() as u64;
        if let Some(expected) = expected {
            if written > expected {
                drop(file);
                remove_if_exists(&part);
                return Err(failed(format!(
                    "配布元のデータが想定より大きいため中止しました ({written} / {expected} バイト)"
                )));
            }
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            on_progress(written, total);
            last_emit = Instant::now();
        }
    }
    if let Err(error) = file.flush().await {
        drop(file);
        remove_if_exists(&part);
        return Err(failed(format!("ダウンロードを書き込めません: {error}")));
    }
    drop(file);

    // サイズ検証。合わなければ使わない。
    let actual = match fs::metadata(&part) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            remove_if_exists(&part);
            return Err(failed(format!("ダウンロードを確認できません: {error}")));
        }
    };
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
///
/// 実行中の `llama-server.exe` は Windows でロックされるため、先にエンジンを止める。
pub async fn install_runtime(
    app: &AppHandle,
    settings: &SettingsStore,
    downloads: &DownloadManager,
    engine: &engine::Engine,
) -> Result<InstalledRuntime, String> {
    const KIND: &str = "runtime";
    const ID: &str = "runtime";
    let cancel = downloads.begin(ID)?;
    engine.kill_now();
    let result = install_runtime_inner(app, settings, cancel).await;
    downloads.finish(ID);
    match result {
        Ok(runtime) => {
            emit_done(app, KIND, ID);
            // 新しいランタイム (またはまだ何も無い状態) で状態を作り直す。
            engine.start(&settings.get()).await;
            Ok(runtime)
        }
        Err(error) => {
            let message = error.message();
            eprintln!("ランタイムのインストールに失敗しました: {message}");
            emit_error(app, KIND, ID, &message);
            // 既存のランタイムが残っていればそれで戻す。無ければ未導入のエラー状態。
            engine.start(&settings.get()).await;
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
    let staging_dir = runtime_root.join("llama.staging");
    let downloads_dir = runtime_root.join(".downloads");
    fs::create_dir_all(&runtime_root).map_err(|error| {
        failed(format!(
            "保存先を作れません ({}): {error}",
            runtime_root.display()
        ))
    })?;
    // 前回の失敗で残った staging は捨てる。
    let _ = fs::remove_dir_all(&staging_dir);
    fs::create_dir_all(&staging_dir).map_err(|error| {
        failed(format!(
            "保存先を作れません ({}): {error}",
            staging_dir.display()
        ))
    })?;
    fs::create_dir_all(&downloads_dir).map_err(|error| {
        failed(format!(
            "保存先を作れません ({}): {error}",
            downloads_dir.display()
        ))
    })?;

    let llama_zip = downloads_dir.join(LLAMA_ZIP.file_name);
    let cudart_zip = downloads_dir.join(CUDART_ZIP.file_name);
    let total = LLAMA_ZIP.bytes + CUDART_ZIP.bytes;

    let prepared: Result<(), DownloadError> = async {
        let first_app = app.clone();
        download_file(
            &client,
            LLAMA_ZIP.url,
            &llama_zip,
            Some(LLAMA_ZIP.bytes),
            total,
            &cancel,
            |written, total| emit_progress(&first_app, KIND, ID, written, total),
        )
        .await?;
        check_cancelled(&cancel)?;
        verify_archive(&llama_zip, &LLAMA_ZIP)?;

        let second_app = app.clone();
        let base = LLAMA_ZIP.bytes;
        download_file(
            &client,
            CUDART_ZIP.url,
            &cudart_zip,
            Some(CUDART_ZIP.bytes),
            total,
            &cancel,
            move |written, total| emit_progress(&second_app, KIND, ID, base + written, total),
        )
        .await?;
        check_cancelled(&cancel)?;
        verify_archive(&cudart_zip, &CUDART_ZIP)?;

        // 展開は staging に対して行い、検証が通るまで既存の llama には触らない。
        for (zip, archive) in [(&llama_zip, &LLAMA_ZIP), (&cudart_zip, &CUDART_ZIP)] {
            if !zip.is_file() {
                return Err(failed(format!(
                    "配布ファイルが見つかりません: {}",
                    archive.file_name
                )));
            }
            let source = zip.clone();
            let destination = staging_dir.clone();
            tokio::task::spawn_blocking(move || extract_zip(&source, &destination))
                .await
                .map_err(|error| failed(format!("展開処理が落ちました: {error}")))??;
            check_cancelled(&cancel)?;
        }
        verify_runtime_dir(&staging_dir).map_err(DownloadError::Failed)?;
        check_cancelled(&cancel)?;
        swap_runtime_dir(&staging_dir, &llama_dir)?;
        Ok(())
    }
    .await;

    // 成功・失敗・キャンセルのいずれでも zip と staging を残さない。
    remove_if_exists(&llama_zip);
    remove_if_exists(&cudart_zip);
    let _ = fs::remove_dir_all(&staging_dir);
    let _ = fs::remove_dir(&downloads_dir);
    prepared?;

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

/// staging を本体へ入れ替える。既存の `llama` は消してから rename する。
fn swap_runtime_dir(staging: &Path, target: &Path) -> Result<(), DownloadError> {
    if target.exists() {
        fs::remove_dir_all(target).map_err(|error| {
            failed(format!(
                "既存のランタイムを置き換えられません ({}): {error}",
                target.display()
            ))
        })?;
    }
    fs::rename(staging, target).map_err(|error| {
        failed(format!(
            "ランタイムを配置できません ({}): {error}",
            target.display()
        ))
    })
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

/// 既存ファイルの状態。`Ok(true)` は同サイズのファイルが既にある (再 DL 不要)。
fn existing_model_file(dest: &Path, expected: u64) -> Result<bool, DownloadError> {
    match fs::symlink_metadata(dest) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(failed(format!(
                    "保存先にファイル以外のものがあります: {}",
                    dest.display()
                )));
            }
            if metadata.len() == expected {
                Ok(true)
            } else {
                Err(failed(format!(
                    "サイズの合わないファイルが既にあります: {} ({} / {} バイト)。削除してから再実行してください。",
                    dest.display(),
                    metadata.len(),
                    expected
                )))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(failed(format!(
            "既存ファイルを確認できません ({}): {error}",
            dest.display()
        ))),
    }
}

fn register_model(
    settings: &SettingsStore,
    entry: &CatalogEntry,
    dest: &Path,
) -> Result<InstalledModel, DownloadError> {
    let model = InstalledModel {
        file: entry.file.clone(),
        repo: entry.repo.clone(),
        bytes: entry.bytes,
        path: dest.to_string_lossy().into_owned(),
        installed_at: unix_seconds(),
    };
    settings
        .update(|next| {
            next.installed_models.retain(|item| item.file != entry.file);
            next.installed_models.push(model.clone());
            Ok(())
        })
        .map_err(failed)?;
    Ok(model)
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

    // settings を作り直した・保存先を変えた等で、同じファイルが既にある場合は
    // ダウンロードせず登録だけ行う。
    if existing_model_file(&dest, entry.bytes)? {
        return register_model(settings, entry, &dest);
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

    check_cancelled(&cancel)?;
    register_model(settings, entry, &dest)
}

/// インストール済みモデルを削除する。使用中のものは拒否する。
pub fn delete_model(settings: &SettingsStore, file: &str) -> Result<Settings, String> {
    let current = settings.get();
    if current.active_model.as_deref() == Some(file) {
        return Err(
            "使用中のモデルは削除できません。別のモデルを選んでから削除してください。".to_string(),
        );
    }
    let Some(entry) = catalog::find_by_file(file) else {
        return Err(format!("カタログにないモデルです: {file}"));
    };
    let Some(model) = current
        .installed_models
        .iter()
        .find(|model| model.file == file)
        .cloned()
    else {
        return Err(format!("インストール済みのモデルにありません: {file}"));
    };

    // 記録と実ファイルがずれていないか確かめてから消す。
    let path = PathBuf::from(&model.path);
    let name_matches = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == entry.file);
    if !name_matches {
        return Err(format!(
            "削除対象のファイル名が一致しません: {} (期待: {})",
            path.display(),
            entry.file
        ));
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("シンボリックリンクは削除できません。".to_string());
        }
        Ok(metadata) if metadata.is_file() => {
            fs::remove_file(&path)
                .map_err(|error| format!("モデルを削除できません ({}): {error}", path.display()))?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "モデルを確認できません ({}): {error}",
                path.display()
            ));
        }
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

/// モデルの保存先として許可できるパスか。
///
/// ローカルの絶対パス (ドライブ文字 + ルート) のみ許可し、UNC と `..` は拒否する。
fn validate_models_dir(path: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(path);
    if !candidate.is_absolute() {
        return Err("フォルダは絶対パスで指定してください。".to_string());
    }
    let Some(Component::Prefix(prefix)) = candidate.components().next() else {
        return Err("フォルダはドライブ文字から始まるローカルパスで指定してください。".to_string());
    };
    match prefix.kind() {
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {}
        _ => {
            return Err(
                "UNC パス (\\\\server\\share) は指定できません。ローカルのフォルダを選んでください。"
                    .to_string(),
            );
        }
    }
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("パスに「..」は指定できません。".to_string());
    }
    Ok(candidate)
}

/// モデルの保存先を変更する。空文字なら既定の場所に戻す。
pub fn set_models_dir(settings: &SettingsStore, path: &str) -> Result<Settings, String> {
    let trimmed = path.trim();
    let value = if trimmed.is_empty() {
        None
    } else {
        let candidate = validate_models_dir(trimmed)?;
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
pub struct ModelState {
    /// エンジンが起動に使えるモデルを解決できたか (管理対象・手動パス・環境変数・探索を含む)。
    pub resolved: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupState {
    pub runtime: RuntimeState,
    pub models: Vec<InstalledModel>,
    pub active_model: Option<String>,
    pub models_dir: String,
    pub model: ModelState,
}

/// 設定画面が使う現在の導入状態。
pub fn setup_state(app: &AppHandle, settings: &SettingsStore) -> SetupState {
    let current = settings.get();
    let search_root = engine::exe_directory();
    let default_server = engine::default_runtime_server_path(app);
    let resolved =
        engine::resolve_runtime_path(&current, default_server.as_deref(), &search_root).ok();
    let resolved_model = engine::resolve_model_path(&current, &search_root).ok();

    let version = current.installed_runtime.as_ref().and_then(|runtime| {
        let managed = PathBuf::from(&runtime.server_path);
        (resolved.as_deref() == Some(managed.as_path())).then(|| runtime.version.clone())
    });
    // ディスク使用量は管理対象 (または既定の場所) のときだけ数える。
    // 外部の server_path を指定したユーザーのディレクトリを勝手に走査しない。
    let is_managed_runtime = resolved.as_deref().is_some_and(|path| {
        current
            .installed_runtime
            .as_ref()
            .is_some_and(|runtime| Path::new(&runtime.server_path) == path)
            || default_server.as_deref() == Some(path)
    });
    let bytes = if is_managed_runtime {
        resolved
            .as_deref()
            .and_then(Path::parent)
            .map(dir_size)
            .filter(|size| *size > 0)
    } else {
        None
    };
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
        model: ModelState {
            resolved: resolved_model.is_some(),
        },
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

    #[test]
    fn pinned_archives_have_sha256_and_expected_sizes() {
        for archive in [&LLAMA_ZIP, &CUDART_ZIP] {
            assert_eq!(archive.sha256.len(), 64, "{}", archive.file_name);
            assert!(archive
                .sha256
                .chars()
                .all(|character| character.is_ascii_hexdigit()));
            assert!(archive.bytes > 0);
            assert!(archive
                .url
                .starts_with("https://github.com/ggml-org/llama.cpp/releases/"));
        }
        assert_eq!(LLAMA_ZIP.bytes, 288_912_954);
        assert_eq!(CUDART_ZIP.bytes, 391_443_627);
        assert_eq!(
            LLAMA_ZIP.file_name,
            "llama-b12136-bin-win-cuda-12.4-x64.zip"
        );
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        let directory = temp_dir("sha256-vector");
        let path = directory.join("abc.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).expect("計算できる"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn verify_archive_checks_size_and_hash() {
        let directory = temp_dir("verify-archive");
        let path = directory.join("archive.zip");
        fs::write(&path, b"abc").unwrap();
        let good = RuntimeArchive {
            url: "https://example.invalid/a.zip",
            file_name: "a.zip",
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(verify_archive(&path, &good).is_ok());

        let wrong_hash = RuntimeArchive {
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            ..good
        };
        let error = verify_archive(&path, &wrong_hash).expect_err("ハッシュ不一致は失敗");
        assert!(error.message().contains("SHA-256"));

        let wrong_size = RuntimeArchive { bytes: 4, ..good };
        let error = verify_archive(&path, &wrong_size).expect_err("サイズ不一致は失敗");
        assert!(error.message().contains("サイズ"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn existing_model_file_classifies_match_and_mismatch() {
        let directory = temp_dir("existing-model");
        let path = directory.join("model.gguf");
        assert!(!existing_model_file(&path, 3).expect("無い場合は false"));
        fs::write(&path, b"abc").unwrap();
        assert!(existing_model_file(&path, 3).expect("同サイズは true"));
        let error = existing_model_file(&path, 4).expect_err("サイズ違いは拒否");
        assert!(error.message().contains("サイズの合わないファイル"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn swap_runtime_dir_replaces_the_target() {
        let directory = temp_dir("runtime-swap");
        let staging = directory.join("llama.staging");
        let target = directory.join("llama");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("new.txt"), b"new").unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("old.txt"), b"old").unwrap();
        swap_runtime_dir(&staging, &target).expect("入れ替えられる");
        assert!(!staging.exists());
        assert!(target.join("new.txt").is_file());
        assert!(!target.join("old.txt").exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn validate_models_dir_allows_only_local_absolute_paths() {
        assert!(validate_models_dir("C:\\Temp\\nuka-models").is_ok());
        assert!(validate_models_dir("relative\\models").is_err());
        let unc = validate_models_dir("\\\\server\\share\\models").expect_err("UNC は拒否");
        assert!(unc.contains("UNC"));
        let parent = validate_models_dir("C:\\Temp\\..\\models").expect_err(".. は拒否");
        assert!(parent.contains(".."));
    }

    #[test]
    fn set_models_dir_creates_and_clears_the_setting() {
        let store = SettingsStore::in_memory();
        let directory = temp_dir("models-dir-set");
        let target = directory.join("custom-models");
        let settings = set_models_dir(&store, &target.to_string_lossy()).expect("変更できるはず");
        assert_eq!(
            settings.models_dir.as_deref(),
            Some(target.to_string_lossy().as_ref())
        );
        assert!(target.is_dir());
        assert!(set_models_dir(&store, "C:\\Temp\\..\\escape").is_err());
        let cleared = set_models_dir(&store, "  ").expect("既定に戻せる");
        assert!(cleared.models_dir.is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn delete_model_checks_catalog_and_file_name() {
        let directory = temp_dir("delete-model");
        let store = SettingsStore::new(directory.join("settings.json"));
        let file = "plamo-3-translate-0.8b-Q2_K.gguf";
        let path = directory.join(file);
        fs::write(&path, b"x").unwrap();
        store
            .update(|next| {
                next.installed_models.push(InstalledModel {
                    file: file.to_string(),
                    repo: "pfnet/plamo-3-translate-0.8b-GGUF".to_string(),
                    bytes: 1,
                    path: path.to_string_lossy().into_owned(),
                    installed_at: 0,
                });
                Ok(())
            })
            .expect("記録できる");

        let wrong_name = directory.join("other.gguf");
        fs::write(&wrong_name, b"x").unwrap();
        store
            .update(|next| {
                next.installed_models[0].path = wrong_name.to_string_lossy().into_owned();
                Ok(())
            })
            .unwrap();
        let error = delete_model(&store, file).expect_err("名前が違えば拒否");
        assert!(error.contains("ファイル名が一致しません"));
        assert!(wrong_name.exists());

        store
            .update(|next| {
                next.installed_models[0].path = path.to_string_lossy().into_owned();
                Ok(())
            })
            .unwrap();
        let updated = delete_model(&store, file).expect("削除できる");
        assert!(updated.installed_models.is_empty());
        assert!(!path.exists());

        let error = delete_model(&store, "unknown.gguf").expect_err("カタログにない");
        assert!(error.contains("カタログにない"));
        let _ = fs::remove_dir_all(&directory);
    }
}
