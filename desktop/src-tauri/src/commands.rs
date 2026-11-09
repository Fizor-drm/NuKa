//! フロントエンドとの IPC (Task B がこのコントラクトどおりに実装する)。
//!
//! コマンド:
//!   get_engine_status(role?) -> { role, state, detail? }
//!     role は "main" / "floating"。省略時は main (フローティング画面は自分で指定する)。
//!   restart_engine() -> ()
//!   get_settings() / set_settings(patch) -> Settings
//!   detect_language(text) -> Option<{ code, confidence }>
//!   translate(req: { text, source_lang?, target_lang, form?, glossary? }) -> jobId
//!     jobId のエンジンは呼び出し元ウィンドウのラベルで決まる
//!     ("floating" → フローティング用、それ以外 → メイン用)。
//!   cancel_translation(jobId) -> ()
//!
//! イベント:
//!   engine:status { role, state, detail? }
//!     role ごとに 2 インスタンスぶん流れる。UI は自役割のみ反映する。
//!   translation:progress { jobId, done, total }
//!   translation:chunk { jobId, delta }
//!   translation:done { jobId, text, tokens? }
//!   translation:error { jobId, message, messageKey? }
//!
//! `messageKey` は辞書 (`src/i18n.ts`) の key。付いているときは `message` を
//! フォールバックとして表示言語に合わせて訳す (`localize`)。

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::timeout;

use crate::detect::{self, DetectedLanguage};
use crate::engine::{
    Engine, EnginePhase, EngineReady, EngineRole, EngineStatus, ReservedStart, ENGINE_READY_LIMIT,
    SERVER_ALIAS,
};
use crate::floating;
use crate::message::LocalizedText;
use crate::prompt::{
    self, GlossaryPair, HistoryPair, PromptError, PromptOptions, CHUNK_LIMIT, STOP_TOKENS,
};
use crate::settings::{Settings, SettingsPatch};
use crate::AppState;

/// 1 チャンクあたりの生成上限。
const MAX_TOKENS: u32 = 1024;
/// プロンプトに載せる履歴の上限 (直近 3 対)。
const HISTORY_LIMIT: usize = 3;
/// ストリームがこれだけ黙ったら諦める。
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// SSE の 1 行 (改行待ちのバッファ) に許す上限。
const MAX_SSE_LINE_BYTES: usize = 256 * 1024;
/// 生成途中の訳文を溜めておく上限。
const MAX_TRANSLATION_BYTES: usize = 1024 * 1024;
/// エラー応答の本文から読む先頭バイト数。
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct TranslateRequest {
    pub text: String,
    pub source_lang: Option<String>,
    pub target_lang: String,
    /// `"plain"` / `"polite"`。訳先が `ja` のときだけ効く。
    pub form: Option<String>,
    #[serde(default)]
    pub glossary: Vec<GlossaryPair>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    job_id: u32,
    done: usize,
    total: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChunkEvent {
    job_id: u32,
    delta: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    job_id: u32,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens: Option<u32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    job_id: u32,
    message: String,
    /// 辞書 (`src/i18n.ts`) の key。あれば UI 側が表示言語に合わせて訳す。
    #[serde(skip_serializing_if = "Option::is_none")]
    message_key: Option<String>,
}

/// 新しい翻訳に jobId を差し替えられたことを知らせる。
/// 古いウィンドウは自ジョブなら翻訳中表示を止める (done も error も来ないため)。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupersededEvent {
    job_id: u32,
}

/// 進行中のジョブ。新しい translate が来たら古いジョブのフラグを立てて捨てる。
#[derive(Clone, Default)]
pub struct JobManager {
    inner: Arc<JobManagerInner>,
}

#[derive(Default)]
struct JobManagerInner {
    next_id: AtomicU32,
    current: Mutex<Option<JobControl>>,
}

struct JobControl {
    id: u32,
    cancel: Arc<AtomicBool>,
    /// どのウィンドウから依頼されたジョブか ("main" / "floating")。
    origin: String,
}

impl JobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 新しいジョブを始める。進行中のジョブは捨てる。
    ///
    /// 戻り値は (新しい jobId, 中止フラグ, 差し替えた前の jobId)。
    fn begin(&self, origin: &str) -> (u32, Arc<AtomicBool>, Option<u32>) {
        let mut current = self.lock();
        let previous = current.take();
        let previous_id = previous.as_ref().map(|control| {
            control.cancel.store(true, Ordering::SeqCst);
            control.id
        });
        let id = self
            .inner
            .next_id
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        *current = Some(JobControl {
            id,
            cancel: cancel.clone(),
            origin: origin.to_string(),
        });
        (id, cancel, previous_id)
    }

    fn cancel(&self, job_id: u32) {
        if let Some(control) = self.lock().as_ref() {
            if control.id == job_id {
                control.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    fn finish(&self, job_id: u32) {
        let mut current = self.lock();
        if current.as_ref().is_some_and(|control| control.id == job_id) {
            *current = None;
        }
    }

    /// 進行中のジョブの jobId。無ければ `None`。
    pub fn current_job_id(&self) -> Option<u32> {
        self.lock().as_ref().map(|control| control.id)
    }

    /// 指定したウィンドウ由来のジョブだけを止める。
    ///
    /// フローティングウィンドウを閉じるときに、メイン画面の翻訳を巻き込まないため。
    pub fn cancel_current_for_origin(&self, origin: &str) {
        if let Some(control) = self.lock().as_ref() {
            if control.origin == origin {
                control.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    /// `origin` 以外のジョブが進行中か。
    ///
    /// フローティングを閉じたあとに、ほかの画面 (メイン) の翻訳を
    /// 巻き込まずにエンジンを解放してよいかの判定に使う。
    pub fn has_job_from_other_origin(&self, origin: &str) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|control| control.origin != origin)
    }

    /// `origin` 由来のジョブがまだ走っているか。
    ///
    /// 中止フラグの立ったジョブ (superseded / 画面を閉じた) は「走っていない」と
    /// みなす。エンジンを止めてよいかの判定に使う。
    pub fn has_running_job_from_origin(&self, origin: &str) -> bool {
        self.lock().as_ref().is_some_and(|control| {
            control.origin == origin && !control.cancel.load(Ordering::SeqCst)
        })
    }

    /// まだ中止されていない進行中のジョブがあるか。
    ///
    /// 中止フラグの立ったジョブは、エンジンを使い終えるのを待たずに解放してよい
    /// (`has_running_job_from_origin` と同じ扱い)。常駐ポリシーの再同期で
    /// 「いま翻訳中のエンジンは中断しない」を判定するために使う。
    pub fn has_active_job(&self) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|control| !control.cancel.load(Ordering::SeqCst))
    }

    fn lock(&self) -> MutexGuard<'_, Option<JobControl>> {
        self.inner
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// コマンド
// ---------------------------------------------------------------------------

/// エンジンの状態を返す。
///
/// `role` は `"floating"` でフローティング用エンジン、省略・`"main"` でメイン用。
/// フローティング画面は自分の役割を渡し、メイン画面の状態と混ざらないようにする。
#[tauri::command]
pub fn get_engine_status(state: State<'_, AppState>, role: Option<String>) -> EngineStatus {
    let role = EngineRole::from_arg(role.as_deref());
    state.engine_for(role).status()
}

/// メイン用エンジンを起動し直す。
///
/// 明示的な再起動要求なので、予約済みの停止/再起動より優先する。起動完了後は
/// 常駐ポリシーを再評価する (M1)。フローティング用は常駐ポリシーの再同期が担当する。
#[tauri::command]
pub fn restart_engine(app: AppHandle) -> Result<(), String> {
    start_engine_with_sync(&app, EngineRole::Main, true);
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.get()
}

#[tauri::command]
pub fn set_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> Result<Settings, String> {
    let language_changed = patch.ui_language.is_some();
    let before = state.settings.get();
    let saved = state.settings.apply(patch)?;
    // トレイメニューは WebView の辞書を通らないので、表示言語が変わったら
    // Rust 側で作り直す (失敗しても設定の保存は成功している)。
    if language_changed {
        crate::tray::refresh_labels(&app);
    }
    // 常時起動に関わる設定 (常駐 ON/OFF・トレイ常駐・モデル) が変わったら、
    // 常駐ポリシー (保持先・解決モデルパス) の変化をエンジンへ反映する。
    // 変わっていなければ何もしない (無関係な設定保存でエンジンを触らない)。
    sync_resident_engine(&app, &before, &saved);
    Ok(saved)
}

/// エンジン同期の予約 (世代)。
///
/// 予約を発行した時点と実行した時点で、設定・ジョブの状態が変わっている可能性が
/// ある。実行時にこの世代とジョブの有無を再検証し、古い停止/再起動を破棄する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SyncTicket(u64);

/// 同期予約の世代管理 (H1)。
///
/// - `Self::schedule`: 設定変更・起動完了などで「後で同期する」予約を発行する
/// - `Self::invalidate`: ジョブ受け付けなど、予約より優先する操作で古い予約を捨てる
/// - `Self::may_apply`: 実行時に、最新の予約でかつジョブが走っていないかを確認する
///
/// 確認と適用は [`EngineControl`] の同じロック保持中に行う。ジョブ受け付けが
/// 先に起きた場合、その予約は実行時に破棄され、ジョブ完了後の再同期に委ねる。
#[derive(Default)]
struct SyncEpoch {
    epoch: u64,
}

impl SyncEpoch {
    /// 新しい同期予約を発行する。古い予約は無効になる。
    fn schedule(&mut self) -> SyncTicket {
        self.epoch = self.epoch.wrapping_add(1);
        SyncTicket(self.epoch)
    }

    /// 予約より優先する操作 (ジョブ受け付け・明示的な起動要求・hide・終了) で
    /// 古い予約を捨てる。
    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// いまの世代。可視性スナップショットの前後で変わっていないかを見る。
    fn epoch(&self) -> u64 {
        self.epoch
    }

    /// この予約がまだ最新か。
    fn is_current(&self, ticket: SyncTicket) -> bool {
        self.epoch == ticket.0
    }

    /// この予約を実行してよいか (最新かつジョブが走っていない)。
    fn may_apply(&self, ticket: SyncTicket, in_use: bool) -> bool {
        !in_use && self.is_current(ticket)
    }
}

/// ジョブ受け付けとエンジンのライフサイクル操作を直列化する制御ロック。
///
/// `translate` の受け付け、設定変更・起動完了後の同期、明示的な起動要求が
/// この 1 本のロックを共有する。ロック保持中にメインスレッドへ問い合わせる API
/// (`is_visible` など) を呼ばないこと (デッドロック防止)。表示状態はロックを
/// 取る前に読む。
#[derive(Default)]
pub struct EngineControl {
    inner: Mutex<EngineControlInner>,
}

#[derive(Default)]
pub(crate) struct EngineControlInner {
    sync: SyncEpoch,
    /// 終了処理が始まったか。以後の起動予約をすべて拒否する。
    ///
    /// `stop_engines` の後に残っている同期 ticket やホットキー由来の起動要求が
    /// エンジンを復活させないためのガード (監査 Medium 2)。
    shutting_down: bool,
    /// ホットキー記録中に一時保存した `floating_window.enabled=false` を、通常の
    /// OFF と区別するための記録前の値。
    ///
    /// 記録中は `enabled=false` が settings.json に書かれるが、ユーザーの意図は
    /// 「記録の間だけホットキーを外す」であって「常駐をやめる」ではない。記録中に
    /// 起動完了・ジョブ完了の同期が走っても常駐エンジンを止めないよう、ポリシー
    /// 評価ではここに覚えた値を使う (M3)。
    suspended_floating_enabled: Option<bool>,
}

impl EngineControlInner {
    /// ポリシー評価に使う実効設定。記録中は記録前の `enabled` に戻す。
    pub(crate) fn effective_settings(&self, mut settings: Settings) -> Settings {
        if let Some(enabled) = self.suspended_floating_enabled {
            settings.floating_window.enabled = enabled;
        }
        settings
    }

    /// 予約済みの同期を無効化する (状態変更・停止・終了の前に呼ぶ)。
    pub(crate) fn invalidate_sync(&mut self) {
        self.sync.invalidate();
    }

    /// いまの同期世代。可視性スナップショットの前後で変わっていないかを見る。
    ///
    /// 可視性はメインスレッド往復 API のためロック外で読む必要がある。読む前後で
    /// 世代を照合し、hide/show などの状態変更が入っていたら古い値を使わない
    /// (Medium 2)。
    pub(crate) fn sync_epoch(&self) -> u64 {
        self.sync.epoch()
    }

    /// 新しい起動を予約してよいか。
    pub(crate) fn may_start(&self) -> bool {
        !self.shutting_down
    }

    /// 同期 ticket を適用してよいか (最新・ジョブ無し・シャットダウン中でない)。
    fn may_apply(&self, ticket: SyncTicket, in_use: bool) -> bool {
        !self.shutting_down && self.sync.may_apply(ticket, in_use)
    }

    /// 終了処理を開始する。以後の起動予約と同期適用を拒否する。
    fn begin_shutdown(&mut self) {
        self.shutting_down = true;
        self.sync.invalidate();
    }
}

impl EngineControl {
    /// 制御ロック。ロック保持中にメインスレッドへ問い合わせる API を呼ばないこと。
    pub(crate) fn lock(&self) -> MutexGuard<'_, EngineControlInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// ホットキー記録の一時無効化を開始する。記録前の `enabled` を覚える。
    ///
    /// すでに記録中なら最初に覚えた値を保つ (二重送信で上書きしない)。
    pub(crate) fn begin_hotkey_suspend(&self, enabled: bool) {
        self.lock()
            .suspended_floating_enabled
            .get_or_insert(enabled);
    }

    /// 記録終了後の明示的な `enabled` 変更で一時無効化を解除する。
    pub(crate) fn end_hotkey_suspend(&self) {
        self.lock().suspended_floating_enabled = None;
    }

    /// ポリシー評価に使う実効設定 (記録中は記録前の `enabled`)。
    pub(crate) fn effective_settings(&self, settings: Settings) -> Settings {
        self.lock().effective_settings(settings)
    }
}

/// AppHandle から実効設定を引く。記録中 (suspend) は記録前の `enabled` を使う。
pub(crate) fn effective_settings(app: &AppHandle, settings: Settings) -> Settings {
    match app.try_state::<AppState>() {
        Some(state) => state.engine_control.effective_settings(settings),
        None => settings,
    }
}

/// 可視性やウィンドウ状態が変わったことを制御境界へ知らせ、予約済みの同期を
/// 無効化する。
///
/// 同期 ticket は可視性をロック外で読むため、読んだ後に hide/show が起きても
/// 古い値で適用しないよう、状態変更側で世代を進める (監査 Medium 2)。
pub(crate) fn invalidate_engine_sync(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.engine_control.lock().invalidate_sync();
    }
}

/// 終了処理を開始し、以後の起動予約を拒否する。
pub(crate) fn begin_shutdown(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.engine_control.lock().begin_shutdown();
    }
}

