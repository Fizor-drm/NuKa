//! llama-server の子プロセス管理。
//!
//! - 起動はシェルを介さず、引数配列で `llama-server.exe` を直接叩く
//!   (パスに空白があってもクォート崩れが起きない)。
//! - 127.0.0.1:0 で空きポートを取ってから、そのポートで起動する。
//! - `/health` が 200 を返すまで 500ms 間隔で最大 300 秒待つ。
//! - 進み具合は `engine:status` イベントで流す。
//! - 異常終了したら error を流し、`restart_engine` で起こし直せる。
//! - アプリ終了時 (RunEvent) と Drop で必ず kill する。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::{Child, Command};
use tokio::time::sleep;

use crate::message::{text, LocalizedText};
use crate::settings::Settings;
use job_object::JobObject;

pub const SERVER_ALIAS: &str = "plamo-3-translate";
pub const MODEL_FILE: &str = "plamo-3-translate-0.8b-Q4_K_M.gguf";
pub const MODEL_ENV: &str = "PLAMO_MODEL_PATH";
pub const SERVER_ENV: &str = "PLAMO_LLAMA_SERVER";
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(300);
pub const HEALTH_INTERVAL: Duration = Duration::from_millis(500);

const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);
/// 開発時は exe の位置から上位 6 階層までランタイムを探す。
const DEBUG_SEARCH_DEPTH: usize = 6;
/// リリースでは exe の隣と 1 つ上だけ。親ディレクトリに置かれた runtime を拾わない。
const RELEASE_SEARCH_DEPTH: usize = 1;

