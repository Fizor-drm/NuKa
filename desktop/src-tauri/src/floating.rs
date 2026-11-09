//! フローティング翻訳ウィンドウ (Phase 1)。
//!
//! グローバルショートカット (既定 Ctrl+Shift+T) を押すと、いま前面にあるアプリの
//! 選択テキストを取得し、常時最前面の小ウィンドウで翻訳する。
//!
//! # なぜ SendInput とクリップボードを使うのか
//!
//! 選択テキストを取る手段は、前面アプリに「コピー」を送ってクリップボードを読む
//! しかない。`RegisterHotKey` で Ctrl+C+C を扱うことはできない (Ctrl+C が 2 回
//! 押されたことを OS のホットキーとしては表現できない) ため、Phase 1 は
//! 「修飾子 + 英数字 1 キー」のホットキーだけを登録し、発火時に `SendInput` で
//! Ctrl+C を前景アプリへ送る。Phase 2 の Ctrl+C+C も同じ理由で低レベルフックが
//! 必要になるが、`WH_KEYBOARD_LL` は Phase 1 では使わない。
//!
//! unsafe な Windows API の利用範囲は次の関数だけに閉じている:
//!
//! - `read_clipboard_text` / `write_clipboard_text`: 選択テキストの取得と退避の復元
//! - `send_copy_shortcut`: 前景アプリへの Ctrl+C (KEYEVENTF_SCANCODE で送る)
//! - `wait_for_hotkey_keys_release`: ホットキーのキーと修飾子が離れるまで待つ
//! - `send_vk_key`: 押しっぱなしの Shift / Alt を一時解除・復元する
//! - `cursor_position` / `monitor_work_area`: 表示位置の計算
//!
//! 取得したテキストは Tauri のイベントでこのアプリのウィンドウに渡すだけで、
//! 外部ネットワークへは送らない (翻訳はローカルの llama-server)。
//!
//! # クリップボードの復元
//!
//! 発火前のクリップボードが **テキスト** だったときは、選択テキストを読んだあと
//! 必ず元のテキストへ戻す。画像などテキスト以外だったとき、または空だったときは
//! 復元しない (取得した選択テキストがクリップボードに残る)。クリップボードの
//! 形式を完全に保存・復元するには Phase 1 の範囲を超えるため、README にも明記する。

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

use crate::engine::EngineRole;
use crate::message::LocalizedText;
use crate::AppState;

/// フローティングウィンドウのラベル。
pub const FLOATING_LABEL: &str = "floating";
/// メインウィンドウのラベル。
pub const MAIN_LABEL: &str = "main";
/// 設定の既定ホットキー。settings.rs の既定値と一致させる。
pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+T";
/// ウィンドウ幅 (論理ピクセル)。位置計算は物理ピクセルに直してから行う。
pub const WINDOW_WIDTH: f64 = 420.0;
/// 既定のウィンドウ高さ (論理ピクセル)。
///
/// 右下のつまみでリサイズできるようになったため、初期表示はコンテンツを
/// 隠さない範囲で低めにしてある。capture 時の位置計算は実サイズ
/// (`window.outer_size()`) を使い、取得できないときだけこの値にフォールバックする。
pub const WINDOW_HEIGHT: f64 = 240.0;
/// リサイズで縮められる最小のウィンドウ幅 (論理ピクセル)。
///
/// ヘッダーは「訳マーク + 言語ボタン (最大 150px) + コピー + 閉じる」を
/// 1 行に並べるため、これ以上狭くするとボタンが潰れる (280px でほぼ一杯)。
pub const MIN_WINDOW_WIDTH: f64 = 280.0;
/// リサイズで縮められる最小のウィンドウ高さ (論理ピクセル)。
///
/// ヘッダーは 1 行 (パディング込みで約 40px) なので、160px あれば
/// 本文のスクロール領域が約 120px 残り、操作に支障しない。
pub const MIN_WINDOW_HEIGHT: f64 = 160.0;
/// ホットキー発火イベント。全ウィンドウへ送る。
pub const CAPTURE_EVENT: &str = "floating:capture";

/// ホットキーのキーと修飾子が離れるまで待つ上限。
///
/// 人間の通常の解放 (200〜500ms) より長く取り、押しっぱなしのときは
/// タイムアウト後に Shift / Alt を一時解除して Ctrl+C を送る。
const KEY_RELEASE_WAIT_LIMIT: Duration = Duration::from_millis(900);
/// Ctrl+C を送ってからクリップボードを読むまでの待ち。
const CLIPBOARD_SETTLE: Duration = Duration::from_millis(250);
/// 表示直後のフォーカス揺れを無視する時間。
///
/// `show()` は Windows では `SW_SHOW` でウィンドウをアクティブ化するため、
/// 前面ウィンドウの都合で「一瞬だけフォーカスが当たってすぐ外れる」ことがある。
/// その揺れで閉じないようにする。
const SHOW_FOCUS_GRACE: Duration = Duration::from_millis(800);
/// noSelection の案内を出している間に blur で閉じない時間。
///
/// noSelection は「テキストが選択されていません」を約 2.5 秒見せてから
/// フロントエンドが自分で閉じる設計なので、その間は blur を無視する。
const NO_SELECTION_BLUR_GRACE: Duration = Duration::from_millis(2600);
/// noSelection の案内を最低限表示する時間。
///
/// フロントエンドの auto-hide は約 2.5 秒。それより早い auto-hide が届いても
/// ここまでは表示を続ける (原因調査ログは出す)。× / Esc は即時に閉じる。
const NO_SELECTION_MIN_VISIBLE: Duration = Duration::from_millis(2400);
/// フォーカス喪失から実際に閉じるまでの猶予。この間に戻れば閉じない。
const BLUR_HIDE_DELAY: Duration = Duration::from_millis(180);
/// 同じホットキーの連続押下 (キーリピート/二重送信) を無視する間隔。
const HOTKEY_PRESS_DEBOUNCE: Duration = Duration::from_millis(300);
/// 表示直後にホットキーがもう一度押されても、トグルで閉じないための猶予。
const HOTKEY_TOGGLE_GUARD: Duration = Duration::from_millis(1000);
/// クリップボードを開く再試行回数 (他プロセスが掴んでいることがある)。
const CLIPBOARD_OPEN_RETRIES: u32 = 10;
/// クリップボードを開く再試行の間隔。
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(10);
/// CF_UNICODETEXT。WinUser.h の標準クリップボード形式で、値は Windows でも不変。
const CF_UNICODETEXT: u32 = 13;
/// クリップボードから読む UTF-16 ユニット数の上限 (終端が無いデータで無限に読まない)。
const MAX_CLIPBOARD_UNITS: usize = 1024 * 1024;
/// カーソルからウィンドウを離す量 (物理ピクセル)。
const CURSOR_OFFSET: i32 = 16;

/// `floating:capture` のペイロード。
///
/// `text` が `null` のときは「選択テキストが取れなかった」を意味する。
/// フィールド名はフロントエンドの契約どおり snake_case の `target_lang`。
#[derive(Clone, Debug, Serialize)]
struct CaptureEvent {
    text: Option<String>,
    target_lang: String,
    /// この翻訳を実際に処理するエンジンの役割 ("main" / "floating")。
    ///
    /// 解決モデルがメイン用と同一ならメイン用を共有するため、フローティング
    /// 画面はこの役割の `engine:status` を見て起動中表示を出す。
    engine_role: EngineRole,
}

/// プロセス起動 (setup) からの経過時間。実機でログの順序を確定するために使う。
static LOG_START: OnceLock<Instant> = OnceLock::new();

fn elapsed_label() -> String {
    let start = LOG_START.get_or_init(Instant::now);
    format!("{:.2}s", start.elapsed().as_secs_f64())
}

/// デバッグ用ログ。リリースビルドでは何も出さない
/// (`NUKA_FLOATING_DEBUG=1` を設定したときだけ出す)。
///
/// stderr が閉じられている環境 (E2E ランチャーがパイプを閉じる等) でも
/// 書き込み失敗でパニックしないよう、`eprintln!` ではなく結果を捨てる
/// `writeln!` を使う。
fn debug_log(message: impl std::fmt::Display) {
    if cfg!(debug_assertions) || std::env::var_os("NUKA_FLOATING_DEBUG").is_some() {
        let _ = writeln!(
            std::io::stderr(),
            "[floating {}] {message}",
            elapsed_label()
        );
    }
}

macro_rules! debug_log {
    ($($arg:tt)*) => {
        debug_log(format_args!($($arg)*))
    };
}

/// フローティングウィンドウ関連の共有状態。
#[derive(Clone)]
pub struct FloatingState {
    /// メイン画面の訳先言語。フローティングの初期訳先として使う。
    target_lang: Arc<Mutex<String>>,
    /// いま登録しているショートカット。解除・再登録のために覚えておく。
    registered: Arc<Mutex<Option<Shortcut>>>,
    /// 選択テキスト取得の多重実行を防ぐ (キー押しっぱなしの自動リピート対策)。
    capture_busy: Arc<AtomicBool>,
    /// 直近に show を始めた時刻。表示直後のフォーカス揺れを吸収する。
    shown_at: Arc<Mutex<Option<Instant>>>,
    /// この時刻までは blur で閉じない (noSelection の案内表示中)。
    blur_guard_until: Arc<Mutex<Option<Instant>>>,
    /// 画面に出したままか。Tauri の is_visible だけに頼らずトグル判定に使う。
    shown: Arc<AtomicBool>,
    /// フォーカス状態の世代。遅延 hide のキャンセルに使う。
    focus_epoch: Arc<AtomicU64>,
    /// 表示 (show) の世代。遅延した auto-hide のキャンセルに使う。
    show_epoch: Arc<AtomicU64>,
    /// いま表示しているのが noSelection の案内か。
    no_selection_display: Arc<AtomicBool>,
    /// 直近のホットキー押下時刻。キーリピート/二重押下を無視する。
    last_press: Arc<Mutex<Option<Instant>>>,
}

impl Default for FloatingState {
    fn default() -> Self {
        Self {
            target_lang: Arc::new(Mutex::new("ja".to_string())),
            registered: Arc::new(Mutex::new(None)),
            capture_busy: Arc::new(AtomicBool::new(false)),
            shown_at: Arc::new(Mutex::new(None)),
            blur_guard_until: Arc::new(Mutex::new(None)),
            shown: Arc::new(AtomicBool::new(false)),
            focus_epoch: Arc::new(AtomicU64::new(0)),
            show_epoch: Arc::new(AtomicU64::new(0)),
            no_selection_display: Arc::new(AtomicBool::new(false)),
            last_press: Arc::new(Mutex::new(None)),
        }
    }
}

