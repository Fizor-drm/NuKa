//! llama-server の子プロセス管理。
//!
//! - 起動はシェルを介さず、引数配列で `llama-server.exe` を直接叩く
//!   (パスに空白があってもクォート崩れが起きない)。
//! - 127.0.0.1:0 で空きポートを取ってから、そのポートで起動する。
//! - `/health` が 200 を返すまで 500ms 間隔で最大 300 秒待つ。
//! - 進み具合は `engine:status` イベントで流す (ペイロードに `role` を含める)。
//! - 異常終了したら error を流し、`restart_engine` で起こし直せる。
//! - アプリ終了時 (RunEvent) と Drop で必ず kill する。
//!
//! # 2 インスタンス
//!
//! メイン画面用とフローティング用で `Engine` を 2 つ持つ。役割は [`EngineRole`]
//! で区別し、使うモデル (メイン = `active_model` / フローティング = 専用モデル) と
//! 起動・停止のタイミングが異なる。ポートはどちらも動的割当なので共存できる。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// 翻訳が「準備完了」を待つ上限。
///
/// エンジン自身の起動待ち (`HEALTH_TIMEOUT`) と揃えて 1 か所で持つ。ここを短く
/// 切ると、まだ正常にモデルを読み込んでいる最中に翻訳だけが諦めてしまう。
/// (監査 M-2: UI 側にも別の上限を置かず、待つのはこの 1 か所だけにする)
pub const ENGINE_READY_LIMIT: Duration = HEALTH_TIMEOUT;

const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);
/// 準備完了を待つあいだ状態を見直す間隔。
const ENGINE_WAIT_POLL: Duration = Duration::from_millis(200);
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

/// エンジンの役割。メイン画面用とフローティング用の 2 インスタンスを区別する。
///
/// どちらも同じ [`Engine`] 実装で、使うモデルとライフサイクルだけが違う。
///
/// - `Main`: 選択中のモデル (`active_model`)。オンデマンドで起動し、メイン画面を
///   閉じたら止める (大型モデルの VRAM を解放する)。
/// - `Floating`: フローティング専用モデル。`background.resident_engine` が ON なら
///   アプリ起動中ずっと起動しておく (小型モデル前提)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineRole {
    Main,
    Floating,
}

impl EngineRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Floating => "floating",
        }
    }

    /// WebView のラベルから役割を決める。`floating` 以外はメイン扱い。
    pub fn from_label(label: &str) -> Self {
        if label == crate::floating::FLOATING_LABEL {
            Self::Floating
        } else {
            Self::Main
        }
    }

    /// IPC の `role` 引数から役割を決める。未指定・未知はメイン扱い。
    pub fn from_arg(role: Option<&str>) -> Self {
        match role {
            Some(value) if value == Self::Floating.as_str() => Self::Floating,
            _ => Self::Main,
        }
    }

    /// この役割が使う専用モデル (installed_models の file)。メインは常に `None`。
    pub(crate) fn model_override(self, settings: &Settings) -> Option<&str> {
        match self {
            Self::Main => None,
            Self::Floating => settings.floating_window.model.as_deref(),
        }
    }
}

/// `engine:status` イベントと `get_engine_status` の返り値。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EngineStatus {
    /// どちらのエンジンの状態か。UI は自ウィンドウの役割の status だけを見る。
    pub role: EngineRole,
    pub state: EnginePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<LocalizedText>,
}

impl EngineStatus {
    fn new(role: EngineRole, state: EnginePhase, detail: Option<LocalizedText>) -> Self {
        Self {
            role,
            state,
            detail,
        }
    }
}

/// 準備完了を待った結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineReady {
    /// 準備完了。待ち受けポート。
    Ready(u16),
    /// 待機中に中止された (`cancel` が立った)。翻訳は始めない。
    Cancelled,
    /// 待っても準備できなかった (起動失敗・途中で停止・待機打ち切り)。
    Unavailable,
}

#[derive(Clone)]
pub struct Engine {
    app: AppHandle,
    /// このインスタンスの役割。status の `role` と起動ログのファイル名に使う。
    role: EngineRole,
    inner: Arc<Mutex<Inner>>,
    http: reqwest::Client,
}

