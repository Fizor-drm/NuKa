//! GitHub Releases を使ったアプリ内アップデーター。
//!
//! - 確認: `releases/latest` を 1 回だけ GET し、現在バージョンと比較する。
//! - ダウンロード: 検証済みの asset だけを `{app_cache_dir}/update/` へ保存する。
//! - インストール: ダウンロードしたインストーラーを切り離して起動し、アプリを終了する。
//!
//! リモートの応答は信用しない。asset の URL は GitHub のリリース配布元 prefix に
//! 一致するものだけを使い、サイズは 256MB を上限にする。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::engine::{Engine, EnginePhase};
use crate::installer::DownloadManager;
use crate::message::{text, LocalizedText};
use crate::settings::SettingsStore;

pub const REPO: &str = "Fizor-drm/NuKa";
pub const RELEASES_URL: &str = "https://github.com/Fizor-drm/NuKa/releases/latest";
pub const DOWNLOAD_ID: &str = "update";
const API_URL: &str = "https://api.github.com/repos/Fizor-drm/NuKa/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/Fizor-drm/NuKa/releases/download/";
const USER_AGENT: &str = concat!("NuKa/", env!("CARGO_PKG_VERSION"));
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// 更新インストーラーのサイズ上限。
pub const MAX_UPDATE_BYTES: u64 = 256 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
const INSTALLER_EXIT_DELAY: Duration = Duration::from_millis(800);