impl FloatingState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn target_lang(&self) -> String {
        self.lock_target().clone()
    }

    /// メイン画面から報告された訳先言語を覚える。空・長すぎる・記号入りは無視する。
    pub fn set_target_lang(&self, code: &str) {
        let code = code.trim();
        if code.is_empty() || code.len() > 16 {
            return;
        }
        if !code
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return;
        }
        *self.lock_target() = code.to_string();
    }

    fn begin_capture(&self) -> bool {
        self.capture_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn end_capture(&self) {
        self.capture_busy.store(false, Ordering::SeqCst);
    }

    /// show を始める直前に呼ぶ。表示直後の blur を無視する起点になる。
    /// 戻り値はこの表示の世代 (遅延 auto-hide のキャンセル判定に使う)。
    fn mark_show_start(&self, now: Instant, no_selection: bool) -> u64 {
        *self.lock_shown_at() = Some(now);
        self.no_selection_display
            .store(no_selection, Ordering::SeqCst);
        self.bump_focus_epoch();
        self.show_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn show_epoch(&self) -> u64 {
        self.show_epoch.load(Ordering::SeqCst)
    }

    fn is_no_selection_display(&self) -> bool {
        self.no_selection_display.load(Ordering::SeqCst)
    }

    fn time_since_show(&self, now: Instant) -> Option<Duration> {
        self.lock_shown_at()
            .map(|shown| now.saturating_duration_since(shown))
    }

    /// hide 時に表示状態を解除し、遅延中の auto-hide も無効化する。
    fn end_display(&self) {
        self.shown.store(false, Ordering::SeqCst);
        self.no_selection_display.store(false, Ordering::SeqCst);
        *self.lock_blur_guard() = None;
        self.bump_focus_epoch();
        self.show_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// show からの猶予の残り。猶予外なら `None`。
    fn show_grace_remaining(&self, now: Instant) -> Option<Duration> {
        self.lock_shown_at().and_then(|shown| {
            let elapsed = now.saturating_duration_since(shown);
            (elapsed < SHOW_FOCUS_GRACE).then(|| SHOW_FOCUS_GRACE - elapsed)
        })
    }

    /// show から `window` 以内か (トグル誤爆の防止にも使う)。
    fn shown_recently(&self, now: Instant, window: Duration) -> bool {
        self.lock_shown_at()
            .is_some_and(|shown| now.saturating_duration_since(shown) < window)
    }

    fn set_shown(&self, shown: bool) {
        self.shown.store(shown, Ordering::SeqCst);
    }

    /// フローティングウィンドウを表示中か (hide 済みなら false)。
    ///
    /// 常駐ポリシーの再同期が「表示中の翻訳に使うエンジンを止めない」判定に使う。
    pub(crate) fn is_shown(&self) -> bool {
        self.shown.load(Ordering::SeqCst)
    }

    /// `until` まで blur を無視する。`None` で解除。
    fn set_no_selection_guard(&self, until: Option<Instant>) {
        *self.lock_blur_guard() = until;
    }

    fn no_selection_active(&self, now: Instant) -> bool {
        self.no_selection_remaining(now).is_some()
    }

    /// noSelection の案内を出しておく残り時間。期間外なら `None`。
    fn no_selection_remaining(&self, now: Instant) -> Option<Duration> {
        self.lock_blur_guard()
            .and_then(|until| (now < until).then(|| until.saturating_duration_since(now)))
    }

    fn bump_focus_epoch(&self) -> u64 {
        self.focus_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn focus_epoch(&self) -> u64 {
        self.focus_epoch.load(Ordering::SeqCst)
    }

    /// 同じ押下の繰り返し (キーリピート/二重送信) なら false。
    /// 時刻は毎回更新するので、押しっぱなしの間は連続押下として扱わない。
    fn should_handle_press(&self, now: Instant) -> bool {
        let mut last = self
            .last_press
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let allowed = last.is_none_or(|previous| {
            now.saturating_duration_since(previous) >= HOTKEY_PRESS_DEBOUNCE
        });
        *last = Some(now);
        allowed
    }

    fn lock_target(&self) -> MutexGuard<'_, String> {
        self.target_lang
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_registered(&self) -> MutexGuard<'_, Option<Shortcut>> {
        self.registered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_shown_at(&self) -> MutexGuard<'_, Option<Instant>> {
        self.shown_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_blur_guard(&self) -> MutexGuard<'_, Option<Instant>> {
        self.blur_guard_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// ホットキーの正規化とパース (純粋関数)
// ---------------------------------------------------------------------------

/// 修飾子と 1 キーの組み合わせ。`key` は大文字の ASCII 英数字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: char,
}

impl HotkeySpec {
    /// プラグインの `Shortcut` に変換する。パース済みなら常に成功する。
    pub fn to_shortcut(self) -> Option<Shortcut> {
        let mut modifiers = Modifiers::empty();
        if self.ctrl {
            modifiers |= Modifiers::CONTROL;
        }
        if self.alt {
            modifiers |= Modifiers::ALT;
        }
        if self.shift {
            modifiers |= Modifiers::SHIFT;
        }
        Some(Shortcut::new(Some(modifiers), code_for_key(self.key)?))
    }
}

impl std::fmt::Display for HotkeySpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            formatter.write_str("Ctrl+")?;
        }
        if self.alt {
            formatter.write_str("Alt+")?;
        }
        if self.shift {
            formatter.write_str("Shift+")?;
        }
        write!(formatter, "{}", self.key)
    }
}

/// `"ctrl + shift + t"` のような入力をパースする。
///
/// 修飾子は Ctrl / Control / Alt / Shift のみ。キーは ASCII 英数字 1 文字だけ。
/// 修飾子は 1 つ以上、キーはちょうど 1 つ必要。重複した修飾子はまとめる。
pub fn parse_hotkey(input: &str) -> Option<HotkeySpec> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<char> = None;

    for part in input.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            _ => {
                let mut characters = part.chars();
                let character = characters.next()?;
                if characters.next().is_some() || !character.is_ascii_alphanumeric() {
                    return None;
                }
                if key.replace(character.to_ascii_uppercase()).is_some() {
                    // キーが 2 つある ("Ctrl+T+U" など)
                    return None;
                }
            }
        }
    }

    if !(ctrl || alt || shift) {
        return None;
    }
    Some(HotkeySpec {
        ctrl,
        alt,
        shift,
        key: key?,
    })
}

/// パースして表記をそろえる。無効なら `None`。
pub fn normalize_hotkey(input: &str) -> Option<String> {
    parse_hotkey(input).map(|spec| spec.to_string())
}

const LETTER_CODES: [Code; 26] = [
    Code::KeyA,
    Code::KeyB,
    Code::KeyC,
    Code::KeyD,
    Code::KeyE,
    Code::KeyF,
    Code::KeyG,
    Code::KeyH,
    Code::KeyI,
    Code::KeyJ,
    Code::KeyK,
    Code::KeyL,
    Code::KeyM,
    Code::KeyN,
    Code::KeyO,
    Code::KeyP,
    Code::KeyQ,
    Code::KeyR,
    Code::KeyS,
    Code::KeyT,
    Code::KeyU,
    Code::KeyV,
    Code::KeyW,
    Code::KeyX,
    Code::KeyY,
    Code::KeyZ,
];
const DIGIT_CODES: [Code; 10] = [
    Code::Digit0,
    Code::Digit1,
    Code::Digit2,
    Code::Digit3,
    Code::Digit4,
    Code::Digit5,
    Code::Digit6,
    Code::Digit7,
    Code::Digit8,
    Code::Digit9,
];

fn code_for_key(key: char) -> Option<Code> {
    match key {
        'A'..='Z' => LETTER_CODES.get((key as usize) - ('A' as usize)).copied(),
        '0'..='9' => DIGIT_CODES.get((key as usize) - ('0' as usize)).copied(),
        _ => None,
    }
}

/// ショートカットのキーから Windows の仮想キーコードを引く。
/// 押しっぱなし判定に使う (修飾子は VK_CONTROL / VK_SHIFT / VK_MENU を見る)。
#[cfg(windows)]
fn shortcut_vk(shortcut: &Shortcut) -> Option<u16> {
    let key = shortcut.key;
    // VK_A..=VK_Z は 0x41..=0x5A、VK_0..=VK_9 は 0x30..=0x39。
    if let Some(index) = LETTER_CODES.iter().position(|code| *code == key) {
        return Some(0x41 + index as u16);
    }
    DIGIT_CODES
        .iter()
        .position(|code| *code == key)
        .map(|index| 0x30 + index as u16)
}

#[cfg(not(windows))]
fn shortcut_vk(_shortcut: &Shortcut) -> Option<u16> {
    None
}

/// ホットキーが登録できなかったときのメッセージ。
pub fn hotkey_conflict_error(hotkey: &str) -> LocalizedText {
    LocalizedText::new(
        "settings.floating.hotkeyConflict",
        "ホットキーを登録できません。ほかのアプリが同じキーを使っている可能性があります。",
    )
    .param("hotkey", hotkey)
}

/// ホットキーの書式が不正なときのメッセージ。
pub fn hotkey_invalid_error(hotkey: &str) -> LocalizedText {
    LocalizedText::new(
        "settings.floating.hotkeyInvalid",
        "Ctrl・Alt・Shift のいずれか + 英数字の組み合わせで指定してください。",
    )
    .param("hotkey", hotkey)
}

// ---------------------------------------------------------------------------
// 表示位置 (純粋関数)
// ---------------------------------------------------------------------------