struct Inner {
    /// start / kill のたびに増える。古い監視タスクはこれで自分を止める。
    generation: u64,
    /// 進行中の `start` の世代。`None` なら起動処理は走っていない。
    ///
    /// 停止 (`kill_now`) と「起動処理中」を区別するために持つ。`kill_now` は
    /// status を書き換えないため、停止直後に `starting` / `loading` が残る。
    /// phase だけで起動要否を判断すると再起動できなくなる (監査 CR-1)。
    active_start: Option<u64>,
    status: EngineStatus,
    port: Option<u16>,
    child: Option<Child>,
    /// 稼働中の子が読み込んだモデル。停止中・起動処理中は `None`。
    ///
    /// 設定のモデルが変わったときに「いまの子を起こし直すべきか」を比べるために持つ。
    model: Option<PathBuf>,
    /// 予約済みの起動が読み込む予定のモデル。起動処理中だけ `Some`。
    ///
    /// 起動完了を待たずに「この起動はどのモデルか」を判定できるようにする。
    reserved_model: Option<PathBuf>,
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
    pub fn new(app: AppHandle, role: EngineRole) -> Self {
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
            role,
            inner: Arc::new(Mutex::new(Inner {
                generation: 0,
                active_start: None,
                status: EngineStatus::new(role, EnginePhase::Starting, None),
                port: None,
                child: None,
                model: None,
                reserved_model: None,
                job: None,
            })),
            http,
        }
    }

    /// このインスタンスの役割。
    pub fn role(&self) -> EngineRole {
        self.role
    }

    pub fn status(&self) -> EngineStatus {
        self.lock().status.clone()
    }

    /// 子プロセスを保持しているか (起動処理中・稼働中・異常終了の未回収を含む)。
    ///
    /// 「いま動いているか」ではなく「プロセスを掴んでいるか」なので、モデル変更時の
    /// 再起動要否の判定に使う。
    pub fn has_process(&self) -> bool {
        self.lock().child.is_some()
    }

    /// 起動処理の途中か (`begin_start` から spawn 完了まで)。
    pub fn is_starting(&self) -> bool {
        self.lock().active_start.is_some()
    }

    /// 稼働中の子プロセスが読み込んでいるモデル (停止中・起動処理中は `None`)。
    ///
    /// 設定のモデル変更を反映するとき、いまのエンジンがどのモデルを掴んでいるかを
    /// 比べるために使う。`has_process` が true でも、起動処理中はまだ `None`。
    pub fn running_model(&self) -> Option<PathBuf> {
        self.lock().model.clone()
    }

    /// 起動処理中に読み込む予定のモデル (停止中・稼働中は `None`)。
    ///
    /// 起動完了を待たずに、その起動を続けてよいか (設定のモデルと一致するか) を
    /// 判定するために使う。`running_model` と違い、子の spawn 前でも分かる。
    pub fn reserved_model(&self) -> Option<PathBuf> {
        self.lock().reserved_model.clone()
    }

    /// このインスタンスの status を作る (role の付け忘れを防ぐ)。
    fn status_of(&self, state: EnginePhase, detail: Option<LocalizedText>) -> EngineStatus {
        EngineStatus::new(self.role, state, detail)
    }

    /// 準備できていれば llama-server の待ち受けポート。
    pub fn port(&self) -> Option<u16> {
        self.lock().port
    }

    /// いまの起動・待機の判定に使う状態を 1 回のロックで読む。
    fn state(&self) -> EngineState {
        let inner = self.lock();
        EngineState {
            starting: inner.active_start.is_some(),
            port: inner.port,
            phase: inner.status.state,
        }
    }

    /// 準備完了 (ready かつ待ち受けポートあり) になるまで待つ。
    ///
    /// - 起動失敗 (`error`) と「止められた」を知った時点で諦める
    /// - `limit` を超えたら `Unavailable`
    /// - `cancel` が立ったら `Cancelled` (待機中も中止できる)
    ///
    /// ここでは**起動しない**。起動の予約はジョブ受け付け時に制御ロック下で
    /// 確定済みで、取消済みジョブが制御ロック外で新しい起動を奪わないようにする
    /// (監査 Medium 1)。停止/異常終了からの復旧は commands 側で job id・取消・
    /// 希望モデルを再検証してから `reserve_start` する。
    pub async fn wait_until_ready(&self, limit: Duration, cancel: &AtomicBool) -> EngineReady {
        let deadline = Instant::now() + limit;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return EngineReady::Cancelled;
            }
            match self.state().wait_step() {
                EngineWait::Ready(port) => return EngineReady::Ready(port),
                EngineWait::Failed => return EngineReady::Unavailable,
                EngineWait::Pending => {}
            }
            if Instant::now() >= deadline {
                return EngineReady::Unavailable;
            }
            sleep(ENGINE_WAIT_POLL).await;
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// 起動を同期に予約する (既存の子の停止・モデル解決までを行う)。
    ///
    /// 戻り値は [spawn_reserved] に渡す予約。実際の子プロセスの spawn は
    /// tokio ランタイムの中で行う必要があるため、呼び出し側はここで「どの起動を
    /// するか」を確定してから非同期に spawn する。ジョブ受け付けと起動の停止が
    /// 競合しないよう、呼び出し側の制御ロック下で使うこと。
    ///
    /// モデル・ランタイムの解決もここで行う。解決できない場合は `fail_start` で
    /// エラー状態にして `None` を返す (無駄な spawn タスクを残さない)。
    pub(crate) fn reserve_start(&self, settings: &Settings, force: bool) -> Option<ReservedStart> {
        let generation = self.begin_start(force)?;
        let paths = match resolve_paths(settings, &self.app, self.role) {
            Ok(paths) => paths,
            Err(detail) => {
                self.fail_start(generation, detail);
                return None;
            }
        };
        {
            let mut inner = self.lock();
            if inner.active_start == Some(generation) {
                inner.reserved_model = Some(paths.model.clone());
            }
        }
        Some(ReservedStart { generation, paths })
    }

    /// 予約済みの起動を実行する。tokio ランタイムの中で呼ぶこと。
    ///
    /// 予約後に別の起動・停止が入っていたら、古い予約は静かに捨てる。
    pub(crate) async fn spawn_reserved(&self, reserved: ReservedStart, settings: &Settings) {
        self.spawn_engine(reserved.generation, reserved.paths, settings)
            .await;
    }

    /// 起動の開始を宣言する。戻り値はこの起動の世代。
    ///
    /// `force` が false のとき、すでに稼働中か別の起動処理が走っていれば `None` を
    /// 返して二重起動しない。`force` なら既存の子を止めてでも開始する。
    ///
    /// 判定は `EngineState::needs_start` に任せる (phase が `error` なら、ポートが
    /// 残っていても子は死んでいるので置き換える: 監査 CR2-2)。
    fn begin_start(&self, force: bool) -> Option<u64> {
        let (child, job, generation) = {
            let mut inner = self.lock();
            if !force {
                let state = EngineState {
                    starting: inner.active_start.is_some(),
                    port: inner.port,
                    phase: inner.status.state,
                };
                if !state.needs_start() {
                    return None;
                }
            }
            let child = inner.child.take();
            let job = inner.job.take();
            inner.model = None;
            inner.reserved_model = None;
            inner.generation = inner.generation.wrapping_add(1);
            let generation = inner.generation;
            // 判定と同時に「起動処理中」を立てる。ここを分けると、並行した
            // 予約 (`reserve_start`) が二重に llama-server を起こす。
            inner.active_start = Some(generation);
            inner.port = None;
            (child, job, generation)
        };
        // 古い子の停止はロックの外で行う (待たせない)。
        stop_child(child, job);
        self.set_status(
            generation,
            self.status_of(
                EnginePhase::Starting,
                Some(text(
                    "engine.startingDetail",
                    "llama-server を起動しています。",
                )),
            ),
        );
        Some(generation)
    }

    /// 起動本体。呼ぶ前に `reserve_start` で世代と解決済みパスを取っておくこと。
    async fn spawn_engine(&self, generation: u64, paths: ServerPaths, settings: &Settings) {
        // 予約後に別の起動・停止が入っていたら、プロセスを起こさずに捨てる。
        if !self.is_current(generation) {
            return;
        }

        let port = match pick_free_port() {
            Ok(port) => port,
            Err(error) => {
                self.fail_start(
                    generation,
                    text(
                        "engine.portUnavailable",
                        format!("空きポートを確保できません: {error}"),
                    )
                    .param("detail", error),
                );
                return;
            }
        };

        let log_path = self.log_path();
        let child = match spawn_server(&paths, port, settings, log_path.as_deref()) {
            Ok(child) => child,
            Err(detail) => {
                self.fail_start(generation, detail);
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
            inner.model = Some(paths.model);
            inner.reserved_model = None;
            // 起動処理はここで完了。以後の待機は supervise が見る。
            inner.active_start = None;
        }

        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            engine.supervise(generation, port).await;
        });
    }

    /// 起動に失敗したことを記録し、「起動処理中」を解除する。
    /// 解除しないと、次に使うときに再起動できなくなる。
    fn fail_start(&self, generation: u64, detail: LocalizedText) {
        self.set_status(generation, self.status_of(EnginePhase::Error, Some(detail)));
        let mut inner = self.lock();
        if inner.active_start == Some(generation) {
            inner.active_start = None;
            inner.reserved_model = None;
        }
    }

    /// 子プロセスを止める。状態はそのまま (次の start が starting にする)。
    ///
    /// 進行中の起動処理があっても「止められた」として解除する。ここを消さないと、
    /// `starting` / `loading` のまま残って再起動できなくなる (監査 CR-1)。
    pub fn kill_now(&self) {
        let (child, job) = self.take_child();
        stop_child(child, job);
    }

    /// 子プロセスを止め、終了 (ファイルロックの解放) を最大 `limit` 待つ。
    ///
    /// モデル削除のように、プロセスが掴んでいるファイルを触る前の後始末に使う。
    /// UI を長時間止めないよう、待ち時間は呼び出し側が短く決める。
    pub fn kill_and_wait(&self, limit: Duration) {
        let (child, job) = self.take_child();
        if let Some(mut child) = child {
            if let Err(error) = child.start_kill() {
                eprintln!("llama-server を止められません: {error}");
            }
            let deadline = Instant::now() + limit;
            loop {
                match child.try_wait() {
                    // 終了した (または状態不明で待ちようがない)。ここで抜ける。
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => {}
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // Job Object を閉じると、まだ残っている子は OS が終了させる。
        drop(job);
    }

    /// 子プロセスと Job Object を取り出し、「停止済み」の状態に戻す。
    fn take_child(&self) -> (Option<Child>, Option<JobObject>) {
        let mut inner = self.lock();
        inner.generation = inner.generation.wrapping_add(1);
        inner.active_start = None;
        inner.port = None;
        inner.model = None;
        inner.reserved_model = None;
        (inner.child.take(), inner.job.take())
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
                progress_engine.set_status(generation, progress_engine.status_of(phase, detail));
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
                    self.status_of(
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
                self.set_status(generation, self.status_of(EnginePhase::Error, Some(detail)));
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
                self.set_status(generation, self.status_of(EnginePhase::Error, Some(detail)));
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
        // 2 インスタンスが同じファイルへ追記すると原因調査ができないので分ける。
        let name = match self.role {
            EngineRole::Main => "llama-server.log",
            EngineRole::Floating => "llama-server-floating.log",
        };
        Some(directory.join(name))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 準備完了を待つあいだの判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineWait {
    /// 準備完了。待ち受けポートが返る。
    Ready(u16),
    /// もう来ない (起動失敗・停止された)。待ち続けない。
    Failed,
    /// まだ待つ (起動処理中・読み込み中)。
    Pending,
}

/// 起動・待機の判定に使う状態。
///
/// `Engine` から切り離した純粋な値にして、停止直後の `starting` / `loading` から
/// 復帰できることをテストできるようにしている (監査 CR-1)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EngineState {
    /// 進行中の `start` があるか (`kill_now` で false に戻る)。
    pub starting: bool,
    /// llama-server の待ち受けポート。
    pub port: Option<u16>,
    /// 直近の phase。
    pub phase: EnginePhase,
}

impl EngineState {
    /// 起動が必要か。
    ///
    /// phase ではなく「進行中の起動」とポートで判定する。`kill_now` は phase を
    /// 書き換えないため、停止直後に `starting` / `loading` が残っていても
    /// 起動し直せるようにする。
    ///
    /// `error` は特別扱いする。watchdog がクラッシュを検知したときは status を
    /// `error` にするだけでポートを残すため、ポートの有無だけでは「もう動いていない」
    /// ことを表せない (監査 CR2-2)。`starting` が立っている間は二重起動しない。
    pub(crate) fn needs_start(self) -> bool {
        !self.starting && (self.port.is_none() || self.phase == EnginePhase::Error)
    }

    /// 待機ループの次の一手。
    pub(crate) fn wait_step(self) -> EngineWait {
        if self.phase == EnginePhase::Ready {
            if let Some(port) = self.port {
                return EngineWait::Ready(port);
            }
        }
        if self.phase == EnginePhase::Error {
            return EngineWait::Failed;
        }
        // 起動処理も走っておらずポートも無い = 止められた。待っても来ないので
        // 打ち切る (待機上限まで無駄に待たない)。
        if !self.starting && self.port.is_none() {
            return EngineWait::Failed;
        }
        EngineWait::Pending
    }
}

/// 子プロセスを止めて Job Object を閉じる。二重に呼んでも安全。
fn stop_child(child: Option<Child>, job: Option<JobObject>) {
    if let Some(mut child) = child {
        if let Err(error) = child.start_kill() {
            eprintln!("llama-server を止められません: {error}");
        }
        let _ = child.try_wait();
    }
    // Job Object を閉じると、割り当て済みの子は OS が終了させる (二重の保険)。
    drop(job);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerPaths {
    pub model: PathBuf,
    pub server: PathBuf,
}

/// 予約済みの起動。`Engine::reserve_start` が作り、`spawn_reserved` が実行する。
///
/// 予約の時点で世代とモデル・ランタイムのパスを固定する。予約後に設定が変わって
/// 新しい起動・停止が入った場合、この予約は世代不一致で捨てられる。
#[derive(Debug)]
pub(crate) struct ReservedStart {
    generation: u64,
    paths: ServerPaths,
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
    resolve_model_path_for(settings, search_root, None)
}

/// モデルの場所を決める。`preferred` (フローティング専用モデル) があれば最優先し、
/// 実在しない・未登録なら通常の解決チェーンへフォールバックする。
///
/// 専用モデルがアンインストールされても翻訳を止めないための設計。フォールバックした
/// ことは `tried` に残し、起動失敗時のメッセージから分かるようにする。
pub(crate) fn resolve_model_path_for(
    settings: &Settings,
    search_root: &Path,
    preferred: Option<&str>,
) -> Result<PathBuf, LocalizedText> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(file) = preferred {
        match settings
            .installed_models
            .iter()
            .find(|model| model.file == file)
        {
            Some(model) => {
                let path = PathBuf::from(&model.path);
                if path.is_file() {
                    return Ok(path);
                }
                tried.push(format!(
                    "フローティング用モデル {} (実ファイルなし)",
                    path.display()
                ));
            }
            None => tried.push(format!("フローティング用モデル {file} (未登録)")),
        }
    }

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
/// モデルは「役割の専用モデル (フローティングのみ)」→ 管理対象 → settings.json の
/// model_path → 環境変数 → exe の位置からの探索。
/// ランタイムは管理済み → settings.json の server_path → 既定の保存先 → 環境変数 → 探索。
/// 探索はデバッグ実行では上位 6 階層まで、リリースでは隣と 1 つ上だけ。
fn both_not_installed(model_error: &LocalizedText, runtime_error: &LocalizedText) -> LocalizedText {
    let paths = |error: &LocalizedText| error.params.get("paths").cloned().unwrap_or_default();
    text(
        "engine.bothNotInstalled",
        "ランタイムとモデルが未導入です。設定からインストールしてください。",
    )
    .param("model_paths", paths(model_error))
    .param("runtime_paths", paths(runtime_error))
}
fn resolve_paths(
    settings: &Settings,
    app: &AppHandle,
    role: EngineRole,
) -> Result<ServerPaths, LocalizedText> {
    let search_root = exe_directory();
    let default_server = default_runtime_server_path(app);

    let model = resolve_model_path_for(settings, &search_root, role.model_override(settings));
    let server = resolve_runtime_path(settings, default_server.as_deref(), &search_root);

    match (model, server) {
        (Ok(model), Ok(server)) => Ok(ServerPaths { model, server }),
        (Err(model_error), Err(runtime_error)) => {
            Err(both_not_installed(&model_error, &runtime_error))
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// その役割のエンジンを起動できるか (モデルとランタイムが解決できるか)。
///
/// 常時起動を始める前に、未導入の環境で無駄に error を出さないための判定。
pub(crate) fn can_resolve(role: EngineRole, settings: &Settings, app: &AppHandle) -> bool {
    resolve_paths(settings, app, role).is_ok()
}

/// その役割が実際に使うモデルのパス (未解決なら `None`)。
///
/// 設定値ではなく**解決結果**を返す。専用モデルが実ファイル欠落などでアクティブ
/// モデルへフォールバックしている場合も、フォールバック先のパスが返る。
pub(crate) fn resolved_model_path(
    role: EngineRole,
    settings: &Settings,
    search_root: &Path,
) -> Option<PathBuf> {
    resolve_model_path_for(settings, search_root, role.model_override(settings)).ok()
}

/// メイン用とフローティング用が同じモデルを解決するか。
///
/// 同じ GGUF を 2 プロセスで VRAM に載せても意味がないため、この場合は 1 つの
/// エンジンを共有する。両方が解決できたときだけ比較し、どちらかが未解決なら
/// 「共有しない」に倒す (どちらも起動できないので役割どおりに任せる)。
pub(crate) fn models_shared(settings: &Settings, search_root: &Path) -> bool {
    let main = resolved_model_path(EngineRole::Main, settings, search_root);
    let floating = resolved_model_path(EngineRole::Floating, settings, search_root);
    match (main, floating) {
        (Some(main), Some(floating)) => main == floating,
        _ => false,
    }
}

/// 要求された役割の翻訳を処理するエンジンの役割。
///
/// 解決モデルがメイン用と同一なら、フローティング翻訳もメイン用エンジンで
/// 処理する (二重ロード防止)。専用モデルが別ファイルならフローティング用。
/// ルーティングの判断はこの関数 1 か所に集約し、呼び出し側で設定を見比べない。
pub(crate) fn serving_role(
    requested: EngineRole,
    settings: &Settings,
    search_root: &Path,
) -> EngineRole {
    match requested {
        EngineRole::Main => EngineRole::Main,
        // 同一モデルならメイン用エンジンへ相乗りする。
        EngineRole::Floating if models_shared(settings, search_root) => EngineRole::Main,
        EngineRole::Floating => EngineRole::Floating,
    }
}

/// 常駐の保持理由。ログと「保持先が変わったか」の説明に使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResidentReason {
    /// フローティング専用モデルが別ファイルで解決できた (2 本目のエンジン)。
    DedicatedModel,
    /// 専用モデルがアクティブモデルと同一ファイル。メイン用 1 本を共有する。
    SharedWithMain,
}

impl ResidentReason {
    /// デバッグログ用の短い識別子。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DedicatedModel => "dedicated-model",
            Self::SharedWithMain => "shared-with-main",
        }
    }
}

/// 常時起動 (常駐) で保持するエンジン。
///
/// 「どの役割のエンジンを、どのモデルで、なぜ保持するか」を 1 つの値にまとめる。
/// 保持先は `Main` (共有モデル) か `Floating` (専用モデル) のどちらか。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResidentTarget {
    /// 保持するエンジンの役割。
    pub role: EngineRole,
    /// そのエンジンが読み込むモデルの実パス (解決済み)。
    pub model: PathBuf,
    /// なぜ保持するか (共有か専用か)。
    pub reason: ResidentReason,
}

/// フローティング専用モデルの実パス。未指定・未登録・実ファイルなしは `None`。
///
/// [`resolve_model_path_for`] は専用モデルが使えないときにアクティブモデルへ
/// フォールバックする。常駐の判定ではそのフォールバックを「専用モデルなし」と
/// みなす必要がある (未導入の専用モデルを選んだまま黙ってメイン用を常駐させない)
/// ため、ここでは代替チェーンへ落とさず、専用モデル自身が解決できたときだけ返す。
fn dedicated_model_path(settings: &Settings) -> Option<PathBuf> {
    let file = settings.floating_window.model.as_deref()?;
    let model = settings
        .installed_models
        .iter()
        .find(|model| model.file == file)?;
    let path = PathBuf::from(&model.path);
    path.is_file().then_some(path)
}

/// 常時起動で保持すべきエンジン。常駐しないなら `None`。
///
/// 判定は次の 3 段だけ:
/// 1. [`Settings::floating_engine_resident`] (常駐の設定そのもの)
/// 2. フローティング専用モデルが実際に解決できるか (安全ガード)
/// 3. メイン用と同じモデルか ([`serving_role`]) — 共有ならメイン用 1 本を保持
///
/// 専用モデルが未 DL・削除済みなどでアクティブモデルへフォールバックしている
/// ときは 2 で `None` を返す。常時起動・設定変更・メイン非表示・フローティング
/// 終了の全経路はこの関数の結果だけを見る。呼び出し側で `models_shared` や
/// 独立起動 bool を再実装しない (二重ロード事故の再発防止)。
pub(crate) fn resident_target(settings: &Settings, search_root: &Path) -> Option<ResidentTarget> {
    if !settings.floating_engine_resident() {
        return None;
    }
    let model = dedicated_model_path(settings)?;
    // ルーティングと同じ判定を使う。同一モデルならメイン用へ相乗りする。
    let role = serving_role(EngineRole::Floating, settings, search_root);
    let reason = match role {
        EngineRole::Main => ResidentReason::SharedWithMain,
        EngineRole::Floating => ResidentReason::DedicatedModel,
    };
    Some(ResidentTarget {
        role,
        model,
        reason,
    })
}

/// フローティング用の独立エンジンを起動・維持すべきか。
///
/// [`resident_target`] がフローティング用を保持先にしているときだけ true。
/// 同一モデルで共有するときはメイン用 1 本を維持するので、2 本目は起動しない
/// (専用モデルにアクティブモデルを選んだ場合も同じ)。
pub(crate) fn should_run_floating_engine(settings: &Settings, search_root: &Path) -> bool {
    resident_target(settings, search_root).is_some_and(|target| target.role == EngineRole::Floating)
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
    fn both_not_installed_carries_path_params() {
        let model = text("engine.modelNotInstalled", "モデルが未導入です。")
            .param("paths", "/models/a.gguf");
        let runtime = text("engine.runtimeNotInstalled", "ランタイムが未導入です。")
            .param("paths", "/runtime/llama-server.exe");
        let combined = both_not_installed(&model, &runtime);
        assert_eq!(combined.key, "engine.bothNotInstalled");
        assert_eq!(
            combined.params.get("model_paths").map(String::as_str),
            Some("/models/a.gguf")
        );
        assert_eq!(
            combined.params.get("runtime_paths").map(String::as_str),
            Some("/runtime/llama-server.exe")
        );
        assert!(!combined.fallback.contains("/models/a.gguf"));
    }
    /// テスト用の状態。
    fn engine_state(starting: bool, port: Option<u16>, phase: EnginePhase) -> EngineState {
        EngineState {
            starting,
            port,
            phase,
        }
    }

    #[test]
    fn engine_is_restarted_only_when_stopped() {
        // 止まっている (起動処理中でもなくポートも無い) なら起動し直す。
        for phase in [
            EnginePhase::Starting,
            EnginePhase::Loading,
            EnginePhase::Ready,
            EnginePhase::Error,
        ] {
            assert!(
                engine_state(false, None, phase).needs_start(),
                "停止中の {phase:?} は起動し直す"
            );
        }

        // 起動処理が走っている間は二重に起こさない (ポートがまだ取れていなくても)。
        for phase in [EnginePhase::Starting, EnginePhase::Loading] {
            assert!(!engine_state(true, None, phase).needs_start());
        }
        // 稼働中も何もしない。
        for phase in [
            EnginePhase::Starting,
            EnginePhase::Loading,
            EnginePhase::Ready,
        ] {
            assert!(
                !engine_state(false, Some(1234), phase).needs_start(),
                "稼働中の {phase:?} では起動しない"
            );
        }
        assert!(!engine_state(true, Some(1234), EnginePhase::Loading).needs_start());
    }

    #[test]
    fn crashed_engine_with_a_stale_port_starts_again() {
        // 監査 CR2-2: watchdog はクラッシュを検知すると status を `error` にするが
        // ポートは残す。ポートの有無だけで判断すると、クラッシュ後にホットキーや
        // トレイから自動再起動できなくなる。
        let crashed = engine_state(false, Some(1234), EnginePhase::Error);
        assert!(crashed.needs_start());
        // 待機側も `error` なら待ち続けない。
        assert_eq!(crashed.wait_step(), EngineWait::Failed);

        // kill でポートが消えている場合も当然再起動する。
        assert!(engine_state(false, None, EnginePhase::Error).needs_start());

        // 起動処理が走っている間は、`error` が残っていても二重起動しない。
        assert!(!engine_state(true, Some(1234), EnginePhase::Error).needs_start());
        assert!(!engine_state(true, None, EnginePhase::Error).needs_start());
    }

    #[test]
    fn killed_engine_in_a_stale_phase_starts_again() {
        // 監査 CR-1 の回帰: `kill_now` は phase を書き換えないため、モデル読み込み中に
        // バックグラウンド化すると `loading` が残る。ここで再起動できないと、
        // ホットキー翻訳が待機上限まで待って失敗し、アプリ再起動まで翻訳できなくなる。
        let stale_loading = engine_state(false, None, EnginePhase::Loading);
        assert!(stale_loading.needs_start());
        let stale_starting = engine_state(false, None, EnginePhase::Starting);
        assert!(stale_starting.needs_start());

        // 待機側も「止められた」と分かった時点で打ち切る (上限まで待たない)。
        assert_eq!(stale_loading.wait_step(), EngineWait::Failed);
        assert_eq!(stale_starting.wait_step(), EngineWait::Failed);
    }

    #[test]
    fn waiting_stops_on_ready_or_error_or_stop() {
        assert_eq!(
            engine_state(true, Some(7777), EnginePhase::Ready).wait_step(),
            EngineWait::Ready(7777)
        );
        assert_eq!(
            engine_state(false, None, EnginePhase::Error).wait_step(),
            EngineWait::Failed
        );
        assert_eq!(
            engine_state(true, Some(7777), EnginePhase::Error).wait_step(),
            EngineWait::Failed
        );
        // 起動処理中・モデル読み込み中は待つ。
        for (starting, port, phase) in [
            (true, None, EnginePhase::Starting),
            (true, Some(7777), EnginePhase::Starting),
            (true, Some(7777), EnginePhase::Loading),
            // 起動処理が続いていれば、まだポートが無くても待つ。
            (true, None, EnginePhase::Loading),
        ] {
            assert_eq!(
                engine_state(starting, port, phase).wait_step(),
                EngineWait::Pending,
                "{phase:?} / {port:?}"
            );
        }
    }

    #[test]
    fn ready_without_a_port_waits_then_times_out() {
        // ready なのにポートが無い = 停止直後だが、起動処理の途中でもありうる。
        // 起動処理が無ければ「止められた」として打ち切る。
        assert_eq!(
            engine_state(false, None, EnginePhase::Ready).wait_step(),
            EngineWait::Failed
        );
        assert_eq!(
            engine_state(true, None, EnginePhase::Ready).wait_step(),
            EngineWait::Pending
        );
    }

    #[test]
    fn status_matches_the_event_contract() {
        let ready = serde_json::to_value(EngineStatus::new(
            EngineRole::Main,
            EnginePhase::Ready,
            None,
        ))
        .unwrap();
        assert_eq!(
            ready,
            serde_json::json!({ "role": "main", "state": "ready" })
        );

        // UI は role で自分のエンジンだけを選ぶ。付け忘れると両ウィンドウが
        // 他人の status を表示してしまうため、payload に必ず含める。
        let loading = serde_json::to_value(EngineStatus::new(
            EngineRole::Floating,
            EnginePhase::Loading,
            Some(text("engine.loadingDetail", "モデルを読み込んでいます。")),
        ))
        .unwrap();
        assert_eq!(loading["role"], "floating");
        assert_eq!(loading["state"], "loading");
        assert_eq!(loading["detail"]["key"], "engine.loadingDetail");
        assert_eq!(loading["detail"]["fallback"], "モデルを読み込んでいます。");

        let error = serde_json::to_value(EngineStatus::new(
            EngineRole::Main,
            EnginePhase::Error,
            Some(text("error.raw", "見つかりません")),
        ))
        .unwrap();
        assert_eq!(error["state"], "error");
    }

    #[test]
    fn roles_are_derived_from_window_labels_and_ipc_arguments() {
        // translate のルーティングは WebView のラベルで決まる。
        assert_eq!(
            EngineRole::from_label(crate::floating::FLOATING_LABEL),
            EngineRole::Floating
        );
        assert_eq!(
            EngineRole::from_label(crate::floating::MAIN_LABEL),
            EngineRole::Main
        );
        // 未知のラベルはメイン扱い (増えても翻訳を止めない)。
        assert_eq!(EngineRole::from_label("settings"), EngineRole::Main);
        assert_eq!(EngineRole::from_label(""), EngineRole::Main);

        // get_engine_status の role 引数。省略・未知はメイン扱い。
        assert_eq!(EngineRole::from_arg(Some("floating")), EngineRole::Floating);
        assert_eq!(EngineRole::from_arg(Some("main")), EngineRole::Main);
        assert_eq!(EngineRole::from_arg(None), EngineRole::Main);
        assert_eq!(EngineRole::from_arg(Some("")), EngineRole::Main);

        assert_eq!(EngineRole::Main.as_str(), "main");
        assert_eq!(EngineRole::Floating.as_str(), "floating");
    }

    #[test]
    fn floating_role_uses_its_own_model_and_falls_back_to_the_active_one() {
        let base = temp_dir("floating-model");
        let active = base.join("active.gguf");
        fs::write(&active, b"m").unwrap();
        let small = base.join("small.gguf");
        fs::write(&small, b"m").unwrap();

        let mut settings = Settings {
            active_model: Some("active.gguf".to_string()),
            installed_models: vec![
                InstalledModel {
                    file: "active.gguf".to_string(),
                    repo: "pfnet/test".to_string(),
                    bytes: 1,
                    path: active.to_string_lossy().into_owned(),
                    installed_at: 0,
                },
                InstalledModel {
                    file: "small.gguf".to_string(),
                    repo: "pfnet/test".to_string(),
                    bytes: 1,
                    path: small.to_string_lossy().into_owned(),
                    installed_at: 0,
                },
            ],
            ..Default::default()
        };

        // 専用モデルが未指定ならアクティブモデル (従来どおり)。
        assert_eq!(EngineRole::Floating.model_override(&settings), None);
        assert_eq!(
            resolve_model_path_for(&settings, &base, None).unwrap(),
            active
        );

        // 専用モデルがあればそちらを使う。
        settings.floating_window.model = Some("small.gguf".to_string());
        assert_eq!(
            EngineRole::Floating.model_override(&settings),
            Some("small.gguf")
        );
        assert_eq!(
            resolve_model_path_for(
                &settings,
                &base,
                EngineRole::Floating.model_override(&settings)
            )
            .unwrap(),
            small
        );

        // 専用モデルの実ファイルが消えても翻訳を止めない (アクティブへフォールバック)。
        settings.installed_models[1].path =
            base.join("missing.gguf").to_string_lossy().into_owned();
        assert_eq!(
            resolve_model_path_for(
                &settings,
                &base,
                EngineRole::Floating.model_override(&settings)
            )
            .unwrap(),
            active
        );

        // 未登録の ID でも同じくフォールバックする。
        settings.floating_window.model = Some("unknown.gguf".to_string());
        assert_eq!(
            resolve_model_path_for(
                &settings,
                &base,
                EngineRole::Floating.model_override(&settings)
            )
            .unwrap(),
            active
        );

        // メインは専用モデルの設定に影響されない。
        settings.floating_window.model = Some("small.gguf".to_string());
        assert_eq!(EngineRole::Main.model_override(&settings), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn both_not_installed_reports_the_floating_override_in_its_paths() {
        // 専用モデルも通常チェーンも解決できないときは、確認した場所に専用モデルの
        // 情報を残す (どの設定が原因か分かるようにする)。
        let empty = temp_dir("floating-model-missing");
        let settings = Settings {
            floating_window: crate::settings::FloatingWindowSettings {
                model: Some("unknown.gguf".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let error = resolve_model_path_for(
            &settings,
            &empty,
            EngineRole::Floating.model_override(&settings),
        )
        .unwrap_err();
        assert!(error
            .fallback
            .contains("フローティング用モデル unknown.gguf"));
        assert!(error
            .params
            .get("paths")
            .is_some_and(|paths| paths.contains("unknown.gguf")));
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn identical_resolved_models_share_the_main_engine() {
        let base = temp_dir("shared-model");
        let active = base.join("active.gguf");
        fs::write(&active, b"m").unwrap();
        let mut settings = Settings {
            active_model: Some("active.gguf".to_string()),
            installed_models: vec![InstalledModel {
                file: "active.gguf".to_string(),
                repo: "pfnet/test".to_string(),
                bytes: 1,
                path: active.to_string_lossy().into_owned(),
                installed_at: 0,
            }],
            ..Default::default()
        };

        // 既定 (専用モデル未指定): 両役割ともアクティブモデルを解決する。
        assert_eq!(
            resolved_model_path(EngineRole::Main, &settings, &base),
            Some(active.clone())
        );
        assert_eq!(
            resolved_model_path(EngineRole::Floating, &settings, &base),
            Some(active.clone())
        );
        assert!(models_shared(&settings, &base));
        assert_eq!(
            serving_role(EngineRole::Floating, &settings, &base),
            EngineRole::Main
        );
        assert_eq!(
            serving_role(EngineRole::Main, &settings, &base),
            EngineRole::Main
        );
        assert!(!should_run_floating_engine(&settings, &base));

        // 専用モデルに「アクティブモデルと同じファイル」を選んでも共有する (N-2)。
        settings.floating_window.model = Some("active.gguf".to_string());
        assert!(settings.floating_engine_resident());
        assert!(models_shared(&settings, &base));
        assert!(!should_run_floating_engine(&settings, &base));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_different_dedicated_model_uses_the_floating_engine() {
        let base = temp_dir("different-model");
        let active = base.join("active.gguf");
        fs::write(&active, b"m").unwrap();
        let small = base.join("small.gguf");
        fs::write(&small, b"m").unwrap();
        let mut settings = Settings {
            active_model: Some("active.gguf".to_string()),
            installed_models: vec![
                InstalledModel {
                    file: "active.gguf".to_string(),
                    repo: "pfnet/test".to_string(),
                    bytes: 1,
                    path: active.to_string_lossy().into_owned(),
                    installed_at: 0,
                },
                InstalledModel {
                    file: "small.gguf".to_string(),
                    repo: "pfnet/test".to_string(),
                    bytes: 1,
                    path: small.to_string_lossy().into_owned(),
                    installed_at: 0,
                },
            ],
            ..Default::default()
        };
        settings.floating_window.model = Some("small.gguf".to_string());

        assert!(!models_shared(&settings, &base));
        assert_eq!(
            serving_role(EngineRole::Floating, &settings, &base),
            EngineRole::Floating
        );
        // 常時起動の既定 ON なら独立エンジンを維持する。
        assert!(should_run_floating_engine(&settings, &base));
        // 常時起動 OFF ならオンデマンド (独立エンジンは自動では起動しない)。
        settings.background.resident_engine = false;
        assert!(!should_run_floating_engine(&settings, &base));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_dedicated_model_falls_back_to_the_active_model_and_is_shared() {
        let base = temp_dir("missing-dedicated");
        let active = base.join("active.gguf");
        fs::write(&active, b"m").unwrap();
        let settings = Settings {
            active_model: Some("active.gguf".to_string()),
            installed_models: vec![InstalledModel {
                file: "active.gguf".to_string(),
                repo: "pfnet/test".to_string(),
                bytes: 1,
                path: active.to_string_lossy().into_owned(),
                installed_at: 0,
            }],
            floating_window: crate::settings::FloatingWindowSettings {
                model: Some("missing.gguf".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        // 未登録の専用モデルは既存チェーンへフォールバック → メインと同一パス。
        assert_eq!(
            resolved_model_path(EngineRole::Floating, &settings, &base),
            Some(active)
        );
        assert!(models_shared(&settings, &base));
        assert_eq!(
            serving_role(EngineRole::Floating, &settings, &base),
            EngineRole::Main
        );
        // 常時起動の条件は満たすが、専用モデルが解決できないので常駐しない
        // (黙ってメイン用を維持しない: 安全ガード)。
        assert!(settings.floating_engine_resident());
        assert_eq!(resident_target(&settings, &base), None);
        assert!(!should_run_floating_engine(&settings, &base));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unresolved_models_are_not_shared() {
        let base = temp_dir("unresolved-models");
        let settings = Settings::default();
        assert_eq!(
            resolved_model_path(EngineRole::Main, &settings, &base),
            None
        );
        assert!(!models_shared(&settings, &base));
        // 解決できないときは役割どおり (起動時のエラーはエンジン側が出す)。
        assert_eq!(
            serving_role(EngineRole::Floating, &settings, &base),
            EngineRole::Floating
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// 常駐ポリシーの predicate 表。
    ///
    /// 設定の組み合わせごとに「保持先・モデル・理由」を 1 か所で固定する。ここが
    /// 変わるときは、起動・設定変更・メイン非表示・フローティング終了の全経路が
    /// 同じ判断 (`resident_target`) を共有していることを確認すること。
    #[test]
    fn resident_policy_table_is_the_single_source_of_truth() {
        let base = temp_dir("resident-policy");
        let active = base.join("14b.gguf");
        fs::write(&active, b"m").unwrap();
        let small = base.join("0.8b.gguf");
        fs::write(&small, b"m").unwrap();
        let installed = |file: &str, path: &Path| InstalledModel {
            file: file.to_string(),
            repo: "pfnet/test".to_string(),
            bytes: 1,
            path: path.to_string_lossy().into_owned(),
            installed_at: 0,
        };
        let settings = Settings {
            active_model: Some("14b.gguf".to_string()),
            installed_models: vec![
                installed("14b.gguf", &active),
                installed("0.8b.gguf", &small),
            ],
            ..Default::default()
        };
        let policy = |settings: &Settings| {
            resident_target(settings, &base)
                .map(|target| (target.role, target.model, target.reason))
        };

        // 専用モデル未指定: 既定の常駐 ON でも何も保持しない
        // (アクティブモデルを二重ロードしない)。
        assert_eq!(policy(&settings), None);
        assert!(!should_run_floating_engine(&settings, &base));

        // 別ファイルの専用モデル: フローティング用を 1 本保持する。
        let mut split = settings.clone();
        split.floating_window.model = Some("0.8b.gguf".to_string());
        assert_eq!(
            policy(&split),
            Some((
                EngineRole::Floating,
                small.clone(),
                ResidentReason::DedicatedModel
            ))
        );
        assert!(should_run_floating_engine(&split, &base));

        // 専用モデルにアクティブと同じファイル: 2 本目は起こさず、
        // 共有しているメイン用 1 本を保持する (案A)。
        let mut shared = settings.clone();
        shared.floating_window.model = Some("14b.gguf".to_string());
        assert_eq!(
            policy(&shared),
            Some((
                EngineRole::Main,
                active.clone(),
                ResidentReason::SharedWithMain
            ))
        );
        assert!(!should_run_floating_engine(&shared, &base));

        // 常駐 OFF / トレイ常駐 OFF / フローティング無効は保持しない。
        let mut off = split.clone();
        off.background.resident_engine = false;
        assert_eq!(policy(&off), None);
        assert!(!should_run_floating_engine(&off, &base));
        let mut no_tray = split.clone();
        no_tray.background.enabled = false;
        assert_eq!(policy(&no_tray), None);
        let mut disabled = split.clone();
        disabled.floating_window.enabled = false;
        assert_eq!(policy(&disabled), None);

        // 未登録の専用モデル: フォールバックせず常駐しない (安全ガード)。
        let mut unknown = split.clone();
        unknown.floating_window.model = Some("unknown.gguf".to_string());
        assert_eq!(policy(&unknown), None);
        assert!(!should_run_floating_engine(&unknown, &base));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_dedicated_model_is_not_kept_resident() {
        // 0.8B を選んでいたが実ファイルが消えた。翻訳の解決は 14B へフォール
        // バックするが、常駐は「黙ってメイン用を維持する」ことをしない。
        let base = temp_dir("resident-missing-dedicated");
        let active = base.join("14b.gguf");
        fs::write(&active, b"m").unwrap();
        let settings = Settings {
            active_model: Some("14b.gguf".to_string()),
            installed_models: vec![
                InstalledModel {
                    file: "14b.gguf".to_string(),
                    repo: "pfnet/test".to_string(),
                    bytes: 1,
                    path: active.to_string_lossy().into_owned(),
                    installed_at: 0,
                },
                InstalledModel {
                    file: "0.8b.gguf".to_string(),
                    repo: "pfnet/test".to_string(),
                    bytes: 1,
                    path: base.join("missing.gguf").to_string_lossy().into_owned(),
                    installed_at: 0,
                },
            ],
            floating_window: crate::settings::FloatingWindowSettings {
                model: Some("0.8b.gguf".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(settings.floating_engine_resident());
        assert_eq!(
            resolved_model_path(EngineRole::Floating, &settings, &base),
            Some(active)
        );
        assert!(models_shared(&settings, &base));
        assert_eq!(resident_target(&settings, &base), None);
        assert!(!should_run_floating_engine(&settings, &base));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn resident_target_moves_between_shared_and_split_models() {
        let base = temp_dir("resident-transitions");
        let wide = base.join("14b.gguf");
        fs::write(&wide, b"m").unwrap();
        let small = base.join("0.8b.gguf");
        fs::write(&small, b"m").unwrap();
        let installed = |file: &str, path: &Path| InstalledModel {
            file: file.to_string(),
            repo: "pfnet/test".to_string(),
            bytes: 1,
            path: path.to_string_lossy().into_owned(),
            installed_at: 0,
        };
        let mut settings = Settings {
            active_model: Some("14b.gguf".to_string()),
            installed_models: vec![installed("14b.gguf", &wide), installed("0.8b.gguf", &small)],
            ..Default::default()
        };
        settings.floating_window.model = Some("0.8b.gguf".to_string());

        // 専用モデルが別ファイル: フローティング用を保持。
        let target = resident_target(&settings, &base).expect("常駐する");
        assert_eq!(target.role, EngineRole::Floating);
        assert_eq!(target.model, small);
        assert_eq!(target.reason, ResidentReason::DedicatedModel);
        assert!(should_run_floating_engine(&settings, &base));

        // アクティブを専用モデルと同じにすると共有: メイン用 1 本を保持。
        settings.active_model = Some("0.8b.gguf".to_string());
        let target = resident_target(&settings, &base).expect("常駐する");
        assert_eq!(target.role, EngineRole::Main);
        assert_eq!(target.reason, ResidentReason::SharedWithMain);
        assert!(!should_run_floating_engine(&settings, &base));

        // アクティブを戻すとまた専用エンジン。保持先が Main↔Floating で移る。
        settings.active_model = Some("14b.gguf".to_string());
        assert_eq!(
            resident_target(&settings, &base).map(|target| target.role),
            Some(EngineRole::Floating)
        );
        let _ = fs::remove_dir_all(&base);
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