/// エンジン 1 本に対して取るべき同期動作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineSyncAction {
    /// 何もしない (稼働中はそのまま、停止中も起こさない)。
    Keep,
    /// 止める (起動処理中ならその起動も取り消す)。
    Stop,
    /// 止まっていれば起こす (起動中はそのまま待つ)。
    Start,
    /// 今の設定のモデルで起こし直す。
    Restart,
}

/// 1 本のエンジンの同期動作を決める。
///
/// - `warm`: 常駐の保持先か、画面が見えていて温めておく対象 (停止中なら起こす)
/// - `keep_alive`: 今まさに使われている (止めない。自動起動はしない)
/// - `in_use`: 翻訳ジョブが動いている (中断しない)
/// - `model_matches`: 稼働中の子が今の設定と同じモデルを読んでいる
/// - `resolvable`: 今の設定でモデルとランタイムを解決できる
///
/// 起動処理中は `warm` / `keep_alive` だけで判断する。古い設定を掴んだ起動は、
/// 完了後の再評価 (`spawn_reserved_with_sync` の世代ガード) が最新ポリシーで
/// 始末する。
fn decide_engine_sync(
    warm: bool,
    keep_alive: bool,
    in_use: bool,
    starting: bool,
    running: bool,
    model_matches: bool,
    resolvable: bool,
) -> EngineSyncAction {
    if in_use {
        return EngineSyncAction::Keep;
    }
    if starting {
        return if warm || keep_alive {
            EngineSyncAction::Keep
        } else {
            EngineSyncAction::Stop
        };
    }
    if running {
        if !warm && !keep_alive {
            return EngineSyncAction::Stop;
        }
        return if model_matches {
            EngineSyncAction::Keep
        } else {
            EngineSyncAction::Restart
        };
    }
    if warm && resolvable {
        EngineSyncAction::Start
    } else {
        EngineSyncAction::Keep
    }
}

/// 稼働中の子が今の設定と同じモデルを読んでいるか。
///
/// 停止中は比べようがないので「一致」とみなす (起動は呼び出し側が決める)。
fn running_model_matches(engine: &Engine, desired: Option<&Path>) -> bool {
    match (engine.running_model(), desired) {
        (Some(running), Some(desired)) => running == desired,
        _ => true,
    }
}

/// ジョブ受け付け時に、そのジョブを訳すエンジンをどうするか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobEngineAction {
    /// いまのエンジンをそのまま使う (希望モデルと一致、またはモデル未解決)。
    Use,
    /// 停止中なので起こす。
    Start,
    /// 稼働中/起動中だが希望モデルと違うので、希望モデルで起こし直す。
    Restart,
}

/// ジョブ受け付け時のエンジン動作を決める。
///
/// 連続ジョブ (J1: モデル A → 設定 B → J2) で J2 が A のまま訳さないよう、
/// 稼働中・起動中のどちらも「希望モデルと一致するか」を確認する (M2)。
/// 起動処理中は `reserved_model` で、子の spawn 前でも判定できる。
/// モデルが解決できないときは現状に任せる (既存のエラー表示経路)。
fn decide_job_engine(
    desired: Option<&Path>,
    starting: bool,
    reserved_model: Option<&Path>,
    running: bool,
    running_model: Option<&Path>,
) -> JobEngineAction {
    let Some(desired) = desired else {
        return JobEngineAction::Use;
    };
    if starting {
        return if reserved_model == Some(desired) {
            JobEngineAction::Use
        } else {
            JobEngineAction::Restart
        };
    }
    if running {
        return if running_model == Some(desired) {
            JobEngineAction::Use
        } else {
            JobEngineAction::Restart
        };
    }
    JobEngineAction::Start
}

/// 予約済みの起動を非同期で実行し、必要なら完了後に常駐ポリシーを再評価する。
///
/// 起動に失敗した (子を掴んでいない) ときは再評価しない。再評価すると、起動失敗
/// のたびに同じ起動を繰り返してしまう。
///
/// **後追い同期は「起動中に常駐ポリシーかモデルが変わった」ときだけ**走らせる。
/// ホットキー経路は [`crate::ensure_engine_started`] を `window.show()` /
/// `set_shown(true)` より前に呼ぶ (floating.rs の `capture_and_show`)。attach 直後に
/// 無条件で同期すると、その時点では `floating_shown=false`・ジョブ無し・非保持先で
/// Stop と判定され、ロード中のエンジンを止めて次の translate で二重ロードになる。
/// イベントループ開始前の setup ではメインウィンドウは config (tauri.conf.json の
/// `main`、`visible` 既定 true) の作成が setup より先に行われるが、可視性に依存
/// しない判定にしておく。
fn spawn_reserved_with_sync(
    app: &AppHandle,
    engine: &Engine,
    reserved: ReservedStart,
    settings: Settings,
) {
    let app_handle = app.clone();
    let engine = engine.clone();
    tauri::async_runtime::spawn(async move {
        engine.spawn_reserved(reserved, &settings).await;
        if !resync_after_start(engine.has_process()) {
            return;
        }
        let Some(state) = app_handle.try_state::<AppState>() else {
            return;
        };
        let latest = {
            let control = state.engine_control.lock();
            control.effective_settings(state.settings.get())
        };
        let search_root = crate::engine::exe_directory();
        if start_needs_follow_up_sync(engine.role(), &settings, &latest, &search_root) {
            sync_engines_now(&app_handle);
        }
    });
}

/// 起動完了後に常駐ポリシーの再評価を依頼するか (M1)。
///
/// 起動に失敗した (子を掴んでいない) ときに再評価すると、同じ起動を繰り返して
/// しまうため行わない。成功時だけ、ポリシー/モデルの変化を確認する。
fn resync_after_start(has_process: bool) -> bool {
    has_process
}

/// 起動に使った設定と現在の実効設定で、後追い同期が必要か。
///
/// 次のどちらかが変わったときだけ true:
/// - 常駐ポリシー ([`crate::engine::resident_target`] の保持先・解決モデル・理由)
/// - このエンジンの役割が読むモデル ([`crate::engine::resolved_model_path`])
///
/// ホットキー・起動時・復元・ジョブ復旧の「明示的な事前起動」は、たとえ
/// フローティングウィンドウがまだ表示されていなくても、来るべきジョブのために
/// 保持する。無関係な設定 (ホットキー等) の変化でも同期しない。
fn start_needs_follow_up_sync(
    role: EngineRole,
    started: &Settings,
    latest: &Settings,
    search_root: &Path,
) -> bool {
    if crate::engine::resident_target(started, search_root)
        != crate::engine::resident_target(latest, search_root)
    {
        return true;
    }
    crate::engine::resolved_model_path(role, started, search_root)
        != crate::engine::resolved_model_path(role, latest, search_root)
}

/// 起動を予約して spawn を依頼する。呼び出し側は制御ロックを保持していること。
///
/// 予約 (`reserve_start`) は既存の子の停止までを同期に行う。このため、
/// ジョブ受け付けと「止める/起こし直す」の順序は制御ロックで確定する (H1)。
/// 終了処理が始まっていたら予約しない (Medium 2)。
///
/// 戻り値は「この呼び出しで起動を予約したか」。spawn は別ワーカーで走るため、
/// 予約できたかどうかを `engine.is_starting()` の再読みで判定してはならない
/// (spawn が先に完了すると false になり、起動中なのに失敗と誤判定する: Minor)。
/// 予約した時点で true を返す。
fn reserve_and_spawn(
    app: &AppHandle,
    engine: &Engine,
    settings: &Settings,
    force: bool,
    control: &EngineControlInner,
) -> bool {
    if !control.may_start() {
        return false;
    }
    let Some(reserved) = engine.reserve_start(settings, force) else {
        return false;
    };
    spawn_reserved_with_sync(app, engine, reserved, settings.clone());
    true
}