/// モニタの作業領域 (物理ピクセル、仮想スクリーン座標)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// カーソル位置からウィンドウの左上を決める。
///
/// カーソルの下に置き、入りきらなければ上へ回す。横はカーソル中心。
/// 最後に作業領域へクランプする (マルチモニタの負座標にも対応)。
pub fn place_window(
    cursor_x: i32,
    cursor_y: i32,
    width: i32,
    height: i32,
    work: WorkArea,
) -> (i32, i32) {
    let width = width.max(1);
    let height = height.max(1);
    let min_x = work.left;
    let min_y = work.top;
    let max_x = (work.right - width).max(min_x);
    let max_y = (work.bottom - height).max(min_y);

    let x = cursor_x - width / 2;
    let mut y = cursor_y + CURSOR_OFFSET;
    if y + height > work.bottom {
        y = cursor_y - height - CURSOR_OFFSET;
    }
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

/// ウィンドウサイズを作業領域に収める。
///
/// 別モニタで大きくしたウィンドウは、より小さいモニタの作業領域を超えることが
/// ある。位置だけをクランプすると右下のはみ出し分 (リサイズつまみ) が画面外に
/// なり操作できなくなるため、配置前にサイズ自体をクランプする。
pub fn clamp_size_to_work_area(width: i32, height: i32, work: WorkArea) -> (i32, i32) {
    let work_width = (work.right - work.left).max(1);
    let work_height = (work.bottom - work.top).max(1);
    (width.clamp(1, work_width), height.clamp(1, work_height))
}

// ---------------------------------------------------------------------------
// ウィンドウの生成とホットキーの登録
// ---------------------------------------------------------------------------

/// 非表示のフローティングウィンドウを用意し、設定に従ってホットキーを登録する。
///
/// ホットキーの登録はプラグインがメインスレッドへ処理を投げて待つ実装なので、
/// setup (メインスレッド) から直接呼ぶとデッドロックする。ここではブロッキング
/// スレッドへ逃がして、setup を止めない。
pub fn setup(app: &AppHandle) -> Result<(), LocalizedText> {
    let _ = LOG_START.get_or_init(Instant::now);
    if app.get_webview_window(FLOATING_LABEL).is_none() {
        WebviewWindowBuilder::new(app, FLOATING_LABEL, WebviewUrl::App("floating.html".into()))
            .title("NuKa")
            .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
            // 右下のつまみ (startResizeDragging) でリサイズするため、装飾なしでも
            // リサイズ可能にしておく。Windows では tao が undecorated のとき
            // WS_THICKFRAME を外すが、`startResizeDragging` は WM_NCLBUTTONDOWN を
            // 直接 PostMessage するので OS 側のサイズ変更ループは動く。
            .resizable(true)
            .min_inner_size(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .build()
            .map_err(|error| {
                LocalizedText::new(
                    "error.raw",
                    format!("フローティングウィンドウを作成できません: {error}"),
                )
                .param(
                    "detail",
                    format!("フローティングウィンドウを作成できません: {error}"),
                )
            })?;
    }

    // 登録時点の最新設定を読む (起動直後に設定が変わっても古い値で上書きしない)。
    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if app_handle.try_state::<AppState>().is_none() {
            return;
        }
        let floating = app_handle.state::<AppState>().floating.clone();
        if let Err(error) = apply_hotkey(&app_handle, &floating) {
            eprintln!("{}", error.fallback);
        }
    });
    Ok(())
}

/// 設定に合わせてホットキーを登録し直す。
///
/// 設定は呼び出しのたびに AppState から読み直す。保存と登録が別スレッドで
/// 前後しても、最後に実行された登録が最新設定へ収束するようにするため。
///
/// 競合で新しいキーを登録できなかったときは、直前に登録していたキーへ戻してから
/// `settings.floating.hotkeyConflict` を返す。設定値そのものは呼び出し側が保存済み
/// なので変更後の値のまま残る (登録だけ失敗した状態)。この挙動は README に書く。
///
/// この関数はプラグインがメインスレッドを待つため、**メインスレッド以外**から呼ぶこと。
pub fn apply_hotkey(app: &AppHandle, state: &FloatingState) -> Result<(), LocalizedText> {
    // 登録ロックを先に取る。設定の読み取りと登録の順序が入れ替わると、
    // 並行する set_floating_settings の後に古い設定で登録してしまう。
    let mut registered = state.lock_registered();
    let Some(app_state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    let settings = app_state.settings.get();
    let desired = if settings.floating_window.enabled {
        parse_hotkey(&settings.floating_window.hotkey).and_then(HotkeySpec::to_shortcut)
    } else {
        None
    };

    let previous = *registered;
    if let Some(previous) = previous {
        if let Err(error) = app.global_shortcut().unregister(previous) {
            eprintln!("ホットキーを解除できません: {error}");
        }
        *registered = None;
    }

    let Some(desired) = desired else {
        debug_log!("hotkey not registered (floating window disabled)");
        return Ok(());
    };

    match app.global_shortcut().register(desired) {
        Ok(()) => {
            *registered = Some(desired);
            debug_log!("hotkey registered: {}", settings.floating_window.hotkey);
            Ok(())
        }
        Err(error) => {
            eprintln!(
                "ホットキー {} を登録できません (ほかのアプリと競合?): {error}",
                settings.floating_window.hotkey
            );
            if let Some(previous) = previous {
                match app.global_shortcut().register(previous) {
                    Ok(()) => *registered = Some(previous),
                    Err(restore_error) => {
                        eprintln!("以前のホットキーを戻せません: {restore_error}");
                    }
                }
            }
            Err(hotkey_conflict_error(&settings.floating_window.hotkey))
        }
    }
}

/// ホットキーが押されたときの処理。プラグインのハンドラから呼ぶ。
pub fn on_hotkey(app: &AppHandle, shortcut: &Shortcut) {
    let Some(window) = app.get_webview_window(FLOATING_LABEL) else {
        return;
    };
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let hotkey_vk = shortcut_vk(shortcut);
    let now = Instant::now();
    // キーリピートやエージェントの二重押下で、show 直後にトグル hide しない。
    if !state.floating.should_handle_press(now) {
        debug_log!("hotkey press ignored (debounce)");
        return;
    }

    // Tauri の is_visible だけでは判定しない。こちらが hide した覚えのあるときは
    // 古い visible 状態が残っていてもトグルしない。
    let visible = state.floating.is_shown() && window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);
    debug_log!(
        "hotkey: shown={} visible={} focused={} vk={hotkey_vk:?}",
        state.floating.is_shown(),
        visible,
        focused
    );

    if visible && focused {
        if state.floating.no_selection_active(now) {
            // noSelection の案内表示中は閉じない (約 2.5 秒後に自動で閉じる)。
            debug_log!("hotkey toggle ignored (noSelection message is showing)");
            return;
        }
        if state.floating.shown_recently(now, HOTKEY_TOGGLE_GUARD) {
            // 表示直後のキーリピート/二重押下で閉じてしまわないようにする。
            debug_log!("hotkey toggle ignored (shown recently)");
            return;
        }
        hide_with_reason(app, "hotkey-toggle");
        return;
    }

    start_capture(app, hotkey_vk);
}

/// トレイメニュー「翻訳ウィンドウを表示」から、選択テキストの取得を始める。
///
/// ホットキーと同じ capture フローを呼ぶが、表示中でもトグルで閉じない
/// (トレイからは常に「取り直して表示」)。連打はホットキーと同じ間隔で無視する。
pub fn trigger_capture(app: &AppHandle, hotkey_vk: Option<u16>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if !state.floating.should_handle_press(Instant::now()) {
        debug_log!("tray trigger ignored (debounce)");
        return;
    }
    start_capture(app, hotkey_vk);
}

/// 選択テキストの取得を 1 回だけ始める (多重実行の防止込み)。
fn start_capture(app: &AppHandle, hotkey_vk: Option<u16>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if !state.floating.begin_capture() {
        debug_log!("capture ignored (busy)");
        return;
    }

    let trigger_started = Instant::now();
    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        capture_and_show(&app_handle, trigger_started, hotkey_vk);
        if let Some(state) = app_handle.try_state::<AppState>() {
            state.floating.end_capture();
        }
    });
}

/// `hide_floating` に渡された origin をログ用に正規化する。
fn sanitize_origin(origin: &str) -> String {
    let trimmed = origin.trim();
    if trimmed.is_empty()
        || trimmed.len() > 32
        || !trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return "unknown".to_string();
    }
    trimmed.to_string()
}

/// auto-hide 要求の扱い。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoHideDecision {
    /// いまは noSelection 表示ではないので無視する (前の表示の古いタイマー)。
    Ignore,
    /// 最低表示時間まで待ってから閉じる。
    Defer(Duration),
    /// そのまま閉じる。
    Hide,
}

fn decide_auto_hide(no_selection: bool, elapsed: Option<Duration>) -> AutoHideDecision {
    if !no_selection {
        return AutoHideDecision::Ignore;
    }
    match elapsed {
        Some(elapsed) if elapsed < NO_SELECTION_MIN_VISIBLE => {
            AutoHideDecision::Defer(NO_SELECTION_MIN_VISIBLE - elapsed)
        }
        _ => AutoHideDecision::Hide,
    }
}

/// フローティングウィンドウを隠し、進行中のフローティング翻訳を止める。
///
/// `origin` はフロントエンドの呼び出し元 ("close-button" / "escape" / "auto-hide")。
/// auto-hide は現在の noSelection 表示に対するものだけを受け付け、早すぎる場合は
/// 最低表示時間まで遅らせる (原因調査のため、その事実はログに残す)。
pub fn hide(app: &AppHandle, origin: &str) {
    let origin = sanitize_origin(origin);

    if origin == "auto-hide" {
        if let Some(state) = app.try_state::<AppState>() {
            let now = Instant::now();
            let elapsed = state.floating.time_since_show(now);
            match decide_auto_hide(state.floating.is_no_selection_display(), elapsed) {
                AutoHideDecision::Ignore => {
                    debug_log!(
                        "hide (command: auto-hide) ignored (current display is not noSelection)"
                    );
                    return;
                }
                AutoHideDecision::Defer(remaining) => {
                    let epoch = state.floating.show_epoch();
                    debug_log!(
                        "hide (command: auto-hide) deferred {:.2}s (shown={:.2}s, min display)",
                        remaining.as_secs_f64(),
                        elapsed.map_or(0.0, |value| value.as_secs_f64())
                    );
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        std::thread::sleep(remaining);
                        let Some(state) = app_handle.try_state::<AppState>() else {
                            return;
                        };
                        if state.floating.show_epoch() != epoch || !state.floating.is_shown() {
                            debug_log!("deferred auto-hide cancelled (new show or already hidden)");
                            return;
                        }
                        hide_with_reason(&app_handle, "command: auto-hide (deferred)");
                    });
                    return;
                }
                AutoHideDecision::Hide => {}
            }
        }
    }

    hide_with_reason(app, &format!("command: {origin}"));
}

fn hide_with_reason(app: &AppHandle, reason: &str) {
    let now = Instant::now();
    let since_show = if let Some(state) = app.try_state::<AppState>() {
        state.floating.end_display();
        state.jobs.cancel_current_for_origin(FLOATING_LABEL);
        state.floating.time_since_show(now)
    } else {
        None
    };
    if let Some(window) = app.get_webview_window(FLOATING_LABEL) {
        if let Err(error) = window.hide() {
            eprintln!("フローティングウィンドウを隠せません: {error}");
        }
    }
    debug_log!(
        "hide ({reason}) since_show={}",
        since_show.map_or_else(
            || "none".to_string(),
            |elapsed| format!("{:.2}s", elapsed.as_secs_f64())
        )
    );
    // 閉じたあとにエンジンだけが残るのを防ぐ (バックグラウンド時のみ)。
    reclaim_idle_engine(app);
}