/// 探索する親階層の上限。リリース配布では上位への planting を避けるため浅くする。
fn max_search_depth() -> usize {
    if cfg!(debug_assertions) {
        DEBUG_SEARCH_DEPTH
    } else {
        RELEASE_SEARCH_DEPTH
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnginePhase {
    Starting,
    Loading,
    Ready,
    Error,
}

/// `engine:status` イベントと `get_engine_status` の返り値。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EngineStatus {
    pub state: EnginePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<LocalizedText>,
}

impl EngineStatus {
    fn new(state: EnginePhase, detail: Option<LocalizedText>) -> Self {
        Self { state, detail }
    }
}

#[derive(Clone)]
pub struct Engine {
    app: AppHandle,
    inner: Arc<Mutex<Inner>>,
    http: reqwest::Client,
}

struct Inner {
    /// start / kill のたびに増える。古い監視タスクはこれで自分を止める。
    generation: u64,
    status: EngineStatus,
    port: Option<u16>,
    child: Option<Child>,
    /// llama-server を道連れにする Job Object (Windows のみ意味を持つ)。
    /// このハンドルを閉じると OS が割り当て済みの子プロセスも終了させる。
    job: Option<JobObject>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // アプリ終了時に子プロセスを残さない。kill_on_drop もあるが、ここでも明示的に止める。
        // job はこのあと field の drop で閉じられ、残っている子はカーネルが終了させる。
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

impl Engine {
    pub fn new(app: AppHandle) -> Self {
        // llama-server はループバックにしか居ない。OS や環境変数のプロキシを読むと
        // 127.0.0.1 宛の /health までプロキシへ中継され、起動待ちが必ずタイムアウトする。
        // ここで失敗するのは設定不備だけなので、プロキシを読む既定クライアントには戻さない。
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("ループバック専用の HTTP クライアントを作れません");
        Self {
            app,
            inner: Arc::new(Mutex::new(Inner {
                generation: 0,
                status: EngineStatus::new(EnginePhase::Starting, None),
                port: None,
                child: None,
                job: None,
            })),
            http,
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.lock().status.clone()
    }

    /// 準備できていれば llama-server の待ち受けポート。
    pub fn port(&self) -> Option<u16> {
        self.lock().port
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// 起動して `/health` の監視を始める。結果は `engine:status` で流れる。
    ///
    /// tokio のプロセスドライバを使うので、ランタイムの中で呼ぶこと
    /// (`tauri::async_runtime::spawn` の中など)。
    pub async fn start(&self, settings: &Settings) {
        self.kill_now();
        let generation = self.next_generation();
        self.set_status(
            generation,
            EngineStatus::new(
                EnginePhase::Starting,
                Some(text(
                    "engine.startingDetail",
                    "llama-server を起動しています。",
                )),
            ),
        );

        let paths = match resolve_paths(settings, &self.app) {
            Ok(paths) => paths,
            Err(detail) => {
                self.set_status(
                    generation,
                    EngineStatus::new(EnginePhase::Error, Some(detail)),
                );
                return;
            }
        };

        let port = match pick_free_port() {
            Ok(port) => port,
            Err(error) => {
                self.set_status(
                    generation,
                    EngineStatus::new(
                        EnginePhase::Error,
                        Some(
                            text(
                                "engine.portUnavailable",
                                format!("空きポートを確保できません: {error}"),
                            )
                            .param("detail", error),
                        ),
                    ),
                );
                return;
            }
        };

        let log_path = self.log_path();
        let child = match spawn_server(&paths, port, settings, log_path.as_deref()) {
            Ok(child) => child,
            Err(detail) => {
                self.set_status(
                    generation,
                    EngineStatus::new(EnginePhase::Error, Some(detail)),
                );
                return;
            }
        };
        // 起動直後に Job Object へ割り当てておく。以後はアプリがどう落ちても
        // カーネルが llama-server を道連れにする。
        let job = attach_job_object(&child);

        {
            let mut inner = self.lock();
            if inner.generation != generation {
                // 起動の途中で別の start / kill が入った。起動した子は捨てる
                // (kill_on_drop が有効なので drop で止まる)。
                drop(inner);
                drop(child);
                drop(job);
                return;
            }
            inner.child = Some(child);
            inner.job = job;
            inner.port = Some(port);
        }

        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            engine.supervise(generation, port).await;
        });
    }

    /// 子プロセスを止める。状態はそのまま (次の start が starting にする)。
    pub fn kill_now(&self) {
        let (child, job) = {
            let mut inner = self.lock();
            inner.generation = inner.generation.wrapping_add(1);
            inner.port = None;
            (inner.child.take(), inner.job.take())
        };
        if let Some(mut child) = child {
            if let Err(error) = child.start_kill() {
                eprintln!("llama-server を止められません: {error}");
            }
            let _ = child.try_wait();
        }
        // Job Object を閉じると、割り当て済みの子は OS が終了させる (二重の保険)。
        drop(job);
    }

    async fn supervise(self, generation: u64, port: u16) {
        let progress_engine = self.clone();
        let fatal_engine = self.clone();
        let outcome = wait_until_ready(
            &self.http,
            port,
            HEALTH_TIMEOUT,
            HEALTH_INTERVAL,
            move |phase| {
                let detail = match phase {
                    EnginePhase::Loading => {
                        Some(text("engine.loadingDetail", "モデルを読み込んでいます。"))
                    }
                    EnginePhase::Ready => Some(
                        text(
                            "engine.readyDetail",
                            format!("127.0.0.1:{port} で待ち受けています。"),
                        )
                        .param("port", port),
                    ),
                    _ => None,
                };
                progress_engine.set_status(generation, EngineStatus::new(phase, detail));
            },
            move || {
                if !fatal_engine.is_current(generation) {
                    // 新しい start / kill に置き換えられた。静かに抜ける。
                    return Some(text(
                        "engine.superseded",
                        "エンジンの起動が置き換えられました。",
                    ));
                }
                fatal_engine.exited_detail(generation)
            },
        )
        .await;

        match outcome {
            ReadyOutcome::Ready => self.watchdog(generation).await,
            ReadyOutcome::TimedOut => {
                // 置き換えられていたら、新しい子プロセスに手を出さない。
                if !self.is_current(generation) {
                    return;
                }
                self.set_status(
                    generation,
                    EngineStatus::new(
                        EnginePhase::Error,
                        Some(
                            text(
                                "engine.startTimeout",
                                format!(
                                    "llama-server が {} 秒以内に準備できませんでした。",
                                    HEALTH_TIMEOUT.as_secs()
                                ),
                            )
                            .param("seconds", HEALTH_TIMEOUT.as_secs()),
                        ),
                    ),
                );
                self.kill_now();
            }
            ReadyOutcome::Failed(detail) => {
                if !self.is_current(generation) {
                    return;
                }
                self.set_status(
                    generation,
                    EngineStatus::new(EnginePhase::Error, Some(detail)),
                );
                self.kill_now();
            }
        }
    }

    /// 準備完了後も、勝手に落ちていないかを見張る。
    async fn watchdog(self, generation: u64) {
        loop {
            sleep(WATCHDOG_INTERVAL).await;
            if self.lock().generation != generation {
                return;
            }
            if let Some(detail) = self.exited_detail(generation) {
                self.set_status(
                    generation,
                    EngineStatus::new(EnginePhase::Error, Some(detail)),
                );
                return;
            }
        }
    }

    fn exited_detail(&self, generation: u64) -> Option<LocalizedText> {
        let mut inner = self.lock();
        if inner.generation != generation {
            return None;
        }
        let child = inner.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => text(
                    "engine.exitedWithCode",
                    format!("llama-server が終了しました (終了コード {code})。"),
                )
                .param("code", code),
                None => text("engine.exited", "llama-server が終了しました。"),
            }),
            Ok(None) => None,
            Err(error) => Some(
                text(
                    "engine.statusUnknown",
                    format!("llama-server の状態を確認できません: {error}"),
                )
                .param("detail", error),
            ),
        }
    }

    fn next_generation(&self) -> u64 {
        let mut inner = self.lock();
        inner.generation = inner.generation.wrapping_add(1);
        inner.generation
    }

    fn is_current(&self, generation: u64) -> bool {
        self.lock().generation == generation
    }

    fn set_status(&self, generation: u64, status: EngineStatus) {
        let payload = {
            let mut inner = self.lock();
            if inner.generation != generation || inner.status == status {
                return;
            }
            inner.status = status;
            inner.status.clone()
        };
        if let Err(error) = self.app.emit("engine:status", payload) {
            eprintln!("engine:status を送れません: {error}");
        }
    }

    fn log_path(&self) -> Option<PathBuf> {
        let directory = self.app.path().app_log_dir().ok()?;
        std::fs::create_dir_all(&directory).ok()?;
        Some(directory.join("llama-server.log"))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerPaths {
    pub model: PathBuf,
    pub server: PathBuf,
}

fn model_relative() -> PathBuf {
    PathBuf::from("runtime").join("models").join(MODEL_FILE)
}

fn server_relative() -> PathBuf {
    PathBuf::from("runtime")
        .join("llama")
        .join("llama-server.exe")
}

pub(crate) fn exe_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 設定画面から導入する既定のランタイム (llama-server.exe)。
pub(crate) fn default_runtime_server_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|dir| dir.join("runtime").join("llama").join("llama-server.exe"))
}