/// ジョブが使うエンジンを、そのジョブの希望モデルに揃える (M2)。
///
/// 呼び出し側は制御ロックを保持し、ジョブを begin した後に呼ぶこと
/// (`jobs.begin` が前のジョブを中止にしてからエンジンを触る)。
fn prepare_engine_for_job(
    app: &AppHandle,
    state: &AppState,
    role: EngineRole,
    settings: &Settings,
    control: &EngineControlInner,
) {
    let engine = state.engine_for(role);
    let search_root = crate::engine::exe_directory();
    let desired = crate::engine::resolved_model_path(role, settings, &search_root);
    let action = decide_job_engine(
        desired.as_deref(),
        engine.is_starting(),
        engine.reserved_model().as_deref(),
        engine.has_process(),
        engine.running_model().as_deref(),
    );
    match action {
        JobEngineAction::Use => {}
        JobEngineAction::Start => {
            let _ = reserve_and_spawn(app, engine, settings, false, control);
        }
        JobEngineAction::Restart => {
            let _ = reserve_and_spawn(app, engine, settings, true, control);
        }
    }
}

/// 同期動作をエンジンへ適用する。呼び出し側は制御ロックを保持していること。
fn apply_engine_sync(
    app: &AppHandle,
    engine: &Engine,
    action: EngineSyncAction,
    settings: &Settings,
    control: &EngineControlInner,
) {
    match action {
        EngineSyncAction::Keep => {}
        EngineSyncAction::Stop => engine.kill_now(),
        EngineSyncAction::Start => {
            let _ = reserve_and_spawn(app, engine, settings, false, control);
        }
        EngineSyncAction::Restart => {
            let _ = reserve_and_spawn(app, engine, settings, true, control);
        }
    }
}

/// 常駐ポリシーと使用状況から、両方のエンジンを今の設定へ同期する。
///
/// 呼び出し側は制御ロックを保持し、ジョブが無いことを確認してから呼ぶこと。
///
/// - 常駐の保持先 ([`crate::engine::resident_target`]) が止めない対象を決める
/// - 画面が見えている間はその画面が使うエンジンを止めない
/// - 稼働中のモデルが設定と違えば、暇なときに起こし直す (Main↔Floating の
///   保持先移行で旧エンジンを回収するのもこの経路)
fn apply_reconcile_plan(
    app: &AppHandle,
    state: &AppState,
    settings: &Settings,
    main_visible: bool,
    control: &EngineControlInner,
) {
    let search_root = crate::engine::exe_directory();
    let target = crate::engine::resident_target(settings, &search_root);
    let target_role = target.as_ref().map(|target| target.role);
    let serving = crate::engine::serving_role(EngineRole::Floating, settings, &search_root);
    let floating_shown = state.floating.is_shown();
    // 呼び出し側が制御ロック下でジョブ無しを確認済み。ここからロックを解放するまで
    // 新しいジョブは受け付けられない。
    let in_use = state.jobs.has_active_job();

    let main_model = crate::engine::resolved_model_path(EngineRole::Main, settings, &search_root);
    let floating_model =
        crate::engine::resolved_model_path(EngineRole::Floating, settings, &search_root);

    // メイン用: 常駐の保持先か、画面が見えている (共有ならフローティング表示中も)
    // の間は止めない。表示中のメインは止まっていれば起こす (再表示直後と同じ)。
    let main_action = decide_engine_sync(
        target_role == Some(EngineRole::Main) || main_visible,
        main_visible || (floating_shown && serving == EngineRole::Main),
        in_use,
        state.engine.is_starting(),
        state.engine.has_process(),
        running_model_matches(&state.engine, main_model.as_deref()),
        crate::engine::can_resolve(EngineRole::Main, settings, app),
    );

    // フローティング用: 常駐の保持先か、専用エンジンで表示中の間は止めない。
    let floating_action = decide_engine_sync(
        target_role == Some(EngineRole::Floating),
        floating_shown && serving == EngineRole::Floating,
        in_use,
        state.floating_engine.is_starting(),
        state.floating_engine.has_process(),
        running_model_matches(&state.floating_engine, floating_model.as_deref()),
        crate::engine::can_resolve(EngineRole::Floating, settings, app),
    );

    if target.is_some() {
        debug_log_resident(target.as_ref());
    }
    apply_engine_sync(app, &state.engine, main_action, settings, control);
    apply_engine_sync(
        app,
        &state.floating_engine,
        floating_action,
        settings,
        control,
    );
}

/// 常駐ポリシーの再同期を予約する。
///
/// 実行時に制御ロック下で「予約が最新か」「ジョブが始まっていないか」を再検証して
/// から適用する。ジョブ受け付けが先に起きた場合、この予約は破棄され、ジョブ完了
/// 時の再同期に委ねる。古い Restart/Stop が新しいジョブを巻き込まないための
/// 直列化 (H1)。
pub(crate) fn sync_engines_now(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let ticket = {
        let mut control = state.engine_control.lock();
        // 終了処理が始まっていたら、新しい同期予約は作らない。
        if !control.may_start() {
            return;
        }
        control.sync.schedule()
    };
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app_handle.try_state::<AppState>() else {
            return;
        };
        // 可視性はメインスレッドへの問い合わせを含むため、制御ロックの外で読む。
        // 読む前後で世代を確認し、その間に hide/show などの状態変更が入っていたら
        // このスナップショットは使わない (古い visible で起動しない: Medium 2)。
        let snapshot_epoch = {
            let control = state.engine_control.lock();
            if !control.sync.is_current(ticket) {
                return;
            }
            control.sync.epoch()
        };
        let main_visible = main_window_visible(&app_handle);
        let control = state.engine_control.lock();
        if !control.sync.is_current(ticket) || control.sync.epoch() != snapshot_epoch {
            return;
        }
        let in_use = state.jobs.has_active_job();
        if !control.may_apply(ticket, in_use) {
            return;
        }
        // 記録中 (suspend) の一時的な enabled=false を通常 OFF と混同しない。
        let settings = control.effective_settings(state.settings.get());
        apply_reconcile_plan(&app_handle, &state, &settings, main_visible, &control);
    });
}

/// メインウィンドウが見えているか。
///
/// メインスレッドへの問い合わせを含むため、制御ロックを取る前に呼ぶこと
/// (ロック保持中に呼ぶと、メインスレッドが同じロックを待ってデッドロックする)。
fn main_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window(floating::MAIN_LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

/// 明示的な起動要求 (再起動・復元・起動時)。
///
/// 古い停止/再起動予約より優先し、起動完了後にポリシーを再評価する (M1)。
pub(crate) fn start_engine_with_sync(app: &AppHandle, role: EngineRole, force: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let mut control = state.engine_control.lock();
    if !control.may_start() {
        return;
    }
    control.sync.invalidate();
    let settings = state.settings.get();
    let engine = state.engine_for(role).clone();
    let _ = reserve_and_spawn(app, &engine, &settings, force, &control);
}

/// 要求された役割の翻訳に使うエンジンを起こす (トレイの再表示・ホットキー)。
///
/// 実際に起こすのは `engine::serving_role` が決めたエンジン (解決モデルが同一なら
/// メイン用を共有する)。起動完了後にポリシーを再評価する (M1)。
pub(crate) fn start_engine_requested(app: &AppHandle, requested: EngineRole) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let mut control = state.engine_control.lock();
    if !control.may_start() {
        return;
    }
    control.sync.invalidate();
    let settings = state.settings.get();
    let role = crate::engine::serving_role(requested, &settings, &crate::engine::exe_directory());
    let engine = state.engine_for(role).clone();
    let _ = reserve_and_spawn(app, &engine, &settings, false, &control);
}

/// 常駐ポリシーの決定内容をログに残す (不具合調査用)。
///
/// リリースビルドでは黙らせる。フローティング側の `debug_log` と同じ条件で、
/// `NUKA_FLOATING_DEBUG=1` のときだけ出す。
fn debug_log_resident(target: Option<&crate::engine::ResidentTarget>) {
    if !(cfg!(debug_assertions) || std::env::var_os("NUKA_FLOATING_DEBUG").is_some()) {
        return;
    }
    // stderr が閉じられていてもパニックしない (書き込み失敗は無視する)。
    // E2E ではランチャーがパイプを閉じるため、eprintln! の失敗でプロセスが
    // 落ちる実害があった。
    let _ = match target {
        Some(target) => writeln!(
            std::io::stderr(),
            "[resident] keep {} ({}, {})",
            target.role.as_str(),
            target.reason.as_str(),
            target.model.display()
        ),
        None => writeln!(std::io::stderr(), "[resident] none (on-demand)"),
    };
}

/// 常駐ポリシーの遷移をエンジンへ反映する。
///
/// 判定は `engine::resident_target` の 1 か所だけで、保持先 (Main / Floating)・
/// 解決モデルパス・理由のどれかが変わったときだけ同期する。独立エンジンの有無
/// (bool) だけを見ると、共有モデルでの常駐 ON/OFF を見落とす。
///
/// - ON: 保持先のエンジンを起こす (隠れたメイン用も予熱する。起動中は触らない)
/// - OFF: 使っていなければ止める。使用中 (画面表示・翻訳中) は維持し、次の
///   [`sync_engines_now`] / メイン非表示で解放する
/// - Main↔Floating: 新しい保持先を起こし、旧エンジンは暇なら回収する
fn sync_resident_engine(app: &AppHandle, before: &Settings, after: &Settings) {
    let search_root = crate::engine::exe_directory();
    // ホットキー記録中の一時的な enabled=false を通常の OFF と混同しない。
    let before = effective_settings(app, before.clone());
    let after = effective_settings(app, after.clone());
    let was = crate::engine::resident_target(&before, &search_root);
    let now = crate::engine::resident_target(&after, &search_root);
    if was == now {
        return;
    }
    sync_engines_now(app);
}

#[tauri::command]
pub fn detect_language(text: String) -> Option<DetectedLanguage> {
    detect::detect_language(&text)
}

/// 翻訳を始めて jobId を返す。結果はイベントで流れる。
///
/// 引数は 1 つの `req` オブジェクトなので、フロントは
/// `invoke("translate", { req: { text, target_lang, ... } })` で呼ぶ。
///
/// どちらのエンジンで訳すかは呼び出し元のウィンドウのラベルから決め、
/// `engine::serving_role` が最終的なエンジンを選ぶ。解決モデルがメイン用と
/// 同一なら、フローティング翻訳もメイン用エンジンで処理する (二重ロード防止)。
///
/// ジョブの受け付けとエンジンの停止/再起動は制御ロックで直列化する (H1/M2)。
/// 受け付けたジョブは、その時点の設定で解決したモデルにエンジンを揃えてから
/// 実行する (連続ジョブで旧モデルのまま訳さない)。
#[tauri::command]
pub fn translate(
    app: AppHandle,
    state: State<'_, AppState>,
    webview: tauri::WebviewWindow,
    req: TranslateRequest,
) -> u32 {
    let origin = webview.label().to_string();
    let requested = EngineRole::from_label(&origin);
    // ここからジョブ開始まで、エンジンの停止/再起動の予約は実行できない。
    let (job_id, cancel, superseded, engine, settings) = {
        let mut control = state.engine_control.lock();
        // 予約済みの停止/再起動より、このジョブの受け付けを優先する。
        control.sync.invalidate();
        let settings = state.settings.get();
        let role =
            crate::engine::serving_role(requested, &settings, &crate::engine::exe_directory());
        // 前のジョブを先に中止にしてから、エンジンをこのジョブのモデルへ揃える。
        let (job_id, cancel, superseded) = state.jobs.begin(&origin);
        prepare_engine_for_job(&app, &state, role, &settings, &control);
        (
            job_id,
            cancel,
            superseded,
            state.engine_for(role).clone(),
            settings,
        )
    };
    if let Some(superseded) = superseded {
        if let Err(error) = app.emit(
            "translation:superseded",
            SupersededEvent { job_id: superseded },
        ) {
            eprintln!("translation:superseded を送れません: {error}");
        }
    }
    let jobs = state.jobs.clone();
    tauri::async_runtime::spawn(async move {
        run_translation(
            app,
            engine,
            jobs,
            TranslationJob {
                settings,
                origin,
                job_id,
                cancel,
            },
            req,
        )
        .await;
    });
    job_id
}