// ---------------------------------------------------------------------------
// 型
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdateAsset {
    pub name: String,
    pub size: u64,
    pub url: String,
    /// 正規化済みの SHA-256 (64 文字 hex)。検証必須。
    pub digest: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateCheck {
    pub current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<UpdateAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LocalizedText>,
}

/// 確認結果として保持する「更新候補」。ダウンロードはこの内容だけを使う。
#[derive(Clone, Debug)]
struct UpdateOffer {
    version: String,
    asset: UpdateAsset,
}

#[derive(Clone, Debug)]
struct DownloadedUpdate {
    path: PathBuf,
    version: String,
}

/// アップデーターの状態。フロントエンドからはコマンド経由でのみ触る。
#[derive(Clone, Default)]
pub struct UpdateState {
    offer: Arc<Mutex<Option<UpdateOffer>>>,
    downloaded: Arc<Mutex<Option<DownloadedUpdate>>>,
}

impl UpdateState {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_offer(&self) -> MutexGuard<'_, Option<UpdateOffer>> {
        self.offer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_downloaded(&self) -> MutexGuard<'_, Option<DownloadedUpdate>> {
        self.downloaded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    downloaded: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct DoneEvent {
    version: String,
}

#[derive(Clone, Serialize)]
struct ErrorEvent {
    message: LocalizedText,
}

fn emit_progress(app: &AppHandle, downloaded: u64, total: u64) {
    if let Err(error) = app.emit("update:progress", ProgressEvent { downloaded, total }) {
        eprintln!("update:progress を送れません: {error}");
    }
}

fn emit_done(app: &AppHandle, version: &str) {
    if let Err(error) = app.emit(
        "update:done",
        DoneEvent {
            version: version.to_string(),
        },
    ) {
        eprintln!("update:done を送れません: {error}");
    }
}

fn emit_error(app: &AppHandle, message: &LocalizedText) {
    if let Err(error) = app.emit(
        "update:error",
        ErrorEvent {
            message: message.clone(),
        },
    ) {
        eprintln!("update:error を送れません: {error}");
    }
}

// ---------------------------------------------------------------------------
// GitHub API
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// 現在バージョン (env の CARGO_PKG_VERSION)。
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `v` を除いた 3 つ組の数値だけを受け付ける。それ以外は None。
fn parse_version(input: &str) -> Option<(u64, u64, u64)> {
    let trimmed = input.trim();
    let without_v = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    let mut parts = without_v.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// `latest` が `current` より新しければ Some(true)。比較できなければ None。
fn is_newer(latest: &str, current: &str) -> Option<bool> {
    let latest = parse_version(latest)?;
    let current = parse_version(current)?;
    Some(latest > current)
}

/// `sha256:<hex>` だけを受け付け、比較用の 64 文字小文字 hex に正規化する。
/// ここで 1 回だけ正規化し、以後は正規化済みの値だけを扱う。
fn normalize_digest(digest: Option<&str>) -> Option<String> {
    let digest = digest?.trim();
    let hex = digest.strip_prefix("sha256:")?;
    if hex.len() == 64 && hex.chars().all(|character| character.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

/// asset 名として安全か。ディレクトリ区切りや `..`、ドライブ指定を拒否する。
fn is_safe_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

/// ダウンロード先 URL が GitHub のリリース配布元として正しいか。
fn is_trusted_asset_url(url: &str) -> bool {
    if !url.starts_with(DOWNLOAD_PREFIX) || url.contains("..") {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        return false;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return false;
    }
    let path = parsed.path();
    path.starts_with("/Fizor-drm/NuKa/releases/download/") && !path.contains("..")
}

/// 更新に使える asset か。名前・URL・digest をすべて検証する。
fn select_asset(assets: &[GitHubAsset]) -> Result<UpdateAsset, LocalizedText> {
    let candidates: Vec<&GitHubAsset> = assets
        .iter()
        .filter(|asset| asset.name.starts_with("NuKa_") && asset.name.ends_with("_x64-setup.exe"))
        .collect();
    if candidates.is_empty() {
        return Err(text(
            "updater.assetMissing",
            "更新ファイル (NuKa_*_x64-setup.exe) が見つかりませんでした。",
        ));
    }
    let Some(asset) = candidates
        .into_iter()
        .find(|asset| is_safe_asset_name(&asset.name))
    else {
        return Err(text(
            "updater.assetName",
            "更新ファイル名が想定と違います。",
        ));
    };

    if asset.size == 0 || asset.size > MAX_UPDATE_BYTES {
        return Err(text(
            "updater.assetSize",
            "更新ファイルのサイズが想定の範囲外です。",
        ));
    }
    if !is_trusted_asset_url(&asset.browser_download_url) {
        return Err(text(
            "updater.assetUrl",
            "更新ファイルの取得先が想定と違います。",
        ));
    }
    let digest = normalize_digest(asset.digest.as_deref()).ok_or_else(|| {
        text(
            "updater.digestMissing",
            "更新ファイルの検証情報が取得できませんでした。",
        )
    })?;
    Ok(UpdateAsset {
        name: asset.name.clone(),
        size: asset.size,
        url: asset.browser_download_url.clone(),
        digest,
    })
}

fn evaluate_release(current: &str, release: GitHubRelease) -> UpdateCheck {
    let latest = release
        .tag_name
        .trim()
        .trim_start_matches(['v', 'V'])
        .to_string();
    let base = UpdateCheck {
        current: current.to_string(),
        latest: Some(latest.clone()),
        available: false,
        asset: None,
        error: None,
    };
    let Some(newer) = is_newer(&latest, current) else {
        return UpdateCheck {
            error: Some(
                text(
                    "updater.versionCompare",
                    format!("バージョンを比較できませんでした (現在 {current} / 最新 {latest})。"),
                )
                .param("current", current)
                .param("latest", latest),
            ),
            ..base
        };
    };
    if !newer {
        return base;
    }
    match select_asset(&release.assets) {
        Ok(asset) => UpdateCheck {
            available: true,
            asset: Some(asset),
            ..base
        },
        Err(message) => UpdateCheck {
            error: Some(message),
            ..base
        },
    }
}

fn check_client() -> Result<reqwest::Client, LocalizedText> {
    reqwest::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .connect_timeout(CHECK_TIMEOUT)
        .timeout(CHECK_TIMEOUT)
        .build()
        .map_err(|error| {
            text(
                "updater.checkClient",
                format!("更新確認用クライアントを作れません: {error}"),
            )
            .param("detail", error)
        })
}

async fn fetch_release() -> Result<GitHubRelease, LocalizedText> {
    let client = check_client()?;
    let response = client
        .get(API_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| {
            text(
                "updater.checkFailed",
                format!("更新情報を取得できませんでした: {error}"),
            )
            .param("detail", error)
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(text(
            "updater.httpStatus",
            format!("更新情報を取得できませんでした (HTTP {status})。"),
        )
        .param("status", status));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        match tokio::time::timeout(CHECK_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_RESPONSE_BYTES {
                    return Err(text("updater.responseTooLarge", "更新情報が大きすぎます。"));
                }
            }
            Ok(Some(Err(error))) => {
                return Err(text(
                    "updater.responseRead",
                    format!("更新情報を読めませんでした: {error}"),
                )
                .param("detail", error));
            }
            Ok(None) => break,
            Err(_) => {
                return Err(text(
                    "updater.responseTimeout",
                    "更新情報の取得がタイムアウトしました。",
                ))
            }
        }
    }
    serde_json::from_slice::<GitHubRelease>(&body).map_err(|error| {
        text(
            "updater.parseFailed",
            format!("更新情報を解析できませんでした: {error}"),
        )
        .param("detail", error)
    })
}

/// GitHub の最新リリースを確認する。失敗しても Result ではなく `error` に入れて返す。
pub async fn check(_app: &AppHandle, state: &UpdateState) -> UpdateCheck {
    let current = current_version();
    let result = fetch_release().await;
    let check = match result {
        Ok(release) => evaluate_release(&current, release),
        Err(message) => UpdateCheck {
            current: current.clone(),
            latest: None,
            available: false,
            asset: None,
            error: Some(message),
        },
    };
    let mut offer = state.lock_offer();
    *offer = if check.available {
        match &check.asset {
            Some(asset) => Some(UpdateOffer {
                version: check.latest.clone().unwrap_or_default(),
                asset: asset.clone(),
            }),
            None => None,
        }
    } else {
        None
    };
    check
}

// ---------------------------------------------------------------------------
// ダウンロード
// ---------------------------------------------------------------------------

fn cache_dir(app: &AppHandle) -> Result<PathBuf, LocalizedText> {
    app.path()
        .app_cache_dir()
        .map(|directory| directory.join("update"))
        .map_err(|error| {
            text(
                "updater.cachePath",
                format!("保存先を取得できません: {error}"),
            )
            .param("detail", error)
        })
}

/// 起動時に、中断されたダウンロードやインストーラーの残骸を片付ける。
pub fn clean_cache(app: &AppHandle) {
    if let Ok(directory) = cache_dir(app) {
        let _ = fs::remove_dir_all(&directory);
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("update");
    dest.with_file_name(format!("{name}.part"))
}

fn remove_if_exists(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

fn sha256_file(path: &Path) -> Result<String, LocalizedText> {
    let mut file = fs::File::open(path).map_err(|error| {
        eprintln!("SHA-256 の対象を開けません ({}): {error}", path.display());
        text("updater.fileCheck", "SHA-256 を計算できませんでした。")
    })?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|error| {
        eprintln!("SHA-256 を計算できません ({}): {error}", path.display());
        text("updater.fileCheck", "SHA-256 を計算できませんでした。")
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// ダウンロードしたファイルを検証する。SHA-256 は必須で、正規化済みの 64 文字 hex を受ける。
/// ここで再正規化はしない (二重適用で検証が素通りする事故を防ぐ)。
fn verify_download(path: &Path, expected_size: u64, digest: &str) -> Result<(), LocalizedText> {
    let size = fs::metadata(path)
        .map_err(|error| {
            text(
                "updater.fileCheck",
                format!("更新ファイルを確認できません: {error}"),
            )
            .param("detail", error)
        })?
        .len();
    if size != expected_size {
        return Err(text(
            "updater.sizeMismatch",
            format!("更新ファイルのサイズが一致しません ({size} / {expected_size} バイト)。"),
        )
        .param("actual", size)
        .param("expected", expected_size));
    }
    let expected = digest.trim().to_ascii_lowercase();
    if expected.len() != 64
        || !expected
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(text(
            "updater.digestInvalid",
            "更新ファイルの検証情報が不正です。",
        ));
    }
    let actual = sha256_file(path)?;
    if actual != expected {
        eprintln!("更新ファイルの SHA-256 が一致しません (期待 {expected} / 実際 {actual})");
        return Err(text(
            "updater.hashMismatch",
            "更新ファイルの SHA-256 が一致しません。",
        ));
    }
    Ok(())
}

fn download_client() -> Result<reqwest::Client, LocalizedText> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT);
    // テストではローカル HTTP のスタブに接続するため、テスト時だけ https 強制を外す。
    #[cfg(not(test))]
    let builder = builder.https_only(true);
    builder.build().map_err(|error| {
        text(
            "updater.downloadClient",
            format!("ダウンロード用クライアントを作れません: {error}"),
        )
        .param("detail", error)
    })
}

/// 実ファイルのダウンロードと検証。保存先を引数で受けるので、テストからも同じ経路を通せる。
async fn download_to_directory(
    asset: &UpdateAsset,
    directory: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<PathBuf, LocalizedText> {
    if asset.size == 0 || asset.size > MAX_UPDATE_BYTES {
        return Err(text(
            "updater.assetSize",
            "更新ファイルのサイズが想定の範囲外です。",
        ));
    }
    if !is_safe_asset_name(&asset.name) {
        return Err(text(
            "updater.assetName",
            "更新ファイル名が想定と違います。",
        ));
    }
    if asset.digest.len() != 64
        || !asset
            .digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(text(
            "updater.digestInvalid",
            "更新ファイルの検証情報が不正です。",
        ));
    }
    let dest = directory.join(&asset.name);
    let part = part_path(&dest);
    remove_if_exists(&part);
    remove_if_exists(&dest);

    let client = download_client()?;
    let response = client.get(&asset.url).send().await.map_err(|error| {
        text(
            "updater.downloadStart",
            format!("ダウンロードを開始できません: {error}"),
        )
        .param("detail", error)
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(text(
            "updater.httpStatus",
            format!("配布元が {status} を返しました。"),
        )
        .param("status", status));
    }
    if let Some(length) = response.content_length() {
        if length != asset.size {
            return Err(text(
                "updater.sizeMismatch",
                format!(
                    "配布元のサイズが想定と違います ({length} / {} バイト)。",
                    asset.size
                ),
            )
            .param("actual", length)
            .param("expected", asset.size));
        }
    }

    let mut file = tokio::fs::File::create(&part).await.map_err(|error| {
        text("updater.savePath", format!("保存先を作れません: {error}")).param("detail", error)
    })?;
    let mut stream = response.bytes_stream();
    let mut written: u64 = 0;
    let mut last_emit = Instant::now();
    on_progress(0, asset.size);

    loop {
        if cancel.load(Ordering::SeqCst) {
            drop(file);
            remove_if_exists(&part);
            return Err(text("updater.cancelled", "ダウンロードを中止しました。"));
        }
        let item = tokio::time::timeout(READ_TIMEOUT, stream.next()).await;
        let chunk = match item {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                drop(file);
                remove_if_exists(&part);
                return Err(text(
                    "updater.readFailed",
                    format!("ダウンロードを読めません: {error}"),
                )
                .param("detail", error));
            }
            Ok(None) => break,
            Err(_) => {
                drop(file);
                remove_if_exists(&part);
                return Err(text(
                    "updater.timeout",
                    "ダウンロードがタイムアウトしました。",
                ));
            }
        };
        if let Err(error) = file.write_all(&chunk).await {
            drop(file);
            remove_if_exists(&part);
            return Err(text(
                "updater.writeFailed",
                format!("ダウンロードを書き込めません: {error}"),
            )
            .param("detail", error));
        }
        written += chunk.len() as u64;
        if written > asset.size || written > MAX_UPDATE_BYTES {
            drop(file);
            remove_if_exists(&part);
            return Err(text(
                "updater.tooLarge",
                "更新ファイルが想定より大きいため中止しました。",
            ));
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            on_progress(written, asset.size);
            last_emit = Instant::now();
        }
    }
    if let Err(error) = file.flush().await {
        drop(file);
        remove_if_exists(&part);
        return Err(text(
            "updater.writeFailed",
            format!("ダウンロードを書き込めません: {error}"),
        )
        .param("detail", error));
    }
    drop(file);
    on_progress(written, asset.size);

    if let Err(message) = verify_download(&part, asset.size, &asset.digest) {
        remove_if_exists(&part);
        return Err(message);
    }
    eprintln!("更新ファイルの SHA-256 を確認しました");
    if cancel.load(Ordering::SeqCst) {
        remove_if_exists(&part);
        return Err(text("updater.cancelled", "ダウンロードを中止しました。"));
    }
    fs::rename(&part, &dest).map_err(|error| {
        remove_if_exists(&part);
        text(
            "updater.renameFailed",
            format!("更新ファイルを配置できません: {error}"),
        )
        .param("detail", error)
    })?;
    Ok(dest)
}

async fn download_asset(
    app: &AppHandle,
    offer: &UpdateOffer,
    cancel: &AtomicBool,
) -> Result<PathBuf, LocalizedText> {
    let asset = &offer.asset;
    // 保持している URL も再検証する。
    if !is_trusted_asset_url(&asset.url) {
        return Err(text(
            "updater.assetUrl",
            "更新ファイルの取得先が想定と違います。",
        ));
    }
    let directory = cache_dir(app)?;
    fs::create_dir_all(&directory).map_err(|error| {
        text("updater.savePath", format!("保存先を作れません: {error}")).param("detail", error)
    })?;
    download_to_directory(asset, &directory, cancel, |downloaded, total| {
        emit_progress(app, downloaded, total);
    })
    .await
}

/// 更新インストーラーをダウンロードして保持する。成功/失敗はイベントで流す。
pub async fn download(
    app: &AppHandle,
    state: &UpdateState,
    downloads: &DownloadManager,
) -> Result<(), LocalizedText> {
    let offer = {
        let guard = state.lock_offer();
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| text("updater.notChecked", "先に更新を確認してください。"))?
    };
    let cancel = downloads.begin(DOWNLOAD_ID)?;
    *state.lock_downloaded() = None;
    let result = download_asset(app, &offer, &cancel).await;
    downloads.finish(DOWNLOAD_ID);
    match result {
        Ok(path) => {
            *state.lock_downloaded() = Some(DownloadedUpdate {
                path,
                version: offer.version.clone(),
            });
            emit_done(app, &offer.version);
            Ok(())
        }
        Err(message) => {
            emit_error(app, &message);
            Err(message)
        }
    }
}

/// キャンセル要求。進行中でなければ何もしない。
pub fn cancel(downloads: &DownloadManager) -> bool {
    downloads.cancel(DOWNLOAD_ID)
}

// ---------------------------------------------------------------------------
// インストール
// ---------------------------------------------------------------------------

/// spawn 失敗時にエンジンを再起動すべきか。以前 ready だったときだけ復元する
/// (元々止まっていたなら止まったままにする)。
fn should_restore_engine(state: &EnginePhase) -> bool {
    matches!(state, EnginePhase::Ready)
}

/// ダウンロード済みのインストーラーを切り離して起動し、少し待ってアプリを終了する。
pub fn install(
    app: &AppHandle,
    state: &UpdateState,
    engine: &Engine,
    settings: &SettingsStore,
) -> Result<(), LocalizedText> {
    // take() で消費し、二重起動を防ぐ。起動に失敗したときだけ戻す。
    let downloaded = {
        let mut guard = state.lock_downloaded();
        guard.take().ok_or_else(|| {
            text(
                "updater.notDownloaded",
                "更新ファイルがありません。先にダウンロードしてください。",
            )
        })?
    };
    let restore = |message: LocalizedText| -> Result<(), LocalizedText> {
        *state.lock_downloaded() = Some(downloaded.clone());
        Err(message)
    };

    let directory = match cache_dir(app) {
        Ok(directory) => directory,
        Err(error) => return restore(error),
    };
    if downloaded.path.parent() != Some(directory.as_path()) {
        return restore(text(
            "updater.installPath",
            "更新ファイルの保存先が想定と違います。",
        ));
    }
    let metadata = match fs::symlink_metadata(&downloaded.path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return restore(
                text(
                    "updater.fileCheck",
                    format!("更新ファイルを確認できません: {error}"),
                )
                .param("detail", error),
            );
        }
    };
    if metadata.file_type().is_symlink() {
        return restore(text(
            "updater.installSymlink",
            "更新ファイルがシンボリックリンクのため中止しました。",
        ));
    }
    if !metadata.is_file() {
        return restore(text(
            "updater.installMissing",
            "更新ファイルが見つかりません。再ダウンロードしてください。",
        ));
    }

    // インストーラーとファイルロックが競合しないよう、先にエンジンを止める。
    // spawn に失敗したときは、もともと動いていた場合だけ裏で復元する。
    let engine_was_ready = should_restore_engine(&engine.status().state);
    engine.kill_now();

    let mut command = Command::new(&downloaded.path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if engine_was_ready {
                let engine = engine.clone();
                let settings = settings.get();
                tauri::async_runtime::spawn(async move {
                    engine.start(&settings).await;
                });
            }
            return restore(
                text(
                    "updater.installSpawn",
                    format!("インストーラーを起動できません: {error}"),
                )
                .param("detail", error),
            );
        }
    };
    let pid = child.id();
    eprintln!(
        "更新インストーラー ({}) を起動しました (pid {pid})。NuKa を終了します。",
        downloaded.version
    );

    // 応答を返して UI のメッセージを描画させてから終了する。
    std::thread::spawn(move || {
        std::thread::sleep(INSTALLER_EXIT_DELAY);
        // Job Object (llama-server) は OS がプロセス終了時に閉じ、子も道連れになる。
        std::process::exit(0);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn asset(name: &str, size: u64, url: &str, digest: Option<&str>) -> GitHubAsset {
        GitHubAsset {
            name: name.to_string(),
            size,
            browser_download_url: url.to_string(),
            digest: digest.map(str::to_string),
        }
    }

    const GOOD_URL: &str =
        "https://github.com/Fizor-drm/NuKa/releases/download/v1.0.0/NuKa_1.0.0_x64-setup.exe";
    const TEST_DIGEST: &str = "28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161";
    const TEST_DIGEST_FIELD: &str =
        "sha256:28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161";

    fn temp_dir(label: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("nuka-updater-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("一時ディレクトリを作れる");
        directory
    }

    fn hex_sha256(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    /// 決められた本文を返す小さな HTTP サーバ (テスト専用)。
    async fn spawn_stub(body: Vec<u8>) -> u16 {
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
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        });
        port
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("V10.0.7"), Some((10, 0, 7)));
        assert_eq!(parse_version(" 1.0.0 "), Some((1, 0, 0)));

        assert!(is_newer("1.0.0", "0.9.9").expect("比較できる"));
        assert!(!is_newer("1.0.0", "1.1.0").expect("比較できる"));
        assert!(!is_newer("1.0.0", "1.0.0").expect("比較できる"));
    }

    #[test]
    fn invalid_versions_are_not_comparable() {
        for value in [
            "",
            "1",
            "1.0",
            "1.0.0.0",
            "a.b.c",
            "1.0.0-beta",
            "v",
            "1..0",
            "1.0.0-rc1",
        ] {
            assert_eq!(parse_version(value), None, "{value}");
        }
        assert!(is_newer("1.0.0-rc1", "0.9.9").is_none());
        assert!(is_newer("1.0.0", "main").is_none());
    }

    #[test]
    fn release_evaluation_picks_updates_and_reports_errors() {
        let current = "0.9.9";
        let release = GitHubRelease {
            tag_name: "v1.0.0".to_string(),
            assets: vec![asset(
                "NuKa_1.0.0_x64-setup.exe",
                3_194_862,
                GOOD_URL,
                Some("sha256:28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161"),
            )],
        };
        let check = evaluate_release(current, release);
        assert!(check.available);
        assert_eq!(check.latest.as_deref(), Some("1.0.0"));
        let selected = check.asset.expect("asset がある");
        assert_eq!(selected.name, "NuKa_1.0.0_x64-setup.exe");
        assert_eq!(selected.size, 3_194_862);
        assert_eq!(
            selected.digest,
            "28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161"
        );

        // 現在の方が新しければ更新なし
        let check = evaluate_release(
            "1.1.0",
            GitHubRelease {
                tag_name: "v1.0.0".to_string(),
                assets: vec![asset("NuKa_1.0.0_x64-setup.exe", 3, GOOD_URL, None)],
            },
        );
        assert!(!check.available);
        assert_eq!(check.latest.as_deref(), Some("1.0.0"));

        // 比較不能
        let check = evaluate_release(
            "1.1.0",
            GitHubRelease {
                tag_name: "latest".to_string(),
                assets: vec![],
            },
        );
        assert!(!check.available);
        assert!(check
            .error
            .expect("エラーがある")
            .fallback
            .contains("比較できません"));
    }

    #[test]
    fn asset_selection_rejects_untrusted_urls_and_wrong_names() {
        let digest = Some(TEST_DIGEST_FIELD);
        let assets = vec![
            asset("NuKa_1.0.0_x64-setup.exe", 10, GOOD_URL, digest),
            asset("NuKa_1.0.0_x64.msi", 10, GOOD_URL, digest),
            asset("notes.txt", 10, GOOD_URL, digest),
        ];
        let selected = select_asset(&assets).expect("選べる");
        assert_eq!(selected.name, "NuKa_1.0.0_x64-setup.exe");
        assert_eq!(selected.digest, TEST_DIGEST);

        let evil = vec![asset(
            "NuKa_1.0.0_x64-setup.exe",
            10,
            "https://example.com/NuKa_1.0.0_x64-setup.exe",
            digest,
        )];
        let error = select_asset(&evil).expect_err("URL が違えば拒否");
        assert!(error.fallback.contains("取得先"));

        let too_large = vec![asset(
            "NuKa_1.0.0_x64-setup.exe",
            MAX_UPDATE_BYTES + 1,
            GOOD_URL,
            digest,
        )];
        assert!(select_asset(&too_large).is_err());

        assert!(select_asset(&[]).is_err());
    }

    #[test]
    fn asset_selection_requires_a_valid_sha256_digest() {
        for digest in [
            None,
            Some("sha256:xyz"),
            Some("sha512:28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161"),
            Some("md5:28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161"),
            Some(""),
        ] {
            let assets = vec![asset("NuKa_1.0.0_x64-setup.exe", 10, GOOD_URL, digest)];
            let error = select_asset(&assets).expect_err("digest 不備は拒否");
            assert!(error.fallback.contains("検証情報"), "{digest:?}: {error:?}");
        }
    }

    #[test]
    fn asset_names_and_urls_reject_traversal() {
        for name in [
            "NuKa_../evil_x64-setup.exe",
            "NuKa_/evil_x64-setup.exe",
            "NuKa_\\evil_x64-setup.exe",
            "NuKa_C:\\evil_x64-setup.exe",
        ] {
            let assets = vec![asset(name, 10, GOOD_URL, Some(TEST_DIGEST_FIELD))];
            let error = select_asset(&assets).expect_err("危険な名前は拒否");
            assert!(error.fallback.contains("ファイル名"), "{name}: {error:?}");
        }

        let evil_url = "https://github.com/Fizor-drm/NuKa/releases/download/v1.0.0/../evil/NuKa_1.0.0_x64-setup.exe";
        assert!(!is_trusted_asset_url(evil_url));
        assert!(!is_trusted_asset_url(
            "https://evil.example/NuKa_1.0.0_x64-setup.exe"
        ));
        assert!(is_trusted_asset_url(GOOD_URL));
        let assets = vec![asset(
            "NuKa_1.0.0_x64-setup.exe",
            10,
            evil_url,
            Some(TEST_DIGEST_FIELD),
        )];
        let error = select_asset(&assets).expect_err(".. を含む URL は拒否");
        assert!(error.fallback.contains("取得先"), "{error:?}");
    }

    #[test]
    fn digest_normalization_accepts_only_sha256_hex() {
        assert_eq!(
            normalize_digest(Some(
                "sha256:28B283711529BA5D2C919BA2277376DA95D0DF95A8D0B340C4EEC35C1C64A161"
            ))
            .as_deref(),
            Some("28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161")
        );
        assert!(normalize_digest(Some("sha256:xyz")).is_none());
        assert!(normalize_digest(Some("md5:abc")).is_none());
        assert!(normalize_digest(None).is_none());
    }

    #[test]
    fn verify_download_checks_size_and_digest() {
        let directory = temp_dir("verify");
        let path = directory.join("installer.exe");
        fs::write(&path, b"abc").unwrap();
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        assert!(verify_download(&path, 3, digest).is_ok());
        // 大文字 hex も比較できる
        assert!(verify_download(&path, 3, &digest.to_ascii_uppercase()).is_ok());
        assert!(verify_download(&path, 4, digest).is_err());
        assert!(verify_download(
            &path,
            3,
            "0000000000000000000000000000000000000000000000000000000000000000"
        )
        .is_err());
        // 正規化済み hex だけを受ける (sha256: 付きを渡したら不正として拒否)
        assert!(verify_download(&path, 3, &format!("sha256:{digest}")).is_err());
        assert!(verify_download(&path, 3, "not-a-digest").is_err());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn engine_is_restored_only_when_it_was_ready() {
        assert!(should_restore_engine(&EnginePhase::Ready));
        assert!(!should_restore_engine(&EnginePhase::Starting));
        assert!(!should_restore_engine(&EnginePhase::Loading));
        assert!(!should_restore_engine(&EnginePhase::Error));
    }

    #[test]
    fn tauri_and_package_versions_match_cargo_version() {
        let tauri: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(
            tauri["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "tauri.conf.json の version が Cargo.toml と一致しません"
        );
        let package: serde_json::Value =
            serde_json::from_str(include_str!("../../package.json")).expect("package.json");
        assert_eq!(
            package["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "package.json の version が Cargo.toml と一致しません"
        );
    }

    /// select_asset で選んだ digest が、実際のダウンロード経路 (download_to_directory) で
    /// SHA-256 検証に使われることを、スタブ HTTP 経由で確認する。
    #[tokio::test]
    async fn download_path_verifies_sha256_end_to_end() {
        let directory = temp_dir("download-e2e");
        let body = b"NuKa test installer bytes".to_vec();
        let digest = hex_sha256(&body);

        // 1) GitHub の asset から digest を正規化して取り出す
        let release_asset = asset(
            "NuKa_1.0.0_x64-setup.exe",
            body.len() as u64,
            GOOD_URL,
            Some(&format!("sha256:{digest}")),
        );
        let selected = select_asset(&[release_asset]).expect("選択できる");
        assert_eq!(selected.digest, digest);

        // 2) 同じ digest のまま、配布物だけスタブに差し替えてダウンロード
        let port = spawn_stub(body.clone()).await;
        let download = UpdateAsset {
            url: format!("http://127.0.0.1:{port}/NuKa_1.0.0_x64-setup.exe"),
            ..selected
        };
        let cancel = AtomicBool::new(false);
        let path = download_to_directory(&download, &directory, &cancel, |_, _| {})
            .await
            .expect("正しいファイルは通る");
        assert_eq!(fs::read(&path).unwrap(), body);

        // 3) 1 バイトでも変われば拒否される
        let mut tampered = body.clone();
        tampered[0] ^= 0xff;
        fs::write(&path, &tampered).unwrap();
        let error =
            verify_download(&path, download.size, &download.digest).expect_err("改ざんは拒否");
        assert!(error.fallback.contains("SHA-256"));

        // 4) 改ざんされた配信をダウンロードすると失敗し、成果物を残さない
        let port = spawn_stub(tampered).await;
        let bad = UpdateAsset {
            url: format!("http://127.0.0.1:{port}/NuKa_1.0.0_x64-setup.exe"),
            ..download.clone()
        };
        let error = download_to_directory(&bad, &directory, &cancel, |_, _| {})
            .await
            .expect_err("digest 不一致は失敗");
        assert!(error.fallback.contains("SHA-256"));
        assert!(!directory.join(&download.name).exists());
        assert!(!part_path(&directory.join(&download.name)).exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn release_json_tolerates_missing_digest() {
        let json = r#"{
            "tag_name": "v1.0.0",
            "assets": [{
                "name": "NuKa_1.0.0_x64-setup.exe",
                "size": 10,
                "browser_download_url": "https://github.com/Fizor-drm/NuKa/releases/download/v1.0.0/NuKa_1.0.0_x64-setup.exe"
            }]
        }"#;
        let release: GitHubRelease = serde_json::from_str(json).expect("解析できる");
        assert_eq!(release.tag_name, "v1.0.0");
        assert!(release.assets[0].digest.is_none());
    }
}
