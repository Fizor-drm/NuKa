//! フロントエンドとの IPC (Task B がこのコントラクトどおりに実装する)。
//!
//! コマンド:
//!   get_engine_status() -> { state, detail? }
//!   restart_engine() -> ()
//!   get_settings() / set_settings(patch) -> Settings
//!   detect_language(text) -> Option<{ code, confidence }>
//!   translate(req: { text, source_lang?, target_lang, form?, glossary? }) -> jobId
//!   cancel_translation(jobId) -> ()
//!
//! イベント:
//!   engine:status { state, detail? }
//!   translation:progress { jobId, done, total }
//!   translation:chunk { jobId, delta }
//!   translation:done { jobId, text, tokens? }
//!   translation:error { jobId, message }

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::time::timeout;

use crate::detect::{self, DetectedLanguage};
use crate::engine::{Engine, EnginePhase, EngineStatus, SERVER_ALIAS};
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
}

impl JobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 新しいジョブを始める。進行中のジョブは捨てる。
    fn begin(&self) -> (u32, Arc<AtomicBool>) {
        if let Some(previous) = self.lock().take() {
            previous.cancel.store(true, Ordering::SeqCst);
        }
        let id = self
            .inner
            .next_id
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        *self.lock() = Some(JobControl {
            id,
            cancel: cancel.clone(),
        });
        (id, cancel)
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

#[tauri::command]
pub fn get_engine_status(state: State<'_, AppState>) -> EngineStatus {
    state.engine.status()
}

/// エンジンを起動し直す。設定の変更を反映するときにも呼ぶ。
#[tauri::command]
pub async fn restart_engine(state: State<'_, AppState>) -> Result<(), String> {
    let settings = state.settings.get();
    let engine = state.engine.clone();
    engine.start(&settings).await;
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.get()
}

#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, patch: SettingsPatch) -> Result<Settings, String> {
    state.settings.apply(patch)
}

#[tauri::command]
pub fn detect_language(text: String) -> Option<DetectedLanguage> {
    detect::detect_language(&text)
}

/// 翻訳を始めて jobId を返す。結果はイベントで流れる。
///
/// 引数は 1 つの `req` オブジェクトなので、フロントは
/// `invoke("translate", { req: { text, target_lang, ... } })` で呼ぶ。
#[tauri::command]
pub fn translate(app: AppHandle, state: State<'_, AppState>, req: TranslateRequest) -> u32 {
    let (job_id, cancel) = state.jobs.begin();
    let engine = state.engine.clone();
    let jobs = state.jobs.clone();
    tauri::async_runtime::spawn(async move {
        run_translation(app, engine, jobs, job_id, cancel, req).await;
    });
    job_id
}

#[tauri::command]
pub fn cancel_translation(state: State<'_, AppState>, job_id: u32) {
    state.jobs.cancel(job_id);
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
    crate::installer::install_runtime(&app, &settings, &downloads, &engine).await?;
    Ok(())
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

#[tauri::command]
pub fn delete_model(
    state: State<'_, AppState>,
    file: String,
) -> Result<Settings, crate::message::LocalizedText> {
    crate::installer::delete_model(&state.settings, &file)
}

/// 使用するモデルを切り替え、エンジンを起こし直す。
#[tauri::command]
pub async fn set_active_model(
    state: State<'_, AppState>,
    file: String,
) -> Result<Settings, crate::message::LocalizedText> {
    let settings = crate::installer::set_active_model(&state.settings, &file)?;
    let engine = state.engine.clone();
    engine.start(&settings).await;
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
pub fn install_update(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let engine = state.engine.clone();
    let settings = state.settings.clone();
    crate::updater::install(&app, &state.updates, &engine, &settings)
}

// ---------------------------------------------------------------------------
// ジョブの実行
// ---------------------------------------------------------------------------

struct JobSuccess {
    text: String,
    tokens: Option<u32>,
}

async fn run_translation(
    app: AppHandle,
    engine: Engine,
    jobs: JobManager,
    job_id: u32,
    cancel: Arc<AtomicBool>,
    req: TranslateRequest,
) {
    let outcome = translate_job(&app, &engine, job_id, &cancel, &req).await;
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
        Err(message) => {
            if !is_cancelled(&cancel) {
                let event = ErrorEvent { job_id, message };
                if let Err(error) = app.emit("translation:error", event) {
                    eprintln!("translation:error を送れません: {error}");
                }
            }
        }
    }
}

async fn translate_job(
    app: &AppHandle,
    engine: &Engine,
    job_id: u32,
    cancel: &AtomicBool,
    req: &TranslateRequest,
) -> Result<Option<JobSuccess>, String> {
    // 原文は常に 400 文字前後へ割ってから直列に訳す。
    let chunks = prompt::split_text(&req.text, CHUNK_LIMIT);
    let Some(first) = chunks.first() else {
        return Err(PromptError::EmptyText.to_string());
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

    let Some(port) = engine.port() else {
        let status = engine.status();
        return Err(match status.state {
            EnginePhase::Error => format!(
                "エンジンを起動できていません。{}",
                status
                    .detail
                    .map(|detail| detail.fallback)
                    .unwrap_or_default()
            ),
            EnginePhase::Ready => {
                "エンジンの待ち受けポートがありません。再起動してください。".to_string()
            }
            _ => "エンジンを準備しています。少し待ってからもう一度お試しください。".to_string(),
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
            return Err("モデルが訳文を返しませんでした。".to_string());
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
        let (first, first_cancel) = jobs.begin();
        let (second, second_cancel) = jobs.begin();
        assert_ne!(first, second);
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
        let (id, _) = jobs.begin();
        assert_eq!(id, 1);
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