#[tauri::command]
pub fn cancel_translation(state: State<'_, AppState>, job_id: u32) {
    state.jobs.cancel(job_id);
}

// ---------------------------------------------------------------------------
// フローティングウィンドウ (Phase 1)
// ---------------------------------------------------------------------------

/// `set_floating_settings` が受ける差分。省略したフィールドは今の値を保つ。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FloatingSettingsPatch {
    pub enabled: Option<bool>,
    pub hotkey: Option<String>,
    /// フローティング用モデル (`installed_models` の file)。
    /// 空文字は「アクティブモデルと同じ」への変更として扱う。
    pub model: Option<String>,
    /// ホットキー記録のための一時的な無効化か。
    ///
    /// true のときは常駐エンジンの開始/停止をしない。記録のたびにモデルを
    /// 読み込み直すのを避けるため (本物の有効/無効は false で送る)。
    pub suspend: Option<bool>,
}

/// フローティング設定の差分がエンジンのライフサイクルに影響するか。
///
/// ホットキーだけの保存では常駐エンジンを触らない。ホットキー記録中は
/// `enabled=false` を一時保存しているため、記録後のホットキー保存で reconcile
/// すると常駐エンジンを止めてしまい、有効へ戻すときに再ロードが入る (M3)。
fn floating_settings_affect_engine(before: &Settings, after: &Settings) -> bool {
    before.floating_window.enabled != after.floating_window.enabled
        || before.floating_window.model != after.floating_window.model
}

/// この設定保存でエンジンの再同期を行うか。
///
/// ホットキー記録中の一時無効化 (suspend) と、エンジンに影響しない差分
/// (ホットキーのみ) では触らない。
fn should_sync_floating_engine(suspended: bool, affects_engine: bool) -> bool {
    !suspended && affects_engine
}

/// フローティングウィンドウの設定を保存する。
///
/// ホットキーはパース検証してから保存する。登録に失敗したとき (ほかのアプリと
/// 競合) は新キーの登録を旧キーへ戻したうえで `settings.floating.hotkeyConflict`
/// を返す。設定値そのものは保存済みなので、呼び出し側はエラーを表示しつつ
/// `get_settings` の値 (変更後) を画面へ反映すること。
///
/// 専用モデル・有効/無効の変更は常駐ポリシー (保持先・解決モデルパス) の変化として
/// エンジンへ反映する。翻訳中は旧エンジンで完了させ、完了後の再同期で新ポリシーへ
/// 移行する (起動・停止をここで直接競合させない)。
/// `suspend: true` はホットキー記録用の一時無効化で、常駐エンジンを触らない。
#[tauri::command]
pub async fn set_floating_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    patch: FloatingSettingsPatch,
) -> Result<Settings, LocalizedText> {
    let suspended = patch.suspend.unwrap_or(false);
    let normalized = match patch.hotkey.as_deref() {
        Some(value) => Some(
            floating::normalize_hotkey(value)
                .ok_or_else(|| floating::hotkey_invalid_error(value))?,
        ),
        None => None,
    };
    // `null` (未指定) は変更なし、空文字は「アクティブモデルと同じ」(None)。
    let model_patch: Option<Option<String>> = patch.model.as_deref().map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let before = state.settings.get();
    // 記録開始は保存より先に suspend を立てる。保存と同期の隙間で一時的な
    // enabled=false を通常の OFF と誤認しないため (M3)。
    if suspended {
        state
            .engine_control
            .begin_hotkey_suspend(before.floating_window.enabled);
    }
    let saved = match state.settings.update(|settings| {
        if let Some(enabled) = patch.enabled {
            settings.floating_window.enabled = enabled;
        }
        if let Some(hotkey) = &normalized {
            settings.floating_window.hotkey = hotkey.clone();
        }
        if let Some(model) = &model_patch {
            settings.floating_window.model = model.clone();
        }
        Ok(())
    }) {
        Ok(saved) => saved,
        Err(error) => {
            // 保存できなかったら、先に立てた suspend も戻す。
            if suspended {
                state.engine_control.end_hotkey_suspend();
            }
            let fallback = format!("設定を保存できませんでした: {error}");
            return Err(
                LocalizedText::new("settings.floating.saveFailed", fallback).param("detail", error)
            );
        }
    };

    // ホットキー記録中の一時無効化は「通常の OFF」ではない。記録前の enabled を
    // 覚えておき、記録中の同期 (起動完了・ジョブ完了) で常駐エンジンを止めない (M3)。
    if !suspended && patch.enabled.is_some() {
        // 記録終了後の有効化/明示的な無効化で解除する。
        state.engine_control.end_hotkey_suspend();
    }

    // 専用モデル・有効/無効の変更は、常駐ポリシー (保持先・解決モデルパス) を
    // 変える。ジョブ実行中は旧エンジンで完了させ、完了後の再同期が新ポリシーへ
    // 移行する。ホットキーだけの保存ではエンジンを触らない (M3)。
    if should_sync_floating_engine(suspended, floating_settings_affect_engine(&before, &saved)) {
        sync_engines_now(&app);
    }

    // ショートカットの登録はプラグインがメインスレッドの処理完了を待つ。
    // メインスレッドで動くコマンドから直接呼ぶと固まるので、ブロッキング
    // スレッドへ逃がす。
    let app_handle = app.clone();
    let floating_state = state.floating.clone();
    let registration = tauri::async_runtime::spawn_blocking(move || {
        floating::apply_hotkey(&app_handle, &floating_state)
    })
    .await;

    match registration {
        Ok(Ok(())) => Ok(saved),
        Ok(Err(error)) => Err(error),
        Err(error) => {
            let message = format!("ホットキーを登録できません: {error}");
            Err(LocalizedText::new("error.raw", message.clone()).param("detail", message))
        }
    }
}

/// フローティングウィンドウを隠し、進行中のフローティング翻訳を止める。
///
/// `origin` は呼び出し元の識別子 ("close-button" / "escape" / "auto-hide")。
/// デバッグログで hide の経路を特定するためだけに使い、未知の値は unknown 扱い。
#[tauri::command]
pub fn hide_floating(app: AppHandle, origin: Option<String>) {
    floating::hide(&app, origin.as_deref().unwrap_or("unknown"));
}

/// メイン画面の訳先言語をフローティングウィンドウへ知らせる。
/// フローティングの初期訳先に使うだけで、保存はしない。
#[tauri::command]
pub fn set_main_target_lang(state: State<'_, AppState>, code: String) {
    state.floating.set_target_lang(&code);
}

// ---------------------------------------------------------------------------
// 設定画面: カタログとインストール
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_catalog() -> Vec<crate::catalog::CatalogEntry> {
    crate::catalog::entries()
}

#[tauri::command]
pub fn get_setup_state(app: AppHandle, state: State<'_, AppState>) -> crate::installer::SetupState {
    crate::installer::setup_state(&app, &state.settings)
}

/// ランタイム (llama.cpp の 2 zip) をダウンロードして展開する。
#[tauri::command]
pub async fn install_runtime(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), crate::message::LocalizedText> {
    let settings = state.settings.clone();
    let downloads = state.downloads.clone();
    let engine = state.engine.clone();
    let floating_engine = state.floating_engine.clone();
    // 実行中の llama-server.exe は Windows でロックされるため、常駐中の
    // フローティング用も先に止める (installer 側がメイン用を止める)。
    // 起動処理の途中でも止めれば、あとで起こし直す対象として扱う (L-2)。
    floating_engine.kill_now();
    let result = crate::installer::install_runtime(&app, &settings, &downloads, &engine).await;
    // install_runtime はエンジンを直接起こさず、常駐/表示ポリシーに従って復元する
    // (M1: 旧実装は installer が main を直接 start し、フローティングの復元も
    // ポリシーを見ずに直接 start していた)。失敗時も既存ランタイムで同じ経路。
    sync_engines_now(&app);
    result.map(|_| ())
}

/// カタログの 1 件を models_dir へダウンロードする。
#[tauri::command]
pub async fn install_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::settings::InstalledModel, crate::message::LocalizedText> {
    let settings = state.settings.clone();
    let downloads = state.downloads.clone();
    crate::installer::install_model(&app, &settings, &downloads, &id).await
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>, id: String) {
    let _ = state.downloads.cancel(&id);
}

/// インストール済みモデルを削除する。
///
/// フローティング専用モデルを削除するときは、Windows のファイルロックを外すため
/// フローティング用エンジンを止めてから削除し、成功したら `floating_window.model`
/// をクリアする (M-4)。削除に失敗したときは、常駐設定が成立するなら起こし直す。
///
/// `#[tauri::command(async)]` は必須。この中で最大 3 秒 (`kill_and_wait`) 待つため、
/// メインスレッド (イベントループ) で実行すると UI とトレイが固まる。`async fn` に
/// すると `std::thread::sleep` が tokio ワーカーを塞ぐので、属性で別スレッド実行に
/// する (監査 N-1)。
#[tauri::command(async)]
pub fn delete_model(
    app: AppHandle,
    state: State<'_, AppState>,
    file: String,
) -> Result<Settings, crate::message::LocalizedText> {
    let before = state.settings.get();
    let is_floating_model = before.floating_window.model.as_deref() == Some(file.as_str());
    // 実際に削除されうるものだけエンジンを止める (使用中モデル・未登録モデルは
    // installer が先にエラーにするため、止める必要がない)。
    let can_delete = before
        .installed_models
        .iter()
        .any(|model| model.file == file)
        && before.active_model.as_deref() != Some(file.as_str());
    let floating_engine = state.floating_engine.clone();
    let killed = is_floating_model && can_delete;
    if killed {
        // GGUF を開いているプロセスの終了を待ってから削除する。
        floating_engine.kill_and_wait(std::time::Duration::from_secs(3));
    }

    let result = crate::installer::delete_model(&state.settings, &file);
    match result {
        Ok(_) if is_floating_model => {
            // 削除したモデルを指したままにしない (次はアクティブモデルで解決)。
            let saved = state
                .settings
                .update(|settings| {
                    if settings.floating_window.model.as_deref() == Some(file.as_str()) {
                        settings.floating_window.model = None;
                    }
                    Ok(())
                })
                .map_err(crate::message::LocalizedText::from)?;
            // predicate は false になったので常駐はしない。
            Ok(saved)
        }
        Ok(settings) => Ok(settings),
        Err(error) => {
            // 削除できなかった (ウイルス対策ソフト等のロック)。止めた分を戻す。
            // 専用モデルが欠落したまま常駐を再開しないよう、ポリシーの保持先が
            // フローティング用のときだけ起こす。
            let current = state.settings.get();
            let current = effective_settings(&app, current);
            let restore = crate::engine::resident_target(&current, &crate::engine::exe_directory())
                .is_some_and(|target| target.role == EngineRole::Floating);
            if killed && restore {
                // 常駐の復元も、起動完了後の再同期を通る統一経路で行う (M1)。
                start_engine_with_sync(&app, EngineRole::Floating, false);
            }
            Err(error)
        }
    }
}