/// フローティングを閉じたときに、どのエンジンを解放してよいか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reclaim {
    /// どちらも止めない (通常の利用中・翻訳中)。
    KeepAll,
    /// メイン用だけ止める (フローティング用は常駐させる)。
    StopMain,
    /// フローティング用だけ止める (メイン用は表示中/翻訳中で残す)。
    StopFloating,
    /// 両方止める。
    StopBoth,
}

/// 解放の判定。
///
/// - `shared`: メイン用とフローティング用が同じ解決モデルを共有している。
///   このときエンジンは 1 つ (メイン用) だけなので、「フローティング用だけ止める」
///   という選択肢はない。メイン画面が見えている・どちらかの翻訳が動いている間は
///   止めず、誰も使っていないときだけ止める。
/// - `resident`: 常駐ポリシー ([`crate::engine::resident_target`]) の保持先。
///   保持先がフローティング用ならフローティング用を残し、メイン用だけを
///   「メインが隠れていて暇」なときに止める。
/// - どちらでもない: フローティング用を (翻訳中を除き) 止め、メイン用も
///   非表示かつ暇なら止める。
///
/// 共有時も、保持先がメイン用 (`resident = Some(Main)`) ならメイン用を維持する。
/// 「常駐 ON なのにフローティングを閉じるたびにメイン用まで止まる」不具合の修正で、
/// 共有と常駐は排他ではない (案A)。
fn decide_reclaim(
    shared: bool,
    resident: Option<EngineRole>,
    main_visible: bool,
    main_busy: bool,
    floating_busy: bool,
) -> Reclaim {
    let keep_main = resident == Some(EngineRole::Main);
    let keep_floating = resident == Some(EngineRole::Floating);
    if shared {
        // 共有エンジンはメイン用。floating 側の都合だけで止めない。
        if main_visible || main_busy || floating_busy {
            return Reclaim::KeepAll;
        }
        // 常駐の保持先が共有エンジンなら、メイン非表示でも維持する。
        return if keep_main {
            Reclaim::StopFloating
        } else {
            Reclaim::StopBoth
        };
    }
    // 常時起動 OFF のときは、訳していないフローティング用を解放する (M-3)。
    let stop_floating = !keep_floating && !floating_busy;
    let stop_main = !keep_main && !main_visible && !main_busy;
    match (stop_floating, stop_main) {
        (true, true) => Reclaim::StopBoth,
        (false, true) => Reclaim::StopMain,
        (true, false) => Reclaim::StopFloating,
        (false, false) => Reclaim::KeepAll,
    }
}

/// バックグラウンドで、訳すものが無くなったエンジンを止める。
///
/// 「閉じたら VRAM を解放する」というバックグラウンド実行の契約を、ホットキー
/// 経由の起動にも適用する。常駐ポリシー ([`crate::engine::resident_target`]) の
/// 保持先がフローティング用ならそれを残し、メイン用だけを解放する。解決モデルが
/// 同一で共有しているときも、保持先がメイン用なら維持する
/// (`decide_reclaim` の shared 分岐)。
///
/// - メイン画面が見えている (通常の利用中) ときは共有エンジンを止めない
/// - 共有エンジンでメイン/フローティングの翻訳が動いているときも止めない
/// - フローティングの翻訳中はフローティング用を止めない
fn reclaim_idle_engine(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // 可視性はメインスレッドへの問い合わせを含むためロック外で読む。読む前後で
    // 世代を照合し、その間に show/hide が入っていたら、古い可視性で停止せず
    // 共通の再同期に委ねる (Medium 2)。例えば blur-hide が main_visible=false を
    // 読んだ直後に show_main が表示 + 起動予約を完了した場合、ここで古い false の
    // まま Main を止めると、表示中の画面だけエンジンなしになる。
    let snapshot_epoch = {
        let control = state.engine_control.lock();
        control.sync_epoch()
    };
    let main_visible = app
        .get_webview_window(MAIN_LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);
    let search_root = crate::engine::exe_directory();
    // 判断と停止を制御ロック下で行う。新しいジョブの受け付けと直列化され、
    // 「ジョブが来た直後に停止する」競合を避ける。停止だけなので同期予約の
    // invalidate はしない (保留中のポリシー変更を失わない)。
    let control = state.engine_control.lock();
    if control.sync_epoch() != snapshot_epoch {
        // 可視性の読み取り後に状態が変わった。古い値で停止せず、最新状態で
        // 計画し直す (sync_engines_now が新しい ticket を発行する)。
        drop(control);
        crate::commands::sync_engines_now(app);
        return;
    }
    // ホットキー記録中の一時的な enabled=false を通常の OFF と混同しない (M3)。
    let settings = control.effective_settings(state.settings.get());
    let main_busy = state.jobs.has_job_from_other_origin(FLOATING_LABEL);
    let floating_busy = state.jobs.has_running_job_from_origin(FLOATING_LABEL);
    let resident =
        crate::engine::resident_target(&settings, &search_root).map(|target| target.role);
    match decide_reclaim(
        crate::engine::models_shared(&settings, &search_root),
        resident,
        main_visible,
        main_busy,
        floating_busy,
    ) {
        Reclaim::KeepAll => {}
        Reclaim::StopMain => {
            state.engine.kill_now();
            debug_log!("main engine stopped (floating hidden: メインは隠れているため解放)");
        }
        Reclaim::StopFloating => {
            state.floating_engine.kill_now();
            debug_log!("floating engine stopped (floating hidden: 常時起動なし)");
        }
        Reclaim::StopBoth => {
            state.engine.kill_now();
            state.floating_engine.kill_now();
            debug_log!("engines stopped (floating hidden: 訳すものが無いため解放)");
        }
    }
}

/// `handle_window_event` の判定をユニットテストできるようにしたイベント種別。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowEventKind {
    CloseRequested,
    Destroyed,
    Focused(bool),
    Other,
}

/// ウィンドウイベントに対して取るべき動作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowAction {
    /// floating の明示的な close (Alt+F4 など) を止めて hide する。
    PreventFloatingClose,
    /// floating のフォーカス変化を処理する。
    HandleFloatingFocus(bool),
    /// メインの close を止めて隠す (トレイ常駐)。アプリは終了しない。
    HideMainOnClose,
    /// メイン破棄時に隠れた floating を destroy する。
    DestroyFloating,
    Ignore,
}

fn decide_window_action(
    label: &str,
    kind: WindowEventKind,
    background_enabled: bool,
) -> WindowAction {
    match (label, kind) {
        (FLOATING_LABEL, WindowEventKind::CloseRequested) => WindowAction::PreventFloatingClose,
        (FLOATING_LABEL, WindowEventKind::Focused(focused)) => {
            WindowAction::HandleFloatingFocus(focused)
        }
        // トレイ常駐が有効なときは、メインの × をアプリ終了にせずバックグラウンド化する。
        // 終了はトレイメニューの「終了」だけ (決定済み)。
        (MAIN_LABEL, WindowEventKind::CloseRequested) if background_enabled => {
            WindowAction::HideMainOnClose
        }
        (MAIN_LABEL, WindowEventKind::Destroyed) => WindowAction::DestroyFloating,
        _ => WindowAction::Ignore,
    }
}

/// 設定の `background.enabled`。まだ設定を読めない起動直後は既定 (常駐する)。
pub(crate) fn background_enabled(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .map(|state| state.settings.get().background.enabled)
        .unwrap_or_else(|| crate::settings::BackgroundSettings::default().enabled)
}

/// メインを隠したときに維持するエンジン (メイン用, フローティング用)。
///
/// 常駐ポリシーの保持先だけを残し、それ以外は `kill_now` で止める。起動処理中の
/// エンジンも `kill_now` が取り消すため、ここで起動状態を見る必要はない。
/// 共有モデルで常駐 ON のときはメイン用 1 本を残す (案A)。
fn decide_hidden_engines(target: Option<EngineRole>) -> (bool, bool) {
    (
        target == Some(EngineRole::Main),
        target == Some(EngineRole::Floating),
    )
}

/// メイン画面を閉じたときのバックグラウンド化。
///
/// - メインは破棄せず隠す (プロセスとトレイは残る)
/// - フローティングが表示中なら一緒に隠す (決定済み)
/// - メイン由来の翻訳を止める
/// - エンジンは常駐ポリシー ([`crate::engine::resident_target`]) の保持先だけ残す。
///   - 保持先がメイン用 (共有モデルで常駐 ON): この 1 本を維持する。共有エンジンが
///     フローティング翻訳も担うため、ここで止めると次のホットキーで読み込み直しに
///     なる (案A の修正点)。
///   - 保持先がフローティング用 (専用モデルで常駐 ON): 従来どおりメイン用は止め、
///     フローティング用を維持する。
///   - 常駐 OFF: 従来どおり両方止める (大型モデルの VRAM / メモリを解放する)。
fn hide_main_to_tray(app: &AppHandle) {
    // 可視性が変わる前に、変更前の可視性スナップショットを持つ同期 ticket を
    // 無効化する。停止判断より先に効かせ、隠した後に古い visible=true で
    // 起動予約されるのを防ぐ (Medium 2)。
    crate::commands::invalidate_engine_sync(app);
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        if let Err(error) = window.hide() {
            eprintln!("メインウィンドウを隠せません: {error}");
        }
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state.floating.is_shown() {
        hide_with_reason(app, "main-hidden");
    }
    // 隠した画面の翻訳を裏で続けない。
    state.jobs.cancel_current_for_origin(MAIN_LABEL);
    // 停止判断と停止を制御ロック下で行う。ここから解放するまで、新しいジョブの
    // 受け付けや同期の適用は割り込めない。hide 中に予約された同期 ticket も
    // ここで無効化し、隠した後に古い可視性で起動されないようにする (Medium 2)。
    let mut control = state.engine_control.lock();
    control.invalidate_sync();
    // ホットキー記録中の一時的な enabled=false を通常の OFF と混同しない (M3)。
    let settings = control.effective_settings(state.settings.get());
    let target =
        crate::engine::resident_target(&settings, &crate::engine::exe_directory()).map(|t| t.role);
    let (keep_main, keep_floating) = decide_hidden_engines(target);
    if keep_main {
        // 共有モデルの常駐: フローティング翻訳を担う 1 本を維持する。
        debug_log!("main engine kept (resident with shared model)");
    } else {
        state.engine.kill_now();
        debug_log!("main engine stopped (background: VRAM を解放)");
    }
    if keep_floating {
        debug_log!("floating engine kept (resident with dedicated model)");
    } else {
        state.floating_engine.kill_now();
        debug_log!("floating engine stopped (not resident)");
    }
}