/// モデルの場所を決める。管理対象 (active_model) → settings.json の model_path →
/// 環境変数 → exe からの探索。
pub(crate) fn resolve_model_path(
    settings: &Settings,
    search_root: &Path,
) -> Result<PathBuf, LocalizedText> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(active) = settings.active_model.as_deref() {
        match settings
            .installed_models
            .iter()
            .find(|model| model.file == active)
        {
            Some(model) => {
                let path = PathBuf::from(&model.path);
                if path.is_file() {
                    return Ok(path);
                }
                tried.push(format!("管理対象のモデル {}", path.display()));
            }
            None => tried.push(format!("管理対象のモデル {active} (未登録)")),
        }
    }

    if let Some(value) = settings.model_path.as_deref() {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(format!("settings.json の model_path {}", path.display()));
    }

    if let Ok(value) = std::env::var(MODEL_ENV) {
        let path = PathBuf::from(&value);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(format!("{MODEL_ENV} の値 {}", path.display()));
    }

    let depth = max_search_depth();
    let relative = model_relative();
    if let Some(found) = find_in_ancestors(search_root, &relative, depth) {
        return Ok(found);
    }
    tried.push(format!(
        "{} から上位 {depth} 階層の {}",
        search_root.display(),
        relative.display()
    ));

    Err(text(
        "engine.modelNotInstalled",
        format!(
            "モデルが未導入です。設定でインストールしてください。確認した場所: {}",
            tried.join(" / ")
        ),
    )
    .param("paths", tried.join(" / ")))
}