/// 使用するモデルを切り替え、エンジンを今の設定へ同期する。
///
/// アクティブモデルはメイン用エンジンのモデルであり、共有している常駐
/// (フローティング用に同一ファイルを選んだ場合) のモデルでもある。ここでは
/// エンジンを直接起こし直さず、常駐ポリシーの再同期に任せる。翻訳中は旧モデルの
/// エンジンで完了させ、完了後の [`sync_engines_now`] が新しいモデルへ移行する
/// (ジョブを巻き込んで中断しない一貫した方針)。専用モデルが別ファイルなら、
/// アクティブモデルを変えてもフローティング用の解決パスは変わらないので触らない
/// (L-3)。
#[tauri::command]
pub fn set_active_model(
    app: AppHandle,
    state: State<'_, AppState>,
    file: String,
) -> Result<Settings, crate::message::LocalizedText> {
    let settings = crate::installer::set_active_model(&state.settings, &file)?;
    sync_engines_now(&app);
    Ok(settings)
}

/// モデルの保存先を変更する (空文字なら既定の場所)。
#[tauri::command]
pub fn set_models_dir(
    state: State<'_, AppState>,
    path: String,
) -> Result<Settings, crate::message::LocalizedText> {
    crate::installer::set_models_dir(&state.settings, &path)
}

// ---------------------------------------------------------------------------
// アップデート
// ---------------------------------------------------------------------------

/// GitHub の最新リリースを確認する。失敗も `error` フィールドで返す。
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::updater::UpdateCheck, String> {
    let updates = state.updates.clone();
    Ok(crate::updater::check(&app, &updates).await)
}

/// 確認済みの更新インストーラーをダウンロードする。
#[tauri::command]
pub async fn download_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), crate::message::LocalizedText> {
    let updates = state.updates.clone();
    let downloads = state.downloads.clone();
    crate::updater::download(&app, &updates, &downloads).await
}

#[tauri::command]
pub fn cancel_update_download(state: State<'_, AppState>) {
    let _ = crate::updater::cancel(&state.downloads);
}

/// ダウンロード済みのインストーラーを起動し、このアプリを終了する。
#[tauri::command]
pub fn install_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), crate::message::LocalizedText> {
    let engine = state.engine.clone();
    let floating_engine = state.floating_engine.clone();
    // 検証・spawn が成功したときだけ updater 側がフローティング用も止める (L-1)。
    crate::updater::install(&app, &state.updates, &engine, &floating_engine)
}

// ---------------------------------------------------------------------------
// ジョブの実行
// ---------------------------------------------------------------------------

struct JobSuccess {
    text: String,
    tokens: Option<u32>,
}

/// ジョブの失敗。`key` があれば UI が表示言語に合わせて訳す (無ければ日本語のまま)。
#[derive(Debug, PartialEq, Eq)]
struct JobError {
    message: String,
    key: Option<&'static str>,
}

impl JobError {
    fn raw(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            key: None,
        }
    }

    /// 辞書にもある文言。`message` は日本語フォールバック。
    fn keyed(key: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            key: Some(key),
        }
    }
}

impl From<String> for JobError {
    fn from(message: String) -> Self {
        Self::raw(message)
    }
}

/// `run_translation` に渡す、受付時に確定したジョブの実行情報。
struct TranslationJob {
    settings: Settings,
    origin: String,
    job_id: u32,
    cancel: Arc<AtomicBool>,
}

async fn run_translation(
    app: AppHandle,
    engine: Engine,
    jobs: JobManager,
    job: TranslationJob,
    req: TranslateRequest,
) {
    let TranslationJob {
        settings,
        origin,
        job_id,
        cancel,
    } = job;
    // 受付時に制御ロック下で起動予約まで確定している。ここでは起動せず、待つだけ
    // (取消済み J1 が制御ロック外で新しい起動を奪うのを防ぐ: Medium 1)。
    // 停止・異常終了からの復旧だけは、job id・取消・希望モデルを再検証してから
    // 共通の予約経路で起こす。
    let ready = wait_for_job_engine(&app, &engine, &settings, &origin, job_id, &cancel).await;
    let outcome = match ready {
        // 待機中に閉じられた。翻訳は始めない (done も error も流さない)。
        EngineReady::Cancelled => {
            jobs.finish(job_id);
            // 中止も「ジョブ終了」。常駐 OFF / モデル変更の再同期を行う。
            sync_engines_now(&app);
            return;
        }
        EngineReady::Ready(port) => {
            translate_job(&app, &engine, Some(port), job_id, &cancel, &req).await
        }
        EngineReady::Unavailable => translate_job(&app, &engine, None, job_id, &cancel, &req).await,
    };
    jobs.finish(job_id);

    match outcome {
        Ok(Some(success)) => {
            let event = DoneEvent {
                job_id,
                text: success.text,
                tokens: success.tokens,
            };
            if let Err(error) = app.emit("translation:done", event) {
                eprintln!("translation:done を送れません: {error}");
            }
        }
        // 中止された。done も error も流さない (呼んだ側が知っている)。
        Ok(None) => {}
        Err(error) => {
            if !is_cancelled(&cancel) {
                let event = ErrorEvent {
                    job_id,
                    message: error.message,
                    message_key: error.key.map(str::to_string),
                };
                if let Err(error) = app.emit("translation:error", event) {
                    eprintln!("translation:error を送れません: {error}");
                }
            }
        }
    }

    // ジョブ中に常駐 OFF・モデル変更が起きた場合、ここで最新ポリシーへ移行する
    // (hide 待ちにしない)。成功・失敗・取消のどの終了経路でも同じ。
    sync_engines_now(&app);
}

/// 受付済みジョブのエンジン準備。
///
/// 受付時に制御ロック下で起動予約まで確定しているので、ここでは**起動しない**
/// (取消済みジョブが制御ロック外で新世代の起動を奪い、後続ジョブを旧モデルで
/// 走らせる事故の防止: Medium 1)。停止・異常終了からの復旧だけ、制御ロック下で
/// job id・取消・ルーティング・起動状態を再検証して共通の予約経路で起こす。
async fn wait_for_job_engine(
    app: &AppHandle,
    engine: &Engine,
    settings: &Settings,
    origin: &str,
    job_id: u32,
    cancel: &AtomicBool,
) -> EngineReady {
    let ready = engine.wait_until_ready(ENGINE_READY_LIMIT, cancel).await;
    if !matches!(ready, EngineReady::Unavailable) || is_cancelled(cancel) {
        return ready;
    }
    if !retry_job_engine_start(app, engine, settings, origin, job_id, cancel) {
        return EngineReady::Unavailable;
    }
    engine.wait_until_ready(ENGINE_READY_LIMIT, cancel).await
}

/// 停止・異常終了したエンジンの復旧起動を予約できたら true。
///
/// 取消済み・差し替え済み・起動中・シャットダウン中・ルーティング変更では
/// 起動しない。予約した起動には完了後の再同期が付く (M1)。
fn retry_job_engine_start(
    app: &AppHandle,
    engine: &Engine,
    settings: &Settings,
    origin: &str,
    job_id: u32,
    cancel: &AtomicBool,
) -> bool {
    let Some(state) = app.try_state::<AppState>() else {
        return false;
    };
    // 速い経路: まだ読み込み中なら再起動しない (待ち時間を延ばさない)。
    if engine.is_starting() {
        return false;
    }
    let control = state.engine_control.lock();
    let latest = state.settings.get();
    let requested = EngineRole::from_label(origin);
    let role_now = crate::engine::serving_role(requested, &latest, &crate::engine::exe_directory());
    let cancelled = is_cancelled(cancel);
    let current_job = state.jobs.current_job_id() == Some(job_id);
    let starting = engine.is_starting();
    let running = engine.has_process();
    let phase = engine.status().state;
    if role_now != engine.role()
        || !may_retry_job_start(
            current_job,
            cancelled,
            starting,
            running,
            phase,
            control.shutting_down,
        )
    {
        return false;
    }
    // 予約できた時点で true。spawn は別ワーカーで走るため、`is_starting()` を
    // 再読みすると、spawn が先に完了した場合に「起動していない」と誤判定して
    // readyTimeout でジョブを失敗させてしまう (Minor)。
    reserve_and_spawn(app, engine, settings, false, &control)
}

/// 復旧起動を試みてよいか (純粋判定)。
///
/// - 取消済み・差し替え済み・起動処理中・シャットダウン中は不可
/// - 稼働中は、クラッシュ (`error`) の後片付け以外では再起動しない
fn may_retry_job_start(
    current_job: bool,
    cancelled: bool,
    starting: bool,
    running: bool,
    phase: EnginePhase,
    shutting_down: bool,
) -> bool {
    if shutting_down || cancelled || !current_job || starting {
        return false;
    }
    !running || phase == EnginePhase::Error
}

async fn translate_job(
    app: &AppHandle,
    engine: &Engine,
    port: Option<u16>,
    job_id: u32,
    cancel: &AtomicBool,
    req: &TranslateRequest,
) -> Result<Option<JobSuccess>, JobError> {
    // 原文は常に 400 文字前後へ割ってから直列に訳す。
    let chunks = prompt::split_text(&req.text, CHUNK_LIMIT);
    let Some(first) = chunks.first() else {
        return Err(JobError::raw(PromptError::EmptyText.to_string()));
    };
    let source_lang = req.source_lang.as_deref();
    let form = req.form.as_deref();

    // 進捗を流す前に、訳先・文体・原文の不備を弾く。
    prompt::build_prompt(&PromptOptions {
        text: first,
        target_lang: &req.target_lang,
        source_lang,
        context: None,
        glossary: &req.glossary,
        form,
        history: &[],
    })
    .map_err(|error| error.to_string())?;

    let Some(port) = port else {
        let status = engine.status();
        return Err(match status.state {
            EnginePhase::Error => JobError::raw(format!(
                "エンジンを起動できていません。{}",
                status
                    .detail
                    .map(|detail| detail.fallback)
                    .unwrap_or_default()
            )),
            EnginePhase::Ready => {
                JobError::raw("エンジンの待ち受けポートがありません。再起動してください。")
            }
            // 待っても準備できなかった / 途中で止められた。待機上限は
            // ENGINE_READY_LIMIT (engine.rs) の 1 か所で決めている。
            _ => JobError::keyed(
                "engine.readyTimeout",
                "エンジンの準備ができませんでした。もう一度お試しください。",
            ),
        });
    };

    let total = chunks.len();
    emit(
        app,
        "translation:progress",
        ProgressEvent {
            job_id,
            done: 0,
            total,
        },
    )?;

    let mut history: Vec<HistoryPair> = Vec::new();
    let mut parts: Vec<String> = Vec::with_capacity(total);
    let mut tokens: Option<u32> = None;

    for (index, chunk) in chunks.iter().enumerate() {
        if is_cancelled(cancel) {
            return Ok(None);
        }

        let built = prompt::build_prompt(&PromptOptions {
            text: chunk,
            target_lang: &req.target_lang,
            source_lang,
            context: None,
            glossary: &req.glossary,
            form,
            history: &history,
        })
        .map_err(|error| error.to_string())?;

        // 途中経過は「まだ送っていない差分」だけを流す。制御トークンが混ざっても
        // clean_translation の結果だけを送るので、画面に区切りは出ない。
        // 2 チャンク目以降は join_translations と同じ区切りを最初の差分に 1 回だけ前置する。
        // これが無いと done で全文が届くまでチャンクの境目が詰まって見える。
        let separator = if index > 0 {
            prompt::translation_separator(&req.target_lang)
        } else {
            ""
        };
        let mut emitted = String::new();
        let mut separator_pending = !separator.is_empty();
        let outcome = stream_completion(engine.http(), port, &built, cancel, |raw| {
            let visible = prompt::clean_translation(raw);
            if let Some(delta) =
                take_visible_delta(&visible, &emitted, separator, &mut separator_pending)
            {
                let _ = app.emit("translation:chunk", ChunkEvent { job_id, delta });
            }
            emitted = visible;
        })
        .await?;

        let StreamOutcome::Completed {
            text: raw,
            tokens: chunk_tokens,
        } = outcome
        else {
            return Ok(None);
        };

        let cleaned = prompt::clean_translation(&raw);
        if cleaned.is_empty() {
            return Err(JobError::raw("モデルが訳文を返しませんでした。"));
        }

        parts.push(cleaned.clone());
        history.push(HistoryPair {
            source: chunk.clone(),
            translation: cleaned,
        });
        if history.len() > HISTORY_LIMIT {
            history.remove(0);
        }
        tokens = chunk_tokens.or(tokens);

        emit(
            app,
            "translation:progress",
            ProgressEvent {
                job_id,
                done: index + 1,
                total,
            },
        )?;
    }

    let text = prompt::join_translations(&parts, &req.target_lang);
    Ok(Some(JobSuccess { text, tokens }))
}

