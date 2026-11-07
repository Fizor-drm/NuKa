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
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::installer::DownloadManager;

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
    /// `sha256:<hex>`。GitHub が返さない場合は省略される。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
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
    pub error: Option<String>,
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
    path: String,
    version: String,
}

#[derive(Clone, Serialize)]
struct ErrorEvent {
    message: String,
}

fn emit_progress(app: &AppHandle, downloaded: u64, total: u64) {
    if let Err(error) = app.emit("update:progress", ProgressEvent { downloaded, total }) {
        eprintln!("update:progress を送れません: {error}");
    }
}

fn emit_done(app: &AppHandle, path: &str, version: &str) {
    if let Err(error) = app.emit(
        "update:done",
        DoneEvent {
            path: path.to_string(),
            version: version.to_string(),
        },
    ) {
        eprintln!("update:done を送れません: {error}");
    }
}

fn emit_error(app: &AppHandle, message: &str) {
    if let Err(error) = app.emit(
        "update:error",
        ErrorEvent {
            message: message.to_string(),
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

fn normalize_digest(digest: Option<&str>) -> Option<String> {
    let digest = digest?.trim();
    let hex = digest.strip_prefix("sha256:")?;
    if hex.len() == 64 && hex.chars().all(|character| character.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

/// 更新に使える asset か。URL は GitHub のリリース配布元 prefix だけ許可する。
fn select_asset(assets: &[GitHubAsset]) -> Result<UpdateAsset, String> {
    let candidates: Vec<&GitHubAsset> = assets
        .iter()
        .filter(|asset| asset.name.starts_with("NuKa_") && asset.name.ends_with("_x64-setup.exe"))
        .collect();
    if candidates.is_empty() {
        return Err("更新ファイル (NuKa_*_x64-setup.exe) が見つかりませんでした。".to_string());
    }
    let asset = candidates[0];

    if asset.size == 0 || asset.size > MAX_UPDATE_BYTES {
        return Err("更新ファイルのサイズが想定の範囲外です。".to_string());
    }
    if !asset.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
        return Err("更新ファイルの取得先が想定と違います。".to_string());
    }
    Ok(UpdateAsset {
        name: asset.name.clone(),
        size: asset.size,
        url: asset.browser_download_url.clone(),
        digest: normalize_digest(asset.digest.as_deref()),
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
            error: Some(format!(
                "バージョンを比較できませんでした (現在 {current} / 最新 {latest})。"
            )),
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

fn check_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .connect_timeout(CHECK_TIMEOUT)
        .timeout(CHECK_TIMEOUT)
        .build()
        .map_err(|error| format!("更新確認用クライアントを作れません: {error}"))
}

async fn fetch_release() -> Result<GitHubRelease, String> {
    let client = check_client()?;
    let response = client
        .get(API_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("更新情報を取得できませんでした: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            403 | 429 => {
                "GitHub の API 制限に達した可能性があります。時間をおいて再試行してください。"
            }
            404 => "リリースが見つかりませんでした。",
            _ => "時間をおいて再試行してください。",
        };
        return Err(format!(
            "更新情報を取得できませんでした (HTTP {status})。{hint}"
        ));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        match tokio::time::timeout(CHECK_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_RESPONSE_BYTES {
                    return Err("更新情報が大きすぎます。".to_string());
                }
            }
            Ok(Some(Err(error))) => {
                return Err(format!("更新情報を読めませんでした: {error}"));
            }
            Ok(None) => break,
            Err(_) => return Err("更新情報の取得がタイムアウトしました。".to_string()),
        }
    }
    serde_json::from_slice::<GitHubRelease>(&body)
        .map_err(|error| format!("更新情報を解析できませんでした: {error}"))
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

fn cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_cache_dir()
        .map(|directory| directory.join("update"))
        .map_err(|error| format!("保存先を取得できません: {error}"))
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

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("SHA-256 の対象を開けません ({}): {error}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|error| format!("SHA-256 を計算できません: {error}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// ダウンロードしたファイルを検証する。digest があれば SHA-256、無ければサイズ。
fn verify_download(path: &Path, expected_size: u64, digest: Option<&str>) -> Result<(), String> {
    let size = fs::metadata(path)
        .map_err(|error| format!("更新ファイルを確認できません: {error}"))?
        .len();
    if size != expected_size {
        return Err(format!(
            "更新ファイルのサイズが一致しません ({size} / {expected_size} バイト)。"
        ));
    }
    let Some(expected) = normalize_digest(digest) else {
        return Ok(());
    };
    let actual = sha256_file(path)?;
    if actual != expected {
        return Err("更新ファイルの SHA-256 が一致しません。".to_string());
    }
    Ok(())
}

fn download_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| format!("ダウンロード用クライアントを作れません: {error}"))
}

async fn download_asset(
    app: &AppHandle,
    offer: &UpdateOffer,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let asset = &offer.asset;
    // 念のため保持している URL も再検証する。
    if !asset.url.starts_with(DOWNLOAD_PREFIX) {
        return Err("更新ファイルの取得先が想定と違います。".to_string());
    }
    if asset.size == 0 || asset.size > MAX_UPDATE_BYTES {
        return Err("更新ファイルのサイズが想定の範囲外です。".to_string());
    }

    let directory = cache_dir(app)?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("保存先を作れません ({}): {error}", directory.display()))?;
    let dest = directory.join(&asset.name);
    let part = part_path(&dest);
    remove_if_exists(&part);
    remove_if_exists(&dest);

    let client = download_client()?;
    let response = client
        .get(&asset.url)
        .send()
        .await
        .map_err(|error| format!("ダウンロードを開始できません: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("配布元が {status} を返しました。"));
    }
    if let Some(length) = response.content_length() {
        if length != asset.size {
            return Err(format!(
                "配布元のサイズが想定と違います ({length} / {} バイト)。",
                asset.size
            ));
        }
    }

    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|error| format!("保存先を作れません ({}): {error}", part.display()))?;
    let mut stream = response.bytes_stream();
    let mut written: u64 = 0;
    let mut last_emit = Instant::now();
    emit_progress(app, 0, asset.size);

    loop {
        if cancel.load(Ordering::SeqCst) {
            drop(file);
            remove_if_exists(&part);
            return Err("ダウンロードを中止しました。".to_string());
        }
        let item = tokio::time::timeout(READ_TIMEOUT, stream.next()).await;
        let chunk = match item {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                drop(file);
                remove_if_exists(&part);
                return Err(format!("ダウンロードを読めません: {error}"));
            }
            Ok(None) => break,
            Err(_) => {
                drop(file);
                remove_if_exists(&part);
                return Err("ダウンロードがタイムアウトしました。".to_string());
            }
        };
        if let Err(error) = file.write_all(&chunk).await {
            drop(file);
            remove_if_exists(&part);
            return Err(format!("ダウンロードを書き込めません: {error}"));
        }
        written += chunk.len() as u64;
        if written > asset.size || written > MAX_UPDATE_BYTES {
            drop(file);
            remove_if_exists(&part);
            return Err("更新ファイルが想定より大きいため中止しました。".to_string());
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            emit_progress(app, written, asset.size);
            last_emit = Instant::now();
        }
    }
    if let Err(error) = file.flush().await {
        drop(file);
        remove_if_exists(&part);
        return Err(format!("ダウンロードを書き込めません: {error}"));
    }
    drop(file);
    emit_progress(app, written, asset.size);

    if let Err(message) = verify_download(&part, asset.size, asset.digest.as_deref()) {
        remove_if_exists(&part);
        return Err(message);
    }
    if cancel.load(Ordering::SeqCst) {
        remove_if_exists(&part);
        return Err("ダウンロードを中止しました。".to_string());
    }
    fs::rename(&part, &dest).map_err(|error| {
        remove_if_exists(&part);
        format!("更新ファイルを配置できません: {error}")
    })?;
    Ok(dest)
}

/// 更新インストーラーをダウンロードして保持する。成功/失敗はイベントで流す。
pub async fn download(
    app: &AppHandle,
    state: &UpdateState,
    downloads: &DownloadManager,
) -> Result<(), String> {
    let offer = {
        let guard = state.lock_offer();
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "先に更新を確認してください。".to_string())?
    };
    let cancel = downloads.begin(DOWNLOAD_ID)?;
    *state.lock_downloaded() = None;
    let result = download_asset(app, &offer, &cancel).await;
    downloads.finish(DOWNLOAD_ID);
    match result {
        Ok(path) => {
            *state.lock_downloaded() = Some(DownloadedUpdate {
                path: path.clone(),
                version: offer.version.clone(),
            });
            emit_done(app, &path.to_string_lossy(), &offer.version);
            Ok(())
        }
        Err(message) => {
            let message = if cancel.load(Ordering::SeqCst) && message.contains("中止") {
                "ダウンロードを中止しました。".to_string()
            } else {
                message
            };
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

/// ダウンロード済みのインストーラーを切り離して起動し、少し待ってアプリを終了する。
pub fn install(app: &AppHandle, state: &UpdateState) -> Result<(), String> {
    let downloaded = {
        let guard = state.lock_downloaded();
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "更新ファイルがありません。先にダウンロードしてください。".to_string())?
    };
    let directory = cache_dir(app)?;
    if downloaded.path.parent() != Some(directory.as_path()) {
        return Err("更新ファイルの保存先が想定と違います。".to_string());
    }
    if !downloaded.path.is_file() {
        return Err("更新ファイルが見つかりません。再ダウンロードしてください。".to_string());
    }

    let mut command = Command::new(&downloaded.path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("インストーラーを起動できません: {error}"))?;
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
            selected.digest.as_deref(),
            Some("28b283711529ba5d2c919ba2277376da95d0df95a8d0b340c4eec35c1c64a161")
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
        assert!(check.error.unwrap_or_default().contains("比較できません"));
    }

    #[test]
    fn asset_selection_rejects_untrusted_urls_and_wrong_names() {
        let assets = vec![
            asset("NuKa_1.0.0_x64-setup.exe", 10, GOOD_URL, None),
            asset("NuKa_1.0.0_x64.msi", 10, GOOD_URL, None),
            asset("notes.txt", 10, GOOD_URL, None),
        ];
        let selected = select_asset(&assets).expect("選べる");
        assert_eq!(selected.name, "NuKa_1.0.0_x64-setup.exe");

        let evil = vec![asset(
            "NuKa_1.0.0_x64-setup.exe",
            10,
            "https://example.com/NuKa_1.0.0_x64-setup.exe",
            None,
        )];
        let error = select_asset(&evil).expect_err("URL が違えば拒否");
        assert!(error.contains("取得先"));

        let too_large = vec![asset(
            "NuKa_1.0.0_x64-setup.exe",
            MAX_UPDATE_BYTES + 1,
            GOOD_URL,
            None,
        )];
        assert!(select_asset(&too_large).is_err());

        assert!(select_asset(&[]).is_err());
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
        let directory = std::env::temp_dir().join(format!("nuka-updater-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("installer.exe");
        fs::write(&path, b"abc").unwrap();

        assert!(verify_download(
            &path,
            3,
            Some("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        )
        .is_ok());
        assert!(verify_download(&path, 3, None).is_ok());
        assert!(verify_download(&path, 4, None).is_err());
        assert!(verify_download(
            &path,
            3,
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000")
        )
        .is_err());
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