/// ウィンドウイベント。lib.rs の `on_window_event` から呼ぶ。
pub fn handle_window_event(window: &tauri::Window, event: &WindowEvent) {
    let kind = match event {
        WindowEvent::CloseRequested { .. } => WindowEventKind::CloseRequested,
        WindowEvent::Destroyed => WindowEventKind::Destroyed,
        WindowEvent::Focused(focused) => WindowEventKind::Focused(*focused),
        _ => WindowEventKind::Other,
    };

    let background = background_enabled(window.app_handle());
    match decide_window_action(window.label(), kind, background) {
        WindowAction::PreventFloatingClose => {
            // Alt+F4 などで floating を破棄されると、以降 hotkey が無反応になる
            // (get_webview_window が None になる)。close を止めて hide する。
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
            }
            hide(window.app_handle(), "window-close");
        }
        WindowAction::HandleFloatingFocus(focused) => {
            handle_floating_focus(window.app_handle(), focused);
        }
        WindowAction::HideMainOnClose => {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
            }
            hide_main_to_tray(window.app_handle());
        }
        WindowAction::DestroyFloating => {
            // メインが破棄されたら、隠れた floating を破棄してアプリを終了できる
            // ようにする。prevent_close があるため close() では止められるので
            // destroy() を使う。destroy() は proxy 経由でイベントループへ送られる
            // (main スレッドで直接実行されない) ため、ここから呼んでも安全。
            //
            // トレイ常駐が有効なときはメインを破棄しないため、通常はここへ来ない。
            // (`background.enabled = false` のときだけ通る従来の経路)
            if let Some(floating) = window.app_handle().get_webview_window(FLOATING_LABEL) {
                if let Err(error) = floating.destroy() {
                    eprintln!("フローティングウィンドウを破棄できません: {error}");
                }
            }
            // destroy() だけでは Tauri が ExitRequested を出さず、イベントループと
            // プロセスが残る。RequestExit (proxy 経由) で明示的に終了を要求し、
            // lib.rs の ExitRequested ハンドラで llama-server も止める。
            window.app_handle().exit(0);
        }
        WindowAction::Ignore => {}
    }
}

/// フォーカス変化の処理。
///
/// Windows の Tauri は WebView2 コントローラの GotFocus/LostFocus を
/// `WindowEvent::Focused` として合成する。これは「ウィンドウが非アクティブになった」
/// より広く、表示直後のフォーカス揺れでも飛ぶ。すぐには閉じず、少し待って
/// フォーカスが戻らなかったときだけ閉じる。
fn handle_floating_focus(app: &AppHandle, focused: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // どのフォーカスイベントでも、予約済みの遅延 hide は取り消す。
    let epoch = state.floating.bump_focus_epoch();

    if focused {
        debug_log!("focus gained");
        return;
    }
    if !state.floating.is_shown() {
        debug_log!("blur ignored (window is hidden)");
        return;
    }

    let now = Instant::now();
    if let Some(remaining) = state.floating.no_selection_remaining(now) {
        // 案内の表示中は閉じない。フロントエンドが 2.5 秒で閉じるが、
        // 念のため案内期間が終わったら閉じるチェックも予約する。
        debug_log!("blur ignored (noSelection message is showing)");
        schedule_blur_hide(app, epoch, remaining + BLUR_HIDE_DELAY);
        return;
    }
    if let Some(remaining) = state.floating.show_grace_remaining(now) {
        debug_log!("blur ignored (within show grace)");
        // 猶予が終わってもフォーカスが戻っていなければ閉じる。
        schedule_blur_hide(app, epoch, remaining + BLUR_HIDE_DELAY);
        return;
    }

    schedule_blur_hide(app, epoch, BLUR_HIDE_DELAY);
}

fn schedule_blur_hide(app: &AppHandle, epoch: u64, delay: Duration) {
    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(delay);
        let Some(state) = app_handle.try_state::<AppState>() else {
            return;
        };
        if state.floating.focus_epoch() != epoch {
            debug_log!("blur hide cancelled (focus changed)");
            return;
        }
        if !state.floating.is_shown() {
            return;
        }
        let Some(window) = app_handle.get_webview_window(FLOATING_LABEL) else {
            return;
        };
        if window.is_focused().unwrap_or(false) {
            debug_log!("blur hide cancelled (window is focused again)");
            return;
        }
        hide_with_reason(&app_handle, "focus-lost");
    });
}

// ---------------------------------------------------------------------------
// 選択テキストの取得フロー
// ---------------------------------------------------------------------------

/// タイムアウト時に一時解除する修飾子。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ForcedModifierRelease {
    shift: bool,
    alt: bool,
}

impl ForcedModifierRelease {
    fn any(self) -> bool {
        self.shift || self.alt
    }
}

/// 押されている Shift / Alt のうち、一時解除する対象を決める。
/// Ctrl は Ctrl+C に必要なので対象外。ホットキーのキー自体も対象外。
fn forced_modifier_release(shift_down: bool, alt_down: bool) -> ForcedModifierRelease {
    ForcedModifierRelease {
        shift: shift_down,
        alt: alt_down,
    }
}

/// 現在押されている Shift / Alt を調べて、一時解除の対象を得る。
#[cfg(windows)]
fn current_forced_modifier_release() -> ForcedModifierRelease {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_MENU, VK_SHIFT};

    forced_modifier_release(is_vk_pressed(VK_SHIFT), is_vk_pressed(VK_MENU))
}

#[cfg(not(windows))]
fn current_forced_modifier_release() -> ForcedModifierRelease {
    ForcedModifierRelease::default()
}

/// Ctrl+C でクリップボードが更新されたかと、読み取った内容から選択テキストを
/// 採用するかを決める。
///
/// クリップボードに既に入っているのと同じ文字列を選択した場合、テキスト比較では
/// 「変化なし」と誤判定する。呼び出し側は GetClipboardSequenceNumber の変化で
/// コピーの成否を判定し、その結果を `copied` に渡す。
fn decide_selected(copied: bool, captured: Option<&str>) -> Option<String> {
    if !copied {
        return None;
    }
    let text = captured?;
    if text.trim().is_empty() {
        return None;
    }
    Some(text.to_string())
}

fn capture_and_show(app: &AppHandle, trigger_started: Instant, hotkey_vk: Option<u16>) {
    let Some(window) = app.get_webview_window(FLOATING_LABEL) else {
        return;
    };

    // 位置は発火した瞬間のカーソルで決める (取得中にカーソルが動いても追随しない)。
    let cursor = cursor_position();

    // 前景アプリの選択テキストを取るため、クリップボードを退避してから Ctrl+C を送る。
    let backup = read_clipboard_text();
    let released_in_time = wait_for_hotkey_keys_release(hotkey_vk, KEY_RELEASE_WAIT_LIMIT);
    // 押しっぱなしで時間切れになった場合、そのまま Ctrl+C を送るとホットキーと
    // 同じ組み合わせ (例: Ctrl+Alt+C) になり、ブラウザ等ではコピーされない。
    // Shift / Alt だけ一時解除して Ctrl+C を届かせる (Ctrl はコピーに必要)。
    let forced_release = if released_in_time {
        ForcedModifierRelease::default()
    } else {
        current_forced_modifier_release()
    };
    if forced_release.any() {
        debug_log!(
            "modifier hold timeout: force-releasing shift={} alt={}",
            forced_release.shift,
            forced_release.alt
        );
        send_forced_modifier_state(forced_release, true);
    }

    // 既に同じテキストがクリップボードにあっても、Ctrl+C でコピーが起きたかを
    // シーケンス番号の変化で判定する (テキスト比較では noSelection と誤判定する)。
    let sequence_before = clipboard_sequence_number();
    send_copy_shortcut();
    std::thread::sleep(CLIPBOARD_SETTLE);
    let sequence_after = clipboard_sequence_number();
    let captured = read_clipboard_text();
    let selected = decide_selected(sequence_after != sequence_before, captured.as_deref());

    if forced_release.any() {
        // capture 開始時に押されていた修飾子の状態へ戻す。capture 中にユーザーが
        // 離した場合は down が残るリスクがあるが、capture は数百 ms なので許容する
        // (物理状態を問い合わせても、合成 keyup 後は up に見えるため判別できない)。
        send_forced_modifier_state(forced_release, false);
        debug_log!("modifiers restored after copy");
    }

    // 退避がテキストだったときだけ戻す。画像などは復元しない (README に明記)。
    if let Some(text) = backup.as_deref().filter(|text| !text.is_empty()) {
        if !write_clipboard_text(text) {
            eprintln!("クリップボードの退避を戻せませんでした");
        }
    }

    // 訳すものがあったときだけ、止まっているエンジンを起こす。
    // `ensure_engine_started` は解決モデルの共有を見て、実際に使う役割
    // (メイン用 / フローティング用) のエンジンを選ぶ。起動中は engine:status
    // (starting / loading) が全ウィンドウへ流れ、floating は受け取った
    // `engine_role` の status を見て「エンジンを起動しています」を出してから
    // 翻訳に入る。常時起動が ON ならここは何もしない (モデル読み込み待ちなし)。
    // 選択なし (noSelection) のときは起動しない (無駄に VRAM を使わない)。
    //
    // ここはまだ `window.show()` / `state.floating.set_shown(true)` の前。
    // 起動完了後の後追い同期は「ポリシー/モデルが変わったとき」だけ走るので、
    // この時点で floating_shown=false でも、起動したエンジンは止められない。
    if selected.is_some() {
        crate::ensure_engine_started(app, EngineRole::Floating);
    }

    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let text_debug = match &selected {
        Some(text) => format!("Some({} chars)", text.chars().count()),
        None => "None".to_string(),
    };
    let no_selection = selected.is_none();
    // 実際に翻訳を処理するエンジンの役割。共有モデルのときは "main" になるので、
    // フローティング画面はその役割の status を見て起動中表示を出す。
    let engine_role = crate::engine::serving_role(
        EngineRole::Floating,
        &state.settings.get(),
        &crate::engine::exe_directory(),
    );
    let payload = CaptureEvent {
        text: selected,
        target_lang: state.floating.target_lang(),
        engine_role,
    };

    // リサイズ可能なので、位置計算には現在のウィンドウの実サイズ (物理ピクセル) を使う。
    // 取得できないときだけ既定サイズへフォールバックする。
    let scale = window.scale_factor().unwrap_or(1.0);
    let (mut width, mut height) = match window.outer_size() {
        Ok(size) => (size.width as i32, size.height as i32),
        Err(error) => {
            debug_log!("outer_size unavailable, falling back to default size: {error}");
            (
                (WINDOW_WIDTH * scale).round() as i32,
                (WINDOW_HEIGHT * scale).round() as i32,
            )
        }
    };
    let work_area = cursor.and_then(monitor_work_area);
    let mut position_log = "not-set".to_string();
    if let (Some((x, y)), Some(work)) = (cursor, work_area) {
        // 対象モニタの作業領域に収まるサイズへクランプする。別モニタで大きくした
        // ウィンドウをより小さいモニタ上で出すと、outer_size が作業領域を超え、
        // 位置だけのクランプでは右下のつまみが画面外に出る。
        // ただし place_window は位置を返すだけでウィンドウを縮めないため、
        // 収まらないときは実サイズも作業領域に合わせる (収まるときはリサイズ後の
        // ユーザーのサイズをそのまま保つ)。
        let (fitted_width, fitted_height) = clamp_size_to_work_area(width, height, work);
        if (fitted_width, fitted_height) != (width, height) {
            if let Err(error) = window.set_size(tauri::PhysicalSize::new(
                fitted_width as u32,
                fitted_height as u32,
            )) {
                eprintln!("フローティングウィンドウを作業領域に収められません: {error}");
            } else {
                debug_log!(
                    "window shrunk to fit work area: {width}x{height} -> {fitted_width}x{fitted_height}"
                );
            }
            width = fitted_width;
            height = fitted_height;
        }
        let (left, top) = place_window(x, y, width, height, work);
        position_log = format!("{left},{top} {width}x{height}");
        if let Err(error) = window.set_position(tauri::Position::Physical(
            tauri::PhysicalPosition::new(left, top),
        )) {
            eprintln!("フローティングウィンドウを移動できません: {error}");
        }
    }

    // show 直後のフォーカス揺れを吸収する。noSelection は約 2.5 秒見せる設計なので、
    // その間の blur も無視する (フロントエンドが自動で閉じる)。
    let now = Instant::now();
    state
        .floating
        .set_no_selection_guard(no_selection.then(|| now + NO_SELECTION_BLUR_GRACE));
    let display_epoch = state.floating.mark_show_start(now, no_selection);

    let show_started = Instant::now();
    match window.show() {
        Ok(()) => state.floating.set_shown(true),
        Err(error) => {
            eprintln!("フローティングウィンドウを表示できません: {error}");
            state.floating.set_shown(false);
        }
    }
    match window.set_focus() {
        Ok(()) => debug_log!("set_focus ok"),
        Err(error) => eprintln!("フローティングウィンドウにフォーカスできません: {error}"),
    }

    let visible_after_show = window.is_visible().unwrap_or(false);
    let focused_after_show = window.is_focused().unwrap_or(false);
    debug_log!(
        "capture: text={text_debug} clipboard_seq={sequence_before}->{sequence_after} visible={visible_after_show} focused={focused_after_show} pos={position_log} show_epoch={display_epoch} since_hotkey={:?} shown_in={:?}",
        trigger_started.elapsed(),
        show_started.elapsed()
    );

    if let Err(error) = app.emit(CAPTURE_EVENT, payload) {
        eprintln!("{CAPTURE_EVENT} を送れません: {error}");
    }
}