fn emit<T>(app: &AppHandle, event: &str, payload: T) -> Result<(), String>
where
    T: Serialize + Clone,
{
    app.emit(event, payload)
        .map_err(|error| format!("{event} を送れません: {error}"))
}

fn is_cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::SeqCst)
}

/// 生成途中の可視テキストから、まだ送っていない差分を取り出す。
///
/// `separator` はチャンクの頭で 1 回だけ前置する区切りで、`join_translations` と同じ規則。
/// まだ文字が出ていない間は区切りを消費しない。前提が崩れた (可視テキストが縮んだ) ら
/// `None` を返し、呼び出し側が基準を取り直す。
fn take_visible_delta(
    visible: &str,
    emitted: &str,
    separator: &str,
    separator_pending: &mut bool,
) -> Option<String> {
    if visible.len() <= emitted.len() || !visible.starts_with(emitted) {
        return None;
    }
    let mut delta = visible[emitted.len()..].to_string();
    if *separator_pending {
        *separator_pending = false;
        delta.insert_str(0, separator);
    }
    Some(delta)
}

// ---------------------------------------------------------------------------
// llama-server とのやりとり
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum StreamOutcome {
    Completed { text: String, tokens: Option<u32> },
    Cancelled,
}

/// `/v1/completions` を `stream: true` で叩き、SSE を手で読む。
///
/// `on_progress` には「ここまでに受け取った生テキスト全体」を渡す。
async fn stream_completion<F>(
    client: &reqwest::Client,
    port: u16,
    prompt_text: &str,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<StreamOutcome, String>
where
    F: FnMut(&str),
{
    let body = serde_json::json!({
        "model": SERVER_ALIAS,
        "prompt": prompt_text,
        "temperature": 0,
        "max_tokens": MAX_TOKENS,
        "stop": STOP_TOKENS,
        "stream": true,
    });
    let url = format!("http://127.0.0.1:{port}/v1/completions");
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("llama-server に届きません: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        // エラー本文は全文を読まず、先頭だけを見て捨てる。
        let detail = read_limited_body(response, MAX_ERROR_BODY_BYTES).await;
        return Err(format!(
            "llama-server が {status} を返しました。{}",
            truncate(&detail, 300)
        ));
    }

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut raw = String::new();
    let mut tokens: Option<u32> = None;
    let mut finished = false;

    loop {
        if is_cancelled(cancel) {
            return Ok(StreamOutcome::Cancelled);
        }
        let item = match timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
            Err(_) => return Err("llama-server の応答が止まりました。".to_string()),
            Ok(None) => break,
            Ok(Some(Err(error))) => {
                return Err(format!("llama-server の応答を読めません: {error}"))
            }
            Ok(Some(Ok(item))) => item,
        };
        buffer.extend_from_slice(&item);

        // SSE は行単位。UTF-8 の途中で切れることはない (改行はマルチバイトの内側に出ない)。
        while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=position).collect();
            let line = decode_line(&line[..line.len() - 1]);
            match handle_sse_line(&line, &mut raw, &mut tokens) {
                Ok(SseStep::Continue) => {}
                Ok(SseStep::Done) => {
                    finished = true;
                    break;
                }
                Err(message) => return Err(message),
            }
        }
        if finished {
            break;
        }
        // 改行が来ないまま溜まり続ける行と、伸び続ける訳文を打ち切る。
        if buffer.len() > MAX_SSE_LINE_BYTES {
            return Err("llama-server の応答が長すぎます。".to_string());
        }
        on_progress(&raw);
    }

    // 改行で終わらない最後の 1 行を拾う。
    if !finished && !buffer.is_empty() {
        if buffer.len() > MAX_SSE_LINE_BYTES {
            return Err("llama-server の応答が長すぎます。".to_string());
        }
        let line = decode_line(&buffer);
        handle_sse_line(&line, &mut raw, &mut tokens)?;
    }
    on_progress(&raw);

    Ok(StreamOutcome::Completed { text: raw, tokens })
}

/// 応答本文の先頭 `limit` バイトだけを読む。エラー応答を丸ごとメモリに載せないため。
async fn read_limited_body(response: reqwest::Response, limit: usize) -> String {
    let mut stream = response.bytes_stream();
    let mut collected: Vec<u8> = Vec::with_capacity(limit.min(1024));
    while collected.len() < limit {
        match stream.next().await {
            Some(Ok(chunk)) => {
                let remaining = limit - collected.len();
                let take = chunk.len().min(remaining);
                collected.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    break;
                }
            }
            Some(Err(_)) | None => break,
        }
    }
    String::from_utf8_lossy(&collected).into_owned()
}

#[derive(Debug, PartialEq, Eq)]
enum SseStep {
    Continue,
    Done,
}

fn decode_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\r')
        .to_string()
}