/// ランタイムの場所を決める。管理済み → settings.json の server_path → 既定の場所 →
/// 環境変数 → exe からの探索。
pub(crate) fn resolve_runtime_path(
    settings: &Settings,
    default_server: Option<&Path>,
    search_root: &Path,
) -> Result<PathBuf, LocalizedText> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(runtime) = settings.installed_runtime.as_ref() {
        let path = PathBuf::from(&runtime.server_path);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(format!("管理中のランタイム {}", path.display()));
    }

    if let Some(value) = settings.server_path.as_deref() {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(format!("settings.json の server_path {}", path.display()));
    }

    if let Some(path) = default_server {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        tried.push(format!("既定のランタイム {}", path.display()));
    }

    if let Ok(value) = std::env::var(SERVER_ENV) {
        let path = PathBuf::from(&value);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(format!("{SERVER_ENV} の値 {}", path.display()));
    }

    let depth = max_search_depth();
    let relative = server_relative();
    if let Some(found) = find_in_ancestors(search_root, &relative, depth) {
        return Ok(found);
    }
    tried.push(format!(
        "{} から上位 {depth} 階層の {}",
        search_root.display(),
        relative.display()
    ));

    Err(text(
        "engine.runtimeNotInstalled",
        format!(
            "ランタイムが未導入。設定でインストールしてください。確認した場所: {}",
            tried.join(" / ")
        ),
    )
    .param("paths", tried.join(" / ")))
}

/// モデルとサーバの場所を決める。
///
/// モデルは管理対象 → settings.json の model_path → 環境変数 → exe の位置からの探索。
/// ランタイムは管理済み → settings.json の server_path → 既定の保存先 → 環境変数 → 探索。
/// 探索はデバッグ実行では上位 6 階層まで、リリースでは隣と 1 つ上だけ。
fn resolve_paths(settings: &Settings, app: &AppHandle) -> Result<ServerPaths, LocalizedText> {
    let search_root = exe_directory();
    let default_server = default_runtime_server_path(app);

    let model = resolve_model_path(settings, &search_root);
    let server = resolve_runtime_path(settings, default_server.as_deref(), &search_root);

    match (model, server) {
        (Ok(model), Ok(server)) => Ok(ServerPaths { model, server }),
        (Err(model_error), Err(_)) => Err(text(
            "engine.bothNotInstalled",
            format!(
                "ランタイムとモデルが未導入です。設定でインストールしてください。({})",
                model_error.fallback
            ),
        )),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// `start` から親を最大 `depth` 階層たどり、`relative` が実在する場所を返す。
pub(crate) fn find_in_ancestors(start: &Path, relative: &Path, depth: usize) -> Option<PathBuf> {
    let mut current = Some(start);
    for _ in 0..=depth {
        let directory = current?;
        let candidate = directory.join(relative);
        if candidate.is_file() {
            return Some(candidate);
        }
        current = directory.parent();
    }
    None
}

/// OS に空きポートを選ばせてから解放する。
/// 取得と起動のあいだに他のプロセスが使う可能性はゼロではないが、
/// 固定ポートを奪い合うよりは衝突が少ない。
pub(crate) fn pick_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn spawn_server(
    paths: &ServerPaths,
    port: u16,
    settings: &Settings,
    log_path: Option<&Path>,
) -> Result<Child, LocalizedText> {
    let (stdout, stderr) = prepare_logs(log_path, port, paths)?;
    let mut command = Command::new(&paths.server);
    command
        .arg("-m")
        .arg(&paths.model)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("-c")
        .arg(settings.context_tokens.to_string())
        .arg("-ngl")
        .arg(settings.gpu_layers.to_string())
        .arg("--alias")
        .arg(SERVER_ALIAS)
        .arg("--no-webui")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true);
    if let Some(directory) = paths.server.parent() {
        command.current_dir(directory);
    }
    #[cfg(windows)]
    {
        // コンソールウィンドウを出さない。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map_err(|error| {
        text(
            "engine.spawnFailed",
            format!(
                "llama-server を起動できません ({}): {error}",
                paths.server.display()
            ),
        )
        .param("detail", error)
    })
}