// ---------------------------------------------------------------------------
// Windows API (unsafe はこの範囲だけ)
// ---------------------------------------------------------------------------

/// クリップボードの更新回数。Ctrl+C で内容が変わったかの判定に使う。
#[cfg(windows)]
fn clipboard_sequence_number() -> u32 {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;

    // SAFETY: クリップボードの更新回数を読むだけ。
    unsafe { GetClipboardSequenceNumber() }
}

#[cfg(not(windows))]
fn clipboard_sequence_number() -> u32 {
    0
}

/// クリップボードを開く。他プロセスが掴んでいることがあるので少しリトライする。
#[cfg(windows)]
fn open_clipboard() -> bool {
    use windows_sys::Win32::System::DataExchange::OpenClipboard;

    for _ in 0..CLIPBOARD_OPEN_RETRIES {
        // SAFETY: 所有権なし (NULL) でクリップボードを開くだけ。成功したら必ず
        // 呼び出し側が CloseClipboard する。
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            return true;
        }
        std::thread::sleep(CLIPBOARD_RETRY_DELAY);
    }
    false
}

/// クリップボードのテキスト (CF_UNICODETEXT) を読む。無ければ `None`。
#[cfg(windows)]
fn read_clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    if !open_clipboard() {
        return None;
    }

    let mut result = None;
    // SAFETY: 開いたクリップボードの間だけハンドルを使う。GetClipboardData が返す
    // ハンドルはクリップボード所有のもので、解放してはいけない。GlobalLock した
    // ポインタは CloseClipboard 前に GlobalUnlock する。
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if !handle.is_null() {
                let pointer = GlobalLock(handle) as *const u16;
                if !pointer.is_null() {
                    let mut len = 0;
                    // 終端 (0) を探す。壊れたデータでも上限で打ち切る。
                    while len < MAX_CLIPBOARD_UNITS && *pointer.add(len) != 0 {
                        len += 1;
                    }
                    if len < MAX_CLIPBOARD_UNITS {
                        let units = std::slice::from_raw_parts(pointer, len);
                        result = Some(String::from_utf16_lossy(units));
                    }
                    GlobalUnlock(handle);
                }
            }
        }
        CloseClipboard();
    }
    result
}

/// クリップボードへテキスト (CF_UNICODETEXT) を書く。
#[cfg(windows)]
fn write_clipboard_text(text: &str) -> bool {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    if !open_clipboard() {
        return false;
    }

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();

    // SAFETY: クリップボードを開いた状態で、自前で確保した HGLOBAL に UTF-16 を
    // コピーして SetClipboardData に渡す。成功すると所有権はシステムへ移るので
    // 解放しない。失敗時だけ GlobalFree する。
    unsafe {
        if EmptyClipboard() == 0 {
            CloseClipboard();
            return false;
        }
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if handle.is_null() {
            CloseClipboard();
            return false;
        }
        let pointer = GlobalLock(handle) as *mut u16;
        if pointer.is_null() {
            GlobalFree(handle);
            CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
        GlobalUnlock(handle);

        if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
            GlobalFree(handle);
            CloseClipboard();
            return false;
        }
        CloseClipboard();
    }
    true
}

/// ホットキーのキーと修飾子 (Ctrl / Shift / Alt) が離れるまで待つ。
/// 制限時間内にすべて離れれば true、タイムアウトなら false。
#[cfg(windows)]
fn wait_for_hotkey_keys_release(hotkey_vk: Option<u16>, limit: Duration) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_MENU, VK_SHIFT};

    let started = Instant::now();
    loop {
        let any_pressed = is_vk_pressed(VK_CONTROL)
            || is_vk_pressed(VK_SHIFT)
            || is_vk_pressed(VK_MENU)
            || hotkey_vk.is_some_and(is_vk_pressed);
        if !any_pressed {
            return true;
        }
        if started.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 現在その仮想キーが押されているか。
#[cfg(windows)]
fn is_vk_pressed(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    // SAFETY: GetAsyncKeyState は現在のキー状態を読むだけ。
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}

/// 押しっぱなしの Shift / Alt を一時解除 (key_up=true) または復元 (false) する。
#[cfg(windows)]
fn send_forced_modifier_state(release: ForcedModifierRelease, key_up: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_MENU, VK_SHIFT};

    if release.shift {
        send_vk_key(VK_SHIFT, key_up);
    }
    if release.alt {
        send_vk_key(VK_MENU, key_up);
    }
}

/// VK ベースの keydown / keyup を 1 つ送る。
#[cfg(windows)]
fn send_vk_key(vk: u16, key_up: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };

    let flags = if key_up { KEYEVENTF_KEYUP } else { 0 };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    // SAFETY: 長さ 1 の INPUT を正しいサイズで渡すだけ。ポインタは保持しない。
    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        eprintln!("修飾キーの合成入力を送れませんでした (vk=0x{vk:02X})");
    }
}

/// 前景ウィンドウへ Ctrl+C を送る。
///
/// `KEYEVENTF_SCANCODE` を使うのは、仮想キーだけの送信だとキーボード配列や
/// アプリによって取りこぼされることがあるため。スキャンコードは
/// `MapVirtualKeyW` で仮想キーから引く。
#[cfg(windows)]
fn send_copy_shortcut() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, VK_C, VK_LCONTROL,
    };

    fn key_input(scan: u16, key_up: bool) -> INPUT {
        let mut flags = KEYEVENTF_SCANCODE;
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    // SAFETY: MapVirtualKeyW は変換表を引くだけ。SendInput は INPUT 配列の
    // 長さとサイズを正しく渡す。どちらもプロセス外の状態を書き換えるだけで
    // ポインタを保持しない。
    unsafe {
        let control_scan = MapVirtualKeyW(VK_LCONTROL as u32, MAPVK_VK_TO_VSC) as u16;
        let copy_scan = MapVirtualKeyW(VK_C as u32, MAPVK_VK_TO_VSC) as u16;
        if control_scan == 0 || copy_scan == 0 {
            eprintln!("Ctrl+C のスキャンコードを取得できません");
            return;
        }

        let inputs = [
            key_input(control_scan, false),
            key_input(copy_scan, false),
            key_input(copy_scan, true),
            key_input(control_scan, true),
        ];
        let sent = SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
        if sent != inputs.len() as u32 {
            eprintln!("選択テキストの取得 (Ctrl+C) を送れませんでした");
        }
    }
}

/// カーソルの物理ピクセル座標。
#[cfg(windows)]
fn cursor_position() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: POINT への書き込み先を渡すだけ。
    if unsafe { GetCursorPos(&mut point) } == 0 {
        None
    } else {
        Some((point.x, point.y))
    }
}

/// カーソルがあるモニタの作業領域 (タスクバーを除く)。
#[cfg(windows)]
fn monitor_work_area(point: (i32, i32)) -> Option<WorkArea> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    let point = POINT {
        x: point.0,
        y: point.1,
    };
    // SAFETY: 入力は値だけ、出力は初期化済みの MONITORINFO へ書かせる。
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: windows_sys::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: windows_sys::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        Some(WorkArea {
            left: info.rcWork.left,
            top: info.rcWork.top,
            right: info.rcWork.right,
            bottom: info.rcWork.bottom,
        })
    }
}