fn handle_sse_line(
    line: &str,
    raw: &mut String,
    tokens: &mut Option<u32>,
) -> Result<SseStep, String> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(SseStep::Continue);
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(SseStep::Continue);
    }
    if data == "[DONE]" {
        return Ok(SseStep::Done);
    }
    let value: serde_json::Value = match serde_json::from_str(data) {
        Ok(value) => value,
        // 壊れた行は捨てて次の行に進む (ストリーム全体を落とさない)。
        Err(_) => return Ok(SseStep::Continue),
    };

    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(|message| message.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        return Err(format!(
            "llama-server がエラーを返しました: {}",
            truncate(&message, 300)
        ));
    }

    if let Some(text) = value
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("text"))
        .and_then(|text| text.as_str())
    {
        raw.push_str(text);
        if raw.len() > MAX_TRANSLATION_BYTES {
            return Err("訳文が長すぎます。".to_string());
        }
    }

    if let Some(count) = value
        .get("usage")
        .and_then(|usage| usage.get("completion_tokens"))
        .and_then(serde_json::Value::as_u64)
    {
        *tokens = Some(count as u32);
    }
    if let Some(count) = value
        .get("timings")
        .and_then(|timings| timings.get("predicted_n"))
        .and_then(serde_json::Value::as_u64)
    {
        *tokens = Some(count as u32);
    }

    Ok(SseStep::Continue)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut result: String = text.chars().take(limit).collect();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(line: &str) -> (Result<SseStep, String>, String, Option<u32>) {
        let mut raw = String::new();
        let mut tokens = None;
        let step = handle_sse_line(line, &mut raw, &mut tokens);
        (step, raw, tokens)
    }

    #[test]
    fn sse_data_line_appends_the_delta() {
        let (step, raw, tokens) = collect(
            "data: {\"choices\":[{\"text\":\"プロジェクトの\",\"index\":0,\"finish_reason\":null}]}",
        );
        assert_eq!(step.unwrap(), SseStep::Continue);
        assert_eq!(raw, "プロジェクトの");
        assert_eq!(tokens, None);
    }

    #[test]
    fn sse_done_marker_ends_the_stream() {
        let (step, raw, _) = collect("data: [DONE]");
        assert_eq!(step.unwrap(), SseStep::Done);
        assert_eq!(raw, "");
    }

    #[test]
    fn sse_non_data_lines_are_ignored() {
        assert_eq!(collect(": keep-alive").0.unwrap(), SseStep::Continue);
        assert_eq!(collect("event: message").0.unwrap(), SseStep::Continue);
        assert_eq!(collect("").0.unwrap(), SseStep::Continue);
    }

    #[test]
    fn sse_error_payload_is_reported() {
        let (step, _, _) = collect("data: {\"error\":{\"message\":\"model not loaded\"}}");
        let message = step.unwrap_err();
        assert!(message.contains("model not loaded"));
    }

    #[test]
    fn sse_records_token_counts_from_usage_and_timings() {
        let (_, _, tokens) = collect("data: {\"choices\":[],\"usage\":{\"completion_tokens\":6}}");
        assert_eq!(tokens, Some(6));

        let (_, _, tokens) = collect("data: {\"choices\":[],\"timings\":{\"predicted_n\":11}}");
        assert_eq!(tokens, Some(11));
    }

    #[test]
    fn malformed_json_is_skipped() {
        assert_eq!(collect("data: {oops").0.unwrap(), SseStep::Continue);
    }

    #[test]
    fn crlf_lines_are_decoded() {
        assert_eq!(decode_line(b"data: {\"a\":1}\r"), "data: {\"a\":1}");
    }

    #[test]
    fn truncate_keeps_the_text_short() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }

    /// 指定したステータスと本文を返すだけの小さなサーバ。
    async fn spawn_stub_response(status: u16, reason: &str, body: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("stub を bind できる");
        let port = listener.local_addr().unwrap().port();
        let body = body.to_string();
        let reason = reason.to_string();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut temp = [0u8; 4096];
            let _ = socket.read(&mut temp).await;
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        port
    }

    #[tokio::test]
    async fn error_body_is_read_up_to_the_limit() {
        let body = "x".repeat(64 * 1024);
        let port = spawn_stub_response(500, "Internal Server Error", &body).await;
        let client = reqwest::Client::new();
        let response = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect("stub に届く");

        let read = read_limited_body(response, 1024).await;
        assert_eq!(read.len(), 1024);
        assert!(read.chars().all(|character| character == 'x'));
    }

    #[tokio::test]
    async fn oversized_sse_line_is_rejected() {
        // 改行が来ないまま上限を超える 1 行。
        let huge = format!("data: {}", "a".repeat(MAX_SSE_LINE_BYTES + 1024));
        let port = spawn_stub_completions(vec![huge]).await;
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(false);

        let error = stream_completion(&client, port, "prompt", &cancel, |_| {})
            .await
            .expect_err("上限で打ち切る");
        assert!(error.contains("長すぎます"));
    }

    #[tokio::test]
    async fn oversized_translation_is_rejected() {
        // 1 行は上限内でも、累積が 1 MiB を超えたら打ち切る。
        let text = "あ".repeat(4000);
        let mut chunks: Vec<String> = Vec::new();
        for _ in 0..90 {
            chunks.push(format!(
                "data: {{\"choices\":[{{\"text\":\"{text}\"}}]}}\n\n"
            ));
        }
        chunks.push("data: [DONE]\n\n".to_string());
        let port = spawn_stub_completions(chunks).await;
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(false);

        let error = stream_completion(&client, port, "prompt", &cancel, |_| {})
            .await
            .expect_err("累積上限で打ち切る");
        assert!(error.contains("訳文が長すぎます"));
    }

    #[test]
    fn later_chunk_deltas_get_the_join_separator_once() {
        let mut pending = true;
        let mut emitted = String::new();

        // まだ文字が出ていない間は区切りを消費しない
        assert_eq!(take_visible_delta("", &emitted, " ", &mut pending), None);
        assert!(pending);

        // 最初の実テキストにだけ区切りが付く
        assert_eq!(
            take_visible_delta("Hello", &emitted, " ", &mut pending),
            Some(" Hello".to_string())
        );
        assert!(!pending);
        emitted = "Hello".to_string();

        // 2 回目以降は区切りを繰り返さない
        assert_eq!(
            take_visible_delta("Hello world", &emitted, " ", &mut pending),
            Some(" world".to_string())
        );
    }

    #[test]
    fn cjk_targets_join_without_a_separator() {
        for target in ["ja", "zh-hans", "zh-hant"] {
            assert_eq!(prompt::translation_separator(target), "");
            let mut pending = false;
            let delta = take_visible_delta("あい", "", "", &mut pending);
            assert_eq!(delta, Some("あい".to_string()));
        }
        for target in ["en", "ko", "fr"] {
            assert_eq!(prompt::translation_separator(target), " ");
            let mut pending = true;
            let delta = take_visible_delta("abc", "", " ", &mut pending);
            assert_eq!(delta, Some(" abc".to_string()));
        }
    }

    #[test]
    fn take_visible_delta_ignores_text_that_did_not_grow() {
        let mut pending = false;
        assert_eq!(take_visible_delta("same", "same", " ", &mut pending), None);
        assert_eq!(
            take_visible_delta("短い", "長いテキスト", " ", &mut pending),
            None
        );
    }

    #[test]
    fn job_manager_supersedes_the_previous_job() {
        let jobs = JobManager::new();
        let (first, first_cancel, none) = jobs.begin("main");
        assert_eq!(none, None);
        let (second, second_cancel, superseded) = jobs.begin("floating");
        assert_ne!(first, second);
        assert_eq!(superseded, Some(first));
        assert!(first_cancel.load(Ordering::SeqCst));
        assert!(!second_cancel.load(Ordering::SeqCst));

        // 古い jobId の中止要求は新しいジョブに効かない。
        jobs.cancel(first);
        assert!(!second_cancel.load(Ordering::SeqCst));
        jobs.cancel(second);
        assert!(second_cancel.load(Ordering::SeqCst));

        jobs.finish(second);
        assert!(jobs.lock().is_none());
    }

    #[test]
    fn job_ids_start_at_one() {
        let jobs = JobManager::new();
        let (id, _, _) = jobs.begin("main");
        assert_eq!(id, 1);
    }

    #[test]
    fn jobs_can_be_cancelled_by_origin_without_touching_the_other_window() {
        let jobs = JobManager::new();
        let (id, cancel, _) = jobs.begin("main");
        assert_eq!(jobs.current_job_id(), Some(id));

        // フローティングを閉じてもメインの翻訳は止まらない。
        jobs.cancel_current_for_origin("floating");
        assert!(!cancel.load(Ordering::SeqCst));

        jobs.cancel_current_for_origin("main");
        assert!(cancel.load(Ordering::SeqCst));

        jobs.finish(id);
        assert_eq!(jobs.current_job_id(), None);
    }

    #[test]
    fn active_job_ignores_cancelled_ones() {
        // 中止フラグの立ったジョブは「エンジンを使い終える」まで待たない。
        // 常駐 OFF の再同期が、取消済みジョブで止まらないための判定。
        let jobs = JobManager::new();
        assert!(!jobs.has_active_job());
        let (id, cancel, _) = jobs.begin(floating::FLOATING_LABEL);
        assert!(jobs.has_active_job());
        jobs.cancel_current_for_origin(floating::FLOATING_LABEL);
        assert!(cancel.load(Ordering::SeqCst));
        assert!(!jobs.has_active_job());
        jobs.finish(id);
        assert!(!jobs.has_active_job());
    }

    #[test]
    fn engine_sync_keeps_a_busy_engine_and_releases_it_afterward() {
        use EngineSyncAction::{Keep, Restart, Start, Stop};

        // (b) 起動中に常駐 OFF / hide: 保持理由が無ければ起動を取り消す。
        assert_eq!(
            decide_engine_sync(false, false, false, true, false, true, true),
            Stop
        );
        // メイン表示中や常駐の保持先なら、起動中の子は完了まで待つ。
        assert_eq!(
            decide_engine_sync(true, false, false, true, false, true, true),
            Keep
        );
        assert_eq!(
            decide_engine_sync(false, true, false, true, false, true, true),
            Keep
        );

        // 常駐 ON への切替: 停止中なら起こす。モデル未解決なら起こさない。
        assert_eq!(
            decide_engine_sync(true, false, false, false, false, true, true),
            Start
        );
        assert_eq!(
            decide_engine_sync(true, false, false, false, false, true, false),
            Keep
        );

        // 稼働中: 保持先ならモデル一致で継続、違えば起こし直す。
        assert_eq!(
            decide_engine_sync(true, false, false, false, true, true, true),
            Keep
        );
        assert_eq!(
            decide_engine_sync(true, false, false, false, true, false, true),
            Restart
        );
        // 保持先でない (常駐 OFF / Main↔Floating 移行の旧エンジン) なら止める。
        assert_eq!(
            decide_engine_sync(false, false, false, false, true, true, true),
            Stop
        );
        // 表示中は止めない。
        assert_eq!(
            decide_engine_sync(false, true, false, false, true, true, true),
            Keep
        );

        // (d) ジョブ中に OFF にされた: 完了までは維持し、完了後に解放する。
        assert_eq!(
            decide_engine_sync(false, false, true, false, true, false, true),
            Keep
        );
        assert_eq!(
            decide_engine_sync(false, false, false, false, true, false, true),
            Stop
        );
        // ジョブ中にモデルが変わっても中断しない (完了後に Restart)。
        assert_eq!(
            decide_engine_sync(true, false, true, false, true, false, true),
            Keep
        );
    }

    #[test]
    fn on_off_toggles_are_stateless_and_idempotent() {
        use EngineSyncAction::{Keep, Start, Stop};

        // (c) ON/OFF 連打: 途中の遷移を積み上げず、その時点のポリシーだけで
        // 判断する。稼働中のエンジンは ON で維持、OFF で停止。
        for _ in 0..3 {
            assert_eq!(
                decide_engine_sync(true, false, false, false, true, true, true),
                Keep
            );
            assert_eq!(
                decide_engine_sync(false, false, false, false, true, true, true),
                Stop
            );
        }
        // 停止中のエンジンは ON で起こし、OFF では何もしない。
        for _ in 0..3 {
            assert_eq!(
                decide_engine_sync(true, false, false, false, false, true, true),
                Start
            );
            assert_eq!(
                decide_engine_sync(false, false, false, false, false, true, true),
                Keep
            );
        }
    }

    #[test]
    fn a_scheduled_sync_is_released_when_a_job_begins() {
        // H1: idle を観測して予約した Restart/Stop は、実行前にジョブが始まったら
        // 破棄する (実行順が逆なら、予約が先に適用されてから受け付ける)。
        let mut sync = SyncEpoch::default();

        // 予約 → まだジョブが無いので適用できる。
        let ticket = sync.schedule();
        assert!(sync.may_apply(ticket, false));

        // ジョブ受け付けは古い予約を無効化する (queue → job begin → restart 解放)。
        sync.invalidate();
        assert!(!sync.may_apply(ticket, false));

        // ジョブ完了後の再同期は新しい世代で適用できる。
        let ticket = sync.schedule();
        assert!(sync.may_apply(ticket, false));

        // 実行時にジョブが走っていれば適用しない (ジョブを中断しない)。
        assert!(!sync.may_apply(ticket, true));

        // 新しい予約ができた時点で、古い予約は無効。
        let newer = sync.schedule();
        assert!(!sync.may_apply(ticket, false));
        assert!(sync.may_apply(newer, false));
    }

    #[test]
    fn a_completed_start_requests_a_follow_up_sync() {
        // M1: 起動が成功したときだけ完了後の再評価を検討する。
        // 失敗時に予約すると、同じ起動を繰り返してしまう。
        assert!(resync_after_start(true));
        assert!(!resync_after_start(false));

        // ポリシーもモデルも変わっていなければ、後追い同期は走らせない
        // (ホットキー/起動時の事前起動を直後に止めない: Major)。
        let unchanged = Settings::default();
        assert!(!start_needs_follow_up_sync(
            EngineRole::Main,
            &unchanged,
            &unchanged,
            Path::new(".")
        ));

        // 実ファイルのモデルで、モデル変更/常駐変更を検出する。
        let base =
            std::env::temp_dir().join(format!("nuka-start-follow-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp dir");
        let wide = base.join("14b.gguf");
        std::fs::write(&wide, b"m").expect("wide model");
        let small = base.join("0.8b.gguf");
        std::fs::write(&small, b"m").expect("small model");
        let installed = |file: &str, path: &std::path::Path| crate::settings::InstalledModel {
            file: file.to_string(),
            repo: "pfnet/test".to_string(),
            bytes: 1,
            path: path.to_string_lossy().into_owned(),
            installed_at: 0,
        };
        let started = Settings {
            active_model: Some("14b.gguf".to_string()),
            installed_models: vec![installed("14b.gguf", &wide), installed("0.8b.gguf", &small)],
            ..Default::default()
        };
        assert!(!start_needs_follow_up_sync(
            EngineRole::Main,
            &started,
            &started,
            &base
        ));
        // 無関係な設定 (ホットキー) の変更では同期しない。
        let mut hotkey_changed = started.clone();
        hotkey_changed.floating_window.hotkey = "Ctrl+Alt+X".to_string();
        assert!(!start_needs_follow_up_sync(
            EngineRole::Main,
            &started,
            &hotkey_changed,
            &base
        ));
        // 起動中にアクティブモデルが変わった: 再同期する。
        let mut model_changed = started.clone();
        model_changed.active_model = Some("0.8b.gguf".to_string());
        assert!(start_needs_follow_up_sync(
            EngineRole::Main,
            &started,
            &model_changed,
            &base
        ));
        // 起動中に常駐ポリシーが変わった (専用モデル ON→OFF): 再同期する。
        let mut resident_on = started.clone();
        resident_on.floating_window.model = Some("0.8b.gguf".to_string());
        let mut resident_off = resident_on.clone();
        resident_off.background.resident_engine = false;
        assert!(start_needs_follow_up_sync(
            EngineRole::Floating,
            &resident_on,
            &resident_off,
            &base
        ));
        // 常駐 OFF 同士でも専用モデルが変われば再同期する。
        let mut dedicated_changed = resident_off.clone();
        dedicated_changed.floating_window.model = Some("14b.gguf".to_string());
        assert!(start_needs_follow_up_sync(
            EngineRole::Floating,
            &resident_off,
            &dedicated_changed,
            &base
        ));
        // 変化なし (常駐 ON のまま) は同期しない。
        let same = resident_on.clone();
        assert!(!start_needs_follow_up_sync(
            EngineRole::Floating,
            &resident_on,
            &same,
            &base
        ));
        let _ = std::fs::remove_dir_all(&base);

        // その再同期も、起動完了までにジョブが始まっていれば適用しない。
        let mut sync = SyncEpoch::default();
        let ticket = sync.schedule();
        assert!(!sync.may_apply(ticket, true));
        assert!(sync.may_apply(ticket, false));
    }

    #[test]
    fn job_admission_picks_an_engine_with_the_current_model() {
        use JobEngineAction::{Restart, Start, Use};

        let model_a = Path::new("A.gguf");
        let model_b = Path::new("B.gguf");

        // 希望モデルが解決できない: 現状に任せる (既存の起動エラー経路)。
        assert_eq!(decide_job_engine(None, false, None, false, None), Use);

        // 停止中: 起こす。
        assert_eq!(
            decide_job_engine(Some(model_b), false, None, false, None),
            Start
        );
        // 稼働中で希望どおり: そのまま使う。
        assert_eq!(
            decide_job_engine(Some(model_b), false, None, true, Some(model_b)),
            Use
        );
        // 稼働中だが旧モデル: 起こし直す (J1=A の途中に B へ変更 → J2 は B)。
        assert_eq!(
            decide_job_engine(Some(model_b), false, None, true, Some(model_a)),
            Restart
        );
        // 起動中で予約モデルが希望どおり: 起動完了を待って使う。
        assert_eq!(
            decide_job_engine(Some(model_b), true, Some(model_b), false, None),
            Use
        );
        // 起動中で予約モデルが違う: 希望モデルで予約し直す。
        assert_eq!(
            decide_job_engine(Some(model_b), true, Some(model_a), false, None),
            Restart
        );
        // 起動中で予約モデルが不明: 安全側で予約し直す。
        assert_eq!(
            decide_job_engine(Some(model_b), true, None, false, None),
            Restart
        );
    }

    #[test]
    fn hotkey_only_save_does_not_touch_the_resident_engine() {
        // M3: ホットキー記録中は enabled=false を一時保存している。記録後の
        // ホットキーだけの保存で reconcile すると常駐エンジンを止めてしまい、
        // 有効へ戻すときに再ロードが入る。
        let base = Settings::default();

        let mut hotkey_only = base.clone();
        hotkey_only.floating_window.hotkey = "Ctrl+Alt+X".to_string();
        assert!(!floating_settings_affect_engine(&base, &hotkey_only));
        assert!(!should_sync_floating_engine(false, false));
        // 記録中の一時無効化 (suspend) は、設定が変わって見えても触らない。
        assert!(!should_sync_floating_engine(true, true));

        let mut disabled = base.clone();
        disabled.floating_window.enabled = false;
        assert!(floating_settings_affect_engine(&base, &disabled));
        assert!(should_sync_floating_engine(false, true));

        let mut model = base.clone();
        model.floating_window.model = Some("small.gguf".to_string());
        assert!(floating_settings_affect_engine(&base, &model));
    }

    #[test]
    fn hotkey_suspend_is_not_treated_as_a_normal_off() {
        // M3: 記録中は enabled=false が一時保存されるが、エンジン同期は記録前の
        // enabled で評価する。記録中に起動完了/ジョブ完了の sync が走っても、
        // 常駐エンジンを通常の OFF として止めない。
        let control = EngineControl::default();
        let mut settings = Settings::default(); // floating enabled = true
        settings.floating_window.model = Some("small.gguf".to_string());
        assert!(control
            .effective_settings(settings.clone())
            .floating_engine_resident());

        // 記録開始: enabled=false を一時保存し、記録前の値を覚える。
        let mut suspended = settings.clone();
        suspended.floating_window.enabled = false;
        assert!(!suspended.floating_engine_resident());
        control.begin_hotkey_suspend(settings.floating_window.enabled);
        // 二重送信されても、最初に覚えた記録前の値を保つ。
        control.begin_hotkey_suspend(false);

        // 記録中の同期は「通常 OFF」ではなく記録前の enabled で評価する。
        let effective = control.effective_settings(suspended.clone());
        assert!(effective.floating_window.enabled);
        assert!(effective.floating_engine_resident());

        // 記録終了後の明示的な enabled 変更 (有効へ戻す) で suspend を解除する。
        control.end_hotkey_suspend();
        let mut restored = suspended.clone();
        restored.floating_window.enabled = true;
        assert!(control
            .effective_settings(restored)
            .floating_engine_resident());
        // 解除後は保存値がそのまま使われる。
        assert!(
            !control
                .effective_settings(suspended)
                .floating_window
                .enabled
        );
    }

    #[test]
    fn job_start_retry_requires_a_live_current_job() {
        use EnginePhase::Ready;

        // 復旧起動は「現行ジョブ・未取消・停止済み・非シャットダウン」だけ。
        assert!(may_retry_job_start(true, false, false, false, Ready, false));
        assert!(!may_retry_job_start(true, true, false, false, Ready, false));
        assert!(!may_retry_job_start(
            false, false, false, false, Ready, false
        ));
        assert!(!may_retry_job_start(true, false, true, false, Ready, false));
        assert!(!may_retry_job_start(true, false, false, false, Ready, true));
        // 稼働中はクラッシュ (error) の後片付け以外では再起動しない。
        assert!(!may_retry_job_start(true, false, false, true, Ready, false));
        assert!(may_retry_job_start(
            true,
            false,
            false,
            true,
            EnginePhase::Error,
            false
        ));
    }

    #[test]
    fn a_cancelled_job_never_wins_the_start_race_against_the_next_job() {
        // Medium 1 の監査順序を barrier で固定する:
        // J2 が starting=false を読む → 取消済み J1 が起動予約 → J2 続行。
        // J1 は取消済みなので復旧起動してはならず、J2 の Start 予約が勝つ。
        #[derive(Default)]
        struct FakeEngine {
            starting: bool,
            reserved: Option<&'static str>,
        }

        let engine = Arc::new(Mutex::new(FakeEngine::default()));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let j2_engine = Arc::clone(&engine);
        let j2_barrier = Arc::clone(&barrier);
        let j2 = std::thread::spawn(move || {
            // J2 受付: starting=false を読んで Start を選ぶ。
            let starting = j2_engine.lock().expect("fake engine").starting;
            assert_eq!(
                decide_job_engine(Some(Path::new("B.gguf")), starting, None, false, None),
                JobEngineAction::Start
            );
            // ここで旧 J1 の再試行と競合させる。
            j2_barrier.wait();
            // 予約 (reserve_start)。J1 が予約していれば B は入らない。
            let mut engine = j2_engine.lock().expect("fake engine");
            if !engine.starting {
                engine.starting = true;
                engine.reserved = Some("B.gguf");
            }
        });

        let j1_engine = Arc::clone(&engine);
        let j1_barrier = Arc::clone(&barrier);
        let j1 = std::thread::spawn(move || {
            j1_barrier.wait();
            // 取消済み J1 の復旧起動は不可 (job は J2 に差し替わっている)。
            let retry = may_retry_job_start(false, true, false, false, EnginePhase::Ready, false);
            assert!(!retry);
            if retry {
                let mut engine = j1_engine.lock().expect("fake engine");
                engine.starting = true;
                engine.reserved = Some("A.gguf");
            }
        });

        j2.join().expect("J2 thread");
        j1.join().expect("J1 thread");
        let engine = engine.lock().expect("fake engine");
        assert!(engine.starting);
        assert_eq!(engine.reserved, Some("B.gguf"));
    }

    #[test]
    fn a_visibility_snapshot_is_discarded_when_hide_invalidates() {
        // Medium 2: ticket は可視性をロック外で読む。読んだ後に hide/show が
        // 世代を進めたら、そのスナップショットは使わない。
        let mut sync = SyncEpoch::default();
        let ticket = sync.schedule();
        let snapshot = sync.epoch();
        assert!(sync.is_current(ticket));

        sync.invalidate(); // hide / show / 状態変更

        assert!(!sync.is_current(ticket));
        assert_ne!(sync.epoch(), snapshot);
        assert!(!sync.may_apply(ticket, false));
    }

    #[test]
    fn shutdown_rejects_new_starts_and_pending_sync() {
        let mut control = EngineControlInner::default();
        let ticket = control.sync.schedule();
        assert!(control.may_start());
        assert!(control.may_apply(ticket, false));

        control.begin_shutdown();
        assert!(!control.may_start());
        assert!(!control.may_apply(ticket, false));
        // 終了後に新しい ticket を作れても適用はしない。
        let after = control.sync.schedule();
        assert!(!control.may_apply(after, false));
    }

    /// OpenAI 互換の SSE を、指定した断片のまま返す小さなサーバ。
    /// 1 断片が途中で切れていても、クライアント側が行を組み立てられるかを見る。
    async fn spawn_stub_completions(chunks: Vec<String>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("stub を bind できる");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer: Vec<u8> = Vec::new();
            let mut temp = [0u8; 4096];
            // ヘッダの終わりまで読む
            let header_end = loop {
                let Ok(read) = socket.read(&mut temp).await else {
                    return;
                };
                if read == 0 {
                    return;
                }
                buffer.extend_from_slice(&temp[..read]);
                if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            // 本文も読み切ってから返す (書いている途中で閉じない)。
            let header = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
            let length: usize = header
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            while buffer.len() < header_end + length {
                let Ok(read) = socket.read(&mut temp).await else {
                    return;
                };
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp[..read]);
            }

            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await;
            for chunk in chunks {
                if socket.write_all(chunk.as_bytes()).await.is_err() {
                    return;
                }
                let _ = socket.flush().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let _ = socket.shutdown().await;
        });
        port
    }

    #[tokio::test]
    async fn streams_sse_deltas_and_stops_at_done() {
        let chunks = vec![
            // 1 行を 2 回に分けて届ける (行バッファの確認)。
            "data: {\"choices\":[{\"te".to_string(),
            "xt\":\"プロジェクトの\"}]}\n\ndata: {\"choices\":[{\"text\":\"管理画面を\"}]}\n\n"
                .to_string(),
            "data: {\"choices\":[{\"text\":\"開いてください。\"}]}\n\n".to_string(),
            "data: {\"choices\":[],\"usage\":{\"completion_tokens\":6}}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ];
        let port = spawn_stub_completions(chunks).await;
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(false);
        let mut updates: Vec<String> = Vec::new();

        let outcome = stream_completion(&client, port, "prompt", &cancel, |raw| {
            updates.push(raw.to_string());
        })
        .await
        .expect("ストリームを読める");

        match outcome {
            StreamOutcome::Completed { text, tokens } => {
                assert_eq!(text, "プロジェクトの管理画面を開いてください。");
                assert_eq!(tokens, Some(6));
            }
            StreamOutcome::Cancelled => panic!("中止されていない"),
        }
        assert_eq!(
            updates.last().map(String::as_str),
            Some("プロジェクトの管理画面を開いてください。")
        );
    }

    #[tokio::test]
    async fn cancellation_stops_the_stream_before_reading_deltas() {
        let chunks = vec!["data: {\"choices\":[{\"text\":\"あ\"}]}\n\n".to_string()];
        let port = spawn_stub_completions(chunks).await;
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(true);

        let outcome = stream_completion(&client, port, "prompt", &cancel, |_| {})
            .await
            .expect("中止として返る");

        assert!(matches!(outcome, StreamOutcome::Cancelled));
    }

    #[tokio::test]
    async fn http_error_status_is_reported_with_the_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut temp = [0u8; 4096];
                let _ = socket.read(&mut temp).await;
                let body = "{\"error\":\"model is loading\"}";
                let response = format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(false);
        let error = stream_completion(&client, port, "prompt", &cancel, |_| {})
            .await
            .expect_err("エラーになる");
        assert!(error.contains("503"));
        assert!(error.contains("model is loading"));
    }
}