fn prepare_logs(
    log_path: Option<&Path>,
    port: u16,
    paths: &ServerPaths,
) -> Result<(Stdio, Stdio), LocalizedText> {
    let Some(log_path) = log_path else {
        return Ok((Stdio::null(), Stdio::null()));
    };
    let mut file: File = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| {
            text(
                "engine.logOpenFailed",
                format!("ログファイルを開けません ({}): {error}", log_path.display()),
            )
            .param("detail", error)
        })?;
    let _ = writeln!(
        file,
        "=== llama-server 起動: port {port} / {} / {} ===",
        paths.server.display(),
        paths.model.display()
    );
    let stderr_file = file.try_clone().map_err(|error| {
        text(
            "engine.logCloneFailed",
            format!("ログファイルを複製できません: {error}"),
        )
        .param("detail", error)
    })?;
    Ok((Stdio::from(file), Stdio::from(stderr_file)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadyOutcome {
    Ready,
    TimedOut,
    /// プロセスが落ちたなど、待つまでもない失敗。
    Failed(LocalizedText),
}

/// `/health` を `interval` 間隔で叩き、200 が返るまで待つ。
///
/// `fatal` が毎回の試行前に呼ばれ、`Some` を返したら即座に諦める
/// (プロセスの異常終了を拾うため)。
pub(crate) async fn wait_until_ready<F, G>(
    client: &reqwest::Client,
    port: u16,
    budget: Duration,
    interval: Duration,
    mut on_progress: F,
    mut fatal: G,
) -> ReadyOutcome
where
    F: FnMut(EnginePhase),
    G: FnMut() -> Option<LocalizedText>,
{
    let deadline = Instant::now() + budget;
    let url = format!("http://127.0.0.1:{port}/health");
    loop {
        if let Some(detail) = fatal() {
            return ReadyOutcome::Failed(detail);
        }
        if let Ok(response) = client
            .get(&url)
            .timeout(HEALTH_REQUEST_TIMEOUT)
            .send()
            .await
        {
            if response.status().is_success() {
                on_progress(EnginePhase::Ready);
                return ReadyOutcome::Ready;
            }
            // モデル読み込み中は 503 が返る。
            on_progress(EnginePhase::Loading);
        }
        // 接続拒否やタイムアウトは「まだ待ち受けていない」。同じ扱いで次の試行へ進む。
        if Instant::now() >= deadline {
            return ReadyOutcome::TimedOut;
        }
        sleep(interval).await;
    }
}

/// llama-server を Job Object に割り当てる。
///
/// 失敗しても kill_on_drop / kill_now は残っているので、起動自体は続ける
/// (Job Object が使えない環境でも従来どおり動く)。
fn attach_job_object(child: &Child) -> Option<JobObject> {
    let job = match JobObject::kill_on_close() {
        Ok(job) => job,
        Err(error) => {
            eprintln!("Job Object を作れません (子プロセスの後始末は kill で行います): {error}");
            return None;
        }
    };
    let Some(pid) = child.id() else {
        eprintln!("llama-server の PID を取得できません");
        return None;
    };
    if let Err(error) = job.assign_pid(pid) {
        eprintln!("llama-server (pid {pid}) を Job Object に割り当てられません: {error}");
        return None;
    }
    Some(job)
}

/// Windows の Job Object。アプリがクラッシュしたり `std::process::exit` で終わっても、
/// ハンドルが閉じた時点でカーネルが割り当て済みの子プロセスを終了させる。
#[cfg(windows)]
mod job_object {
    use std::ffi::c_void;
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    pub(super) struct JobObject {
        handle: HANDLE,
    }

    impl JobObject {
        pub(super) fn kill_on_close() -> io::Result<Self> {
            // SAFETY: 生成したハンドルは全て null チェックし、失敗時は閉じる。
            // 成功したハンドルの所有権は返す JobObject だけが持つ。
            unsafe {
                let handle = CreateJobObjectW(ptr::null(), ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0 {
                    let error = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(error);
                }
                Ok(Self { handle })
            }
        }

        /// pid のプロセスをこの Job Object に割り当てる。
        pub(super) fn assign_pid(&self, pid: u32) -> io::Result<()> {
            // SAFETY: OpenProcess の戻り値は必ず CloseHandle する。
            // AssignProcessToJobObject は失敗時だけ last_os_error を返す。
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let assigned = AssignProcessToJobObject(self.handle, process);
                let error = io::Error::last_os_error();
                CloseHandle(process);
                if assigned == 0 {
                    return Err(error);
                }
            }
            Ok(())
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            // SAFETY: このハンドルは kill_on_close で作ったもので、所有者はここだけ。
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    // SAFETY: Windows の HANDLE はプロセス内で共有できるカーネルハンドルで、
    // CloseHandle はどのスレッドから呼んでもよい。所有権は JobObject が 1 つだけ持つ。
    unsafe impl Send for JobObject {}
}

#[cfg(not(windows))]
mod job_object {
    use std::io;

    /// Windows 以外では何もしない。kill_on_drop / kill_now が後始末を担う。
    pub(super) struct JobObject;

    impl JobObject {
        pub(super) fn kill_on_close() -> io::Result<Self> {
            Ok(Self)
        }

        pub(super) fn assign_pid(&self, _pid: u32) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{InstalledModel, InstalledRuntime};
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir(label: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("plamo3-engine-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("一時ディレクトリを作れる");
        directory
    }

    #[test]
    fn picks_a_port_that_can_be_bound() {
        let port = pick_free_port().expect("空きポートを取れる");
        assert!(port > 0);
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("取ったポートを bind できる");
        drop(listener);
    }

    #[test]
    fn status_matches_the_event_contract() {
        let ready = serde_json::to_value(EngineStatus::new(EnginePhase::Ready, None)).unwrap();
        assert_eq!(ready, serde_json::json!({ "state": "ready" }));

        let loading = serde_json::to_value(EngineStatus::new(
            EnginePhase::Loading,
            Some(text("engine.loadingDetail", "モデルを読み込んでいます。")),
        ))
        .unwrap();
        assert_eq!(loading["state"], "loading");
        assert_eq!(loading["detail"]["key"], "engine.loadingDetail");
        assert_eq!(loading["detail"]["fallback"], "モデルを読み込んでいます。");

        let error = serde_json::to_value(EngineStatus::new(
            EnginePhase::Error,
            Some(text("error.raw", "見つかりません")),
        ))
        .unwrap();
        assert_eq!(error["state"], "error");
    }

    #[test]
    fn finds_runtime_next_to_an_ancestor() {
        let base = temp_dir("ancestors");
        let exe_dir = base
            .join("desktop")
            .join("src-tauri")
            .join("target")
            .join("debug");
        fs::create_dir_all(&exe_dir).unwrap();
        let relative = server_relative();
        let expected = base.join(&relative);
        fs::create_dir_all(expected.parent().unwrap()).unwrap();
        fs::write(&expected, b"stub").unwrap();

        assert_eq!(
            find_in_ancestors(&exe_dir, &relative, 6),
            Some(expected.clone())
        );
        // 4 階層上にあるので、2 階層では届かない
        assert_eq!(find_in_ancestors(&exe_dir, &relative, 2), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn release_search_depth_is_shallower_than_debug() {
        assert_eq!(DEBUG_SEARCH_DEPTH, 6);
        assert_eq!(RELEASE_SEARCH_DEPTH, 1);
        let expected = if cfg!(debug_assertions) {
            DEBUG_SEARCH_DEPTH
        } else {
            RELEASE_SEARCH_DEPTH
        };
        assert_eq!(max_search_depth(), expected);
    }

    #[cfg(windows)]
    #[test]
    fn job_object_can_be_created_with_kill_on_close() {
        // 何も割り当てずに作って閉じるだけなので、プロセスに影響はない。
        let job = JobObject::kill_on_close().expect("Job Object を作れる");
        drop(job);
    }

    #[test]
    fn model_resolution_prefers_managed_active_model() {
        let base = temp_dir("model-managed");
        let managed = base.join("managed.gguf");
        fs::write(&managed, b"m").unwrap();
        let manual = base.join("manual.gguf");
        fs::write(&manual, b"m").unwrap();

        let mut settings = Settings {
            model_path: Some(manual.to_string_lossy().into_owned()),
            active_model: Some("managed.gguf".to_string()),
            installed_models: vec![InstalledModel {
                file: "managed.gguf".to_string(),
                repo: "pfnet/test".to_string(),
                bytes: 1,
                path: managed.to_string_lossy().into_owned(),
                installed_at: 0,
            }],
            ..Default::default()
        };
        assert_eq!(resolve_model_path(&settings, &base).unwrap(), managed);

        // 管理対象のファイルが消えていれば settings.json の model_path に落ちる
        settings.installed_models[0].path =
            base.join("missing.gguf").to_string_lossy().into_owned();
        assert_eq!(resolve_model_path(&settings, &base).unwrap(), manual);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn model_resolution_falls_back_to_search_and_reports_missing() {
        let base = temp_dir("model-search");
        let exe_dir = base.join("bin");
        fs::create_dir_all(&exe_dir).unwrap();
        let relative = model_relative();
        let searched = base.join(&relative);
        fs::create_dir_all(searched.parent().unwrap()).unwrap();
        fs::write(&searched, b"m").unwrap();
        let settings = Settings::default();
        assert_eq!(resolve_model_path(&settings, &exe_dir).unwrap(), searched);

        let empty = temp_dir("model-search-empty");
        let error = resolve_model_path(&settings, &empty).unwrap_err();
        assert!(error.fallback.contains("モデルが未導入"));
        assert!(error.fallback.contains("設定でインストール"));
        let _ = fs::remove_dir_all(&empty);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn runtime_resolution_prefers_managed_then_default() {
        let base = temp_dir("runtime-resolve");
        let managed = base.join("managed-llama-server.exe");
        fs::write(&managed, b"x").unwrap();
        let default = base.join("default-llama-server.exe");
        fs::write(&default, b"x").unwrap();

        let managed_settings = Settings {
            installed_runtime: Some(InstalledRuntime {
                server_path: managed.to_string_lossy().into_owned(),
                version: "b12136".to_string(),
                installed_at: 0,
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_runtime_path(&managed_settings, Some(&default), &base).unwrap(),
            managed
        );

        // 管理対象が無ければ既定の場所
        assert_eq!(
            resolve_runtime_path(&Settings::default(), Some(&default), &base).unwrap(),
            default
        );

        // どちらも無ければ「未導入」の案内を出す
        let empty = temp_dir("runtime-resolve-empty");
        let error =
            resolve_runtime_path(&Settings::default(), Some(&empty.join("none.exe")), &empty)
                .unwrap_err();
        assert!(error.fallback.contains("ランタイムが未導入"));
        assert!(error.fallback.contains("設定でインストール"));
        let _ = fs::remove_dir_all(&empty);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn spawn_reports_a_missing_binary() {
        let directory = temp_dir("missing-binary");
        let paths = ServerPaths {
            model: directory.join("missing.gguf"),
            server: directory.join("missing-llama-server.exe"),
        };
        let error = spawn_server(&paths, 12345, &Settings::default(), None).unwrap_err();
        assert!(error.fallback.contains("llama-server を起動できません"));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn logs_are_appended_to_the_given_file() {
        let directory = temp_dir("logs");
        let log_path = directory.join("llama-server.log");
        let paths = ServerPaths {
            model: PathBuf::from("model.gguf"),
            server: PathBuf::from("llama-server.exe"),
        };
        let (stdout, stderr) = prepare_logs(Some(&log_path), 9999, &paths).unwrap();
        drop(stdout);
        drop(stderr);
        let written = fs::read_to_string(&log_path).unwrap();
        assert!(written.contains("port 9999"));
        let _ = fs::remove_dir_all(&directory);
    }

    /// `/health` の応答を順番に返すだけの小さなサーバ。
    async fn spawn_stub_health(statuses: Vec<u16>) -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("stub を bind できる");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut remaining = statuses;
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let status = if remaining.is_empty() {
                    200
                } else {
                    remaining.remove(0)
                };
                let body = if status == 200 {
                    "{\"status\":\"ok\"}"
                } else {
                    "{\"error\":\"loading model\"}"
                };
                let reason = if status == 200 { "OK" } else { "Loading" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
                if remaining.is_empty() {
                    break;
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn health_polling_reports_loading_then_ready() {
        let port = spawn_stub_health(vec![503, 503, 200]).await;
        let client = reqwest::Client::new();
        let mut phases = Vec::new();
        let outcome = wait_until_ready(
            &client,
            port,
            Duration::from_secs(5),
            Duration::from_millis(20),
            |phase| phases.push(phase),
            || None,
        )
        .await;

        assert_eq!(outcome, ReadyOutcome::Ready);
        assert_eq!(phases.first(), Some(&EnginePhase::Loading));
        assert_eq!(phases.last(), Some(&EnginePhase::Ready));
    }

    #[tokio::test]
    async fn health_polling_times_out_when_nothing_listens() {
        let port = pick_free_port().unwrap();
        let client = reqwest::Client::new();
        let mut phases = Vec::new();
        let outcome = wait_until_ready(
            &client,
            port,
            Duration::from_millis(300),
            Duration::from_millis(20),
            |phase| phases.push(phase),
            || None,
        )
        .await;

        assert_eq!(outcome, ReadyOutcome::TimedOut);
        assert!(phases.is_empty());
    }

    #[tokio::test]
    async fn a_fatal_probe_stops_the_wait_immediately() {
        let port = pick_free_port().unwrap();
        let client = reqwest::Client::new();
        let outcome = wait_until_ready(
            &client,
            port,
            Duration::from_secs(5),
            Duration::from_millis(20),
            |_| {},
            || {
                Some(text(
                    "engine.exitedWithCode",
                    "llama-server が終了しました (終了コード 1)。",
                ))
            },
        )
        .await;

        assert_eq!(
            outcome,
            ReadyOutcome::Failed(text(
                "engine.exitedWithCode",
                "llama-server が終了しました (終了コード 1)。"
            ))
        );
    }
}