// Windows 以外ではビルドだけ通るスタブ。NuKa の配布対象は Windows のみ。
#[cfg(not(windows))]
fn wait_for_hotkey_keys_release(_hotkey_vk: Option<u16>, _limit: Duration) -> bool {
    true
}

#[cfg(not(windows))]
fn send_forced_modifier_state(_release: ForcedModifierRelease, _key_up: bool) {}

#[cfg(not(windows))]
fn send_copy_shortcut() {}

#[cfg(not(windows))]
fn read_clipboard_text() -> Option<String> {
    None
}

#[cfg(not(windows))]
fn write_clipboard_text(_text: &str) -> bool {
    false
}

#[cfg(not(windows))]
fn cursor_position() -> Option<(i32, i32)> {
    None
}

#[cfg(not(windows))]
fn monitor_work_area(_point: (i32, i32)) -> Option<WorkArea> {
    None
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkey_parses() {
        let spec = parse_hotkey(DEFAULT_HOTKEY).expect("既定ホットキーは有効");
        assert!(spec.ctrl);
        assert!(!spec.alt);
        assert!(spec.shift);
        assert_eq!(spec.key, 'T');
        assert_eq!(
            normalize_hotkey(DEFAULT_HOTKEY).as_deref(),
            Some("Ctrl+Shift+T")
        );
    }

    #[test]
    fn hotkey_modifiers_are_normalized_and_deduplicated() {
        assert_eq!(
            normalize_hotkey(" shift + ctrl + t ").as_deref(),
            Some("Ctrl+Shift+T")
        );
        assert_eq!(
            normalize_hotkey("CTRL+ALT+SHIFT+0").as_deref(),
            Some("Ctrl+Alt+Shift+0")
        );
        assert_eq!(normalize_hotkey("ctrl+ctrl+t").as_deref(), Some("Ctrl+T"));
        assert_eq!(
            normalize_hotkey("Control+Shift+1").as_deref(),
            Some("Ctrl+Shift+1")
        );
        assert_eq!(normalize_hotkey("shift+a").as_deref(), Some("Shift+A"));
    }

    #[test]
    fn invalid_hotkeys_are_rejected() {
        for value in [
            "",
            " ",
            "T",
            "Ctrl",
            "Ctrl+Shift",
            "Ctrl+Shift+F1",
            "Ctrl+Shift+TT",
            "Ctrl+Shift+ ",
            "Ctrl++T",
            "Ctrl+Shift+T+U",
            "Meta+T",
            "Ctrl+Shift+é",
            "Ctrl+Shift+-",
        ] {
            assert!(
                normalize_hotkey(value).is_none(),
                "{value:?} は無効であるべき"
            );
        }
    }

    #[test]
    fn hotkey_maps_to_the_plugin_shortcut() {
        let shortcut = parse_hotkey("Ctrl+Shift+T")
            .unwrap()
            .to_shortcut()
            .expect("変換できる");
        assert_eq!(
            shortcut,
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyT)
        );

        let shortcut = parse_hotkey("Alt+1")
            .unwrap()
            .to_shortcut()
            .expect("変換できる");
        assert_eq!(shortcut, Shortcut::new(Some(Modifiers::ALT), Code::Digit1));
    }

    #[test]
    #[cfg(windows)]
    fn shortcut_vk_maps_top_row_keys() {
        let shortcut = |code: Code| Shortcut::new(Some(Modifiers::CONTROL), code);
        assert_eq!(shortcut_vk(&shortcut(Code::KeyA)), Some(0x41));
        assert_eq!(shortcut_vk(&shortcut(Code::KeyC)), Some(0x43));
        assert_eq!(shortcut_vk(&shortcut(Code::KeyZ)), Some(0x5A));
        assert_eq!(shortcut_vk(&shortcut(Code::Digit0)), Some(0x30));
        assert_eq!(shortcut_vk(&shortcut(Code::Digit1)), Some(0x31));
        assert_eq!(shortcut_vk(&shortcut(Code::Digit9)), Some(0x39));
        // パーサが受け付けないキーは VK に変換しない。
        assert_eq!(shortcut_vk(&shortcut(Code::F1)), None);
    }

    #[test]
    fn forced_release_targets_only_shift_and_alt() {
        assert_eq!(
            forced_modifier_release(false, false),
            ForcedModifierRelease::default()
        );
        assert!(!forced_modifier_release(false, false).any());

        assert_eq!(
            forced_modifier_release(true, false),
            ForcedModifierRelease {
                shift: true,
                alt: false
            }
        );
        assert_eq!(
            forced_modifier_release(false, true),
            ForcedModifierRelease {
                shift: false,
                alt: true
            }
        );
        let both = forced_modifier_release(true, true);
        assert!(both.shift);
        assert!(both.alt);
        assert!(both.any());
    }

    fn work_area(left: i32, top: i32, right: i32, bottom: i32) -> WorkArea {
        WorkArea {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn minimum_window_size_leaves_room_for_the_header_and_body() {
        // レイアウト前提の不変条件なので、コンパイル時に検査する。
        const {
            // 既定サイズより小さい (起動直後からつまみで広げられる)。
            assert!(MIN_WINDOW_WIDTH < WINDOW_WIDTH);
            assert!(MIN_WINDOW_HEIGHT < WINDOW_HEIGHT);
            // ヘッダーは 1 行 (約 40px)。これ以下に縮むと本文が隠れる。
            assert!(MIN_WINDOW_HEIGHT > 40.0);
            // 最小幅でもヘッダーのボタン列が 1 行に収まる (280px でほぼ一杯)。
            assert!(MIN_WINDOW_WIDTH >= 280.0);
        };
    }

    #[test]
    fn window_opens_below_the_cursor_and_centered() {
        let position = place_window(500, 300, 200, 100, work_area(0, 0, 1000, 800));
        assert_eq!(position, (400, 316));
    }

    #[test]
    fn window_flips_above_the_cursor_near_the_bottom() {
        let position = place_window(950, 790, 200, 100, work_area(0, 0, 1000, 800));
        // 右端はクランプ、下はカーソルの上へ回る。
        assert_eq!(position, (800, 674));
    }

    #[test]
    fn window_is_clamped_to_a_negative_monitor() {
        let position = place_window(-1900, 10, 420, 300, work_area(-1920, 0, 0, 1080));
        assert_eq!(position, (-1920, 26));
    }

    #[test]
    fn window_is_clamped_when_the_work_area_is_smaller_than_it() {
        let position = place_window(150, 100, 420, 300, work_area(0, 0, 300, 200));
        assert_eq!(position, (0, 0));
    }

    #[test]
    fn oversized_window_size_is_clamped_to_the_work_area() {
        let work = work_area(0, 0, 800, 600);
        // 作業領域より大きいサイズは縦横とも作業領域まで縮める。
        assert_eq!(clamp_size_to_work_area(1200, 900, work), (800, 600));
        // 収まるサイズは変えない (リサイズ後のサイズを保つ)。
        assert_eq!(clamp_size_to_work_area(420, 240, work), (420, 240));
        // 負座標のモニタでも幅・高さで判定する。
        assert_eq!(
            clamp_size_to_work_area(2000, 1200, work_area(-1920, 0, 0, 1080)),
            (1920, 1080)
        );
        // 不正なサイズでも 1px 以上を返す (place_window の width.max(1) と整合)。
        assert_eq!(clamp_size_to_work_area(0, -10, work), (1, 1));
        // 作業領域が極端に小さくても 1px を返し、パニックしない。
        assert_eq!(
            clamp_size_to_work_area(1200, 900, work_area(0, 0, 0, 0)),
            (1, 1)
        );
    }

    #[test]
    fn target_language_accepts_only_reasonable_codes() {
        let state = FloatingState::new();
        assert_eq!(state.target_lang(), "ja");
        state.set_target_lang("zh-hant");
        assert_eq!(state.target_lang(), "zh-hant");
        state.set_target_lang("  en  ");
        assert_eq!(state.target_lang(), "en");
        // 不正な値は無視して直前の値を保つ。
        state.set_target_lang("");
        state.set_target_lang("../../evil");
        state.set_target_lang("abcdefghijklmnopqrstuvwxyz");
        assert_eq!(state.target_lang(), "en");
    }

    #[test]
    fn show_grace_absorbs_immediate_focus_loss() {
        let state = FloatingState::new();
        let now = Instant::now();
        assert!(state.show_grace_remaining(now).is_none());

        state.mark_show_start(now, false);
        assert_eq!(state.show_grace_remaining(now), Some(SHOW_FOCUS_GRACE));
        assert!(state
            .show_grace_remaining(now + SHOW_FOCUS_GRACE - Duration::from_millis(1))
            .is_some());
        assert!(state.show_grace_remaining(now + SHOW_FOCUS_GRACE).is_none());

        // トグル誤爆の防止は show から 1 秒。
        assert!(state.shown_recently(now + Duration::from_millis(500), HOTKEY_TOGGLE_GUARD));
        assert!(!state.shown_recently(now + HOTKEY_TOGGLE_GUARD, HOTKEY_TOGGLE_GUARD));
    }

    #[test]
    fn no_selection_guard_keeps_the_message_visible() {
        let state = FloatingState::new();
        let now = Instant::now();
        assert!(!state.no_selection_active(now));
        assert!(state.no_selection_remaining(now).is_none());

        state.set_no_selection_guard(Some(now + NO_SELECTION_BLUR_GRACE));
        assert!(state.no_selection_active(now + Duration::from_secs(2)));
        assert!(state
            .no_selection_remaining(now + Duration::from_secs(2))
            .is_some());
        assert!(!state.no_selection_active(now + NO_SELECTION_BLUR_GRACE));
        assert!(state
            .no_selection_remaining(now + NO_SELECTION_BLUR_GRACE)
            .is_none());

        state.set_no_selection_guard(None);
        assert!(!state.no_selection_active(now));
    }

    #[test]
    fn repeated_hotkey_presses_are_debounced() {
        let state = FloatingState::new();
        let now = Instant::now();
        assert!(state.should_handle_press(now));
        assert!(!state.should_handle_press(now + Duration::from_millis(100)));
        assert!(!state.should_handle_press(now + Duration::from_millis(250)));
        // 直前の押下から HOTKEY_PRESS_DEBOUNCE 経過したら次を受け付ける。
        assert!(state.should_handle_press(now + Duration::from_millis(550)));
    }

    #[test]
    fn focus_epoch_changes_on_every_focus_event_and_show() {
        let state = FloatingState::new();
        let first = state.bump_focus_epoch();
        assert_eq!(state.focus_epoch(), first);
        let second = state.bump_focus_epoch();
        assert!(second > first);
        state.mark_show_start(Instant::now(), false);
        assert!(state.focus_epoch() > second);
    }

    #[test]
    fn display_state_tracks_no_selection_and_show_epoch() {
        let state = FloatingState::new();
        let now = Instant::now();
        let first = state.mark_show_start(now, true);
        assert_eq!(state.show_epoch(), first);
        assert!(state.is_no_selection_display());
        assert_eq!(state.time_since_show(now), Some(Duration::ZERO));

        let second = state.mark_show_start(now, false);
        assert!(second > first);
        assert!(!state.is_no_selection_display());

        state.set_shown(true);
        state.end_display();
        assert!(!state.is_shown());
        assert!(!state.is_no_selection_display());
        assert!(state.show_epoch() > second);
    }

    #[test]
    fn auto_hide_only_applies_to_the_current_no_selection_display() {
        use AutoHideDecision::{Defer, Hide, Ignore};

        // 選択ありの表示中に届いた auto-hide は、前の表示の古いタイマー。
        assert_eq!(
            decide_auto_hide(false, Some(Duration::from_millis(300))),
            Ignore
        );

        // noSelection でも早すぎる auto-hide は最低表示時間まで遅らせる。
        assert_eq!(
            decide_auto_hide(true, Some(Duration::from_millis(300))),
            Defer(NO_SELECTION_MIN_VISIBLE - Duration::from_millis(300))
        );
        assert_eq!(decide_auto_hide(true, Some(NO_SELECTION_MIN_VISIBLE)), Hide);
        assert_eq!(decide_auto_hide(true, Some(Duration::from_secs(3))), Hide);
        assert_eq!(decide_auto_hide(true, None), Hide);
    }

    #[test]
    fn hide_origin_is_sanitized_for_logs() {
        assert_eq!(sanitize_origin("auto-hide"), "auto-hide");
        assert_eq!(sanitize_origin(" close-button "), "close-button");
        assert_eq!(sanitize_origin("a_b-1"), "a_b-1");

        let long = "x".repeat(33);
        for value in ["", "   ", "auto hide", "あ", long.as_str()] {
            assert_eq!(sanitize_origin(value), "unknown", "{value:?}");
        }
    }

    #[test]
    fn selection_requires_a_successful_copy_and_non_empty_text() {
        assert_eq!(
            decide_selected(true, Some("hello")),
            Some("hello".to_string())
        );
        assert_eq!(
            decide_selected(true, Some("  hello  ")),
            Some("  hello  ".to_string())
        );
        assert_eq!(decide_selected(true, Some("   ")), None);
        assert_eq!(decide_selected(true, None), None);
        // シーケンスが変わっていなければ、同じ文字列でも選択なし扱い。
        assert_eq!(decide_selected(false, Some("hello")), None);
        assert_eq!(decide_selected(false, None), None);
    }

    #[test]
    fn window_actions_cover_close_focus_and_main_destroyed() {
        use WindowAction::{
            DestroyFloating, HandleFloatingFocus, HideMainOnClose, Ignore, PreventFloatingClose,
        };
        use WindowEventKind::{CloseRequested, Destroyed, Focused, Other};

        for background in [true, false] {
            assert_eq!(
                decide_window_action(FLOATING_LABEL, CloseRequested, background),
                PreventFloatingClose
            );
            assert_eq!(
                decide_window_action(FLOATING_LABEL, Focused(true), background),
                HandleFloatingFocus(true)
            );
            assert_eq!(
                decide_window_action(FLOATING_LABEL, Focused(false), background),
                HandleFloatingFocus(false)
            );
            assert_eq!(
                decide_window_action(FLOATING_LABEL, Destroyed, background),
                Ignore
            );
            assert_eq!(
                decide_window_action(FLOATING_LABEL, Other, background),
                Ignore
            );

            // メインを破棄されたときの後始末は従来どおり。
            assert_eq!(
                decide_window_action(MAIN_LABEL, Destroyed, background),
                DestroyFloating
            );
            assert_eq!(
                decide_window_action(MAIN_LABEL, Focused(false), background),
                Ignore
            );
            assert_eq!(decide_window_action("other", Destroyed, background), Ignore);
            assert_eq!(
                decide_window_action("other", CloseRequested, background),
                Ignore
            );
        }

        // トレイ常駐が有効なら、メインの × は「隠す」。
        assert_eq!(
            decide_window_action(MAIN_LABEL, CloseRequested, true),
            HideMainOnClose
        );
        // 常駐しない設定では、従来どおり閉じて終了する (Destroyed で後始末)。
        assert_eq!(
            decide_window_action(MAIN_LABEL, CloseRequested, false),
            Ignore
        );
    }

    #[test]
    fn shown_flag_tracks_explicit_show_and_hide() {
        let state = FloatingState::new();
        assert!(!state.is_shown());
        state.set_shown(true);
        assert!(state.is_shown());
        state.set_shown(false);
        assert!(!state.is_shown());
    }

    #[test]
    fn reclaim_keeps_the_resident_engine_and_frees_the_main_one() {
        use EngineRole::{Floating, Main};
        use Reclaim::{KeepAll, StopBoth, StopFloating, StopMain};

        let resident = Some(Floating);

        // 共有していない場合 (専用モデルあり/常時起動の有無)。

        // 常時起動 ON: メインが見えていれば両方残す。隠れていて暇ならメインだけ止める。
        assert_eq!(decide_reclaim(false, resident, true, false, false), KeepAll);
        assert_eq!(decide_reclaim(false, resident, true, true, false), KeepAll);
        assert_eq!(
            decide_reclaim(false, resident, false, false, false),
            StopMain
        );
        // メインの翻訳中はメイン用を残す。
        assert_eq!(decide_reclaim(false, resident, false, true, false), KeepAll);

        // 常時起動 OFF: メインが表示中でもフローティング用は止める (M-3)。
        assert_eq!(
            decide_reclaim(false, None, true, false, false),
            StopFloating
        );
        assert_eq!(
            decide_reclaim(false, None, false, true, false),
            StopFloating
        );
        // メインが隠れていて暇なら両方止める (v1.3.2 と同じ)。
        assert_eq!(decide_reclaim(false, None, false, false, false), StopBoth);
        // フローティングの翻訳中はフローティング用を止めない。
        assert_eq!(decide_reclaim(false, None, true, false, true), KeepAll);
        assert_eq!(decide_reclaim(false, None, false, false, true), StopMain);

        // 保持先がメイン用になることは共有時だけ (非共有では起こらないが、
        // 渡されたらメイン用を残す)。
        assert_eq!(
            decide_reclaim(false, Some(Main), false, false, false),
            StopFloating
        );
    }

    #[test]
    fn hiding_main_keeps_only_the_resident_engine() {
        use EngineRole::{Floating, Main};

        // 常駐 OFF: 従来どおり両方止める (大型モデルの VRAM を解放)。
        assert_eq!(decide_hidden_engines(None), (false, false));
        // 専用モデルで常駐 ON: フローティング用だけ残す。
        assert_eq!(decide_hidden_engines(Some(Floating)), (false, true));
        // 共有モデルで常駐 ON: メイン用 1 本を残す (ここが不具合の修正点)。
        assert_eq!(decide_hidden_engines(Some(Main)), (true, false));
        // 起動処理中でも判断は同じ。残さない側は kill_now が起動も取り消す。
    }

    #[test]
    fn a_stale_reclaim_visibility_snapshot_never_stops_a_just_shown_main_engine() {
        // 監査順序を barrier で固定する:
        // reclaim が main_visible=false を取得 → show_main が表示 + invalidate +
        // Main 起動予約 → reclaim 続行。古い可視性で Main を止めず、共通の
        // 再同期に委ねることを確認する (本番と同じ EngineControl/SyncEpoch を使う)。
        use std::sync::{Arc, Barrier, Mutex};

        struct FakeEngines {
            main_starting: bool,
            main_alive: bool,
        }

        let control = Arc::new(crate::commands::EngineControl::default());
        let engines = Arc::new(Mutex::new(FakeEngines {
            main_starting: false,
            main_alive: false,
        }));
        let barrier = Arc::new(Barrier::new(2));

        let reclaim_control = Arc::clone(&control);
        let reclaim_engines = Arc::clone(&engines);
        let reclaim_barrier = Arc::clone(&barrier);
        let reclaim = std::thread::spawn(move || {
            // 可視性の前に世代を記録し、「非表示」というスナップショットを読む。
            let snapshot_epoch = reclaim_control.lock().sync_epoch();
            let main_visible = false;
            // show_main の表示 + invalidate + 起動予約を待つ。
            reclaim_barrier.wait();
            reclaim_barrier.wait();
            // 制御ロック取得後に世代を照合する (本番の reclaim と同じ判定)。
            let control = reclaim_control.lock();
            if control.sync_epoch() != snapshot_epoch {
                // 古い可視性で停止しない (共通の再同期に委ねる)。
                return;
            }
            drop(control);
            if !main_visible {
                // StopMain 相当。
                reclaim_engines.lock().expect("engines").main_alive = false;
            }
        });

        // show_main 相当: barrier の後に invalidate + Main 起動予約。
        barrier.wait();
        control.lock().invalidate_sync();
        {
            let mut engines = engines.lock().expect("engines");
            engines.main_starting = true;
            engines.main_alive = true;
        }
        barrier.wait();

        reclaim.join().expect("reclaim thread");
        let engines = engines.lock().expect("engines");
        assert!(engines.main_starting, "Main の起動予約は残る");
        assert!(engines.main_alive, "表示中の Main を古い可視性で止めない");
    }

    #[test]
    fn reclaim_never_stops_a_shared_engine_while_it_is_in_use() {
        use EngineRole::Main;
        use Reclaim::{KeepAll, StopBoth, StopFloating};

        let resident = Some(Main);

        // 共有エンジン (メイン用) は 1 つだけ。floating を閉じただけでは止めない。
        assert_eq!(decide_reclaim(true, None, true, false, false), KeepAll);
        // メインの翻訳中・フローティングの翻訳中も止めない。
        assert_eq!(decide_reclaim(true, None, false, true, false), KeepAll);
        assert_eq!(decide_reclaim(true, None, false, false, true), KeepAll);
        // メインが隠れていて、どちらの翻訳も動いていないときだけ止める。
        assert_eq!(decide_reclaim(true, None, false, false, false), StopBoth);
        // 共有でも常駐 ON ならメイン用を 1 本維持する (案A)。
        assert_eq!(decide_reclaim(true, resident, true, false, false), KeepAll);
        assert_eq!(
            decide_reclaim(true, resident, false, false, false),
            StopFloating
        );
    }
}
