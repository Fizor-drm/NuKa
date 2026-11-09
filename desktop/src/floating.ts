/**
 * フローティング翻訳ウィンドウ (Phase 1)。
 *
 * Rust 側がグローバルショートカットで選択テキストを取り、`floating:capture` を
 * 全ウィンドウへ送る。この画面はイベントを受けて translate を呼び、ストリーミングで
 * 訳文を出すだけの薄いビュー。ウィンドウの表示・位置・非表示は Rust 側が持つ。
 *
 * イベントは jobId で突き合わせる。invoke の返却前にイベントが届くことがあるので、
 * main.ts と同じ waiter バッファで取りこぼしを防ぐ。`translation:superseded` は
 * 自ジョブが新しい翻訳に差し替えられた合図で、done/error は来ない。翻訳中表示だけ
 * 止め、それまでに届いた訳文は残す。
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { applyTranslations, isUiLanguage, localize, setUiLanguage, t } from "./i18n";
import {
  ipc,
  onEngineStatus,
  onFloatingCapture,
  onTranslationChunk,
  onTranslationDone,
  onTranslationError,
  onTranslationProgress,
  onTranslationSuperseded,
  type EnginePhase,
  type EngineRole,
  type FloatingCapturePayload,
} from "./ipc";
import { LANGUAGES, languageByCode } from "./languages";
import { writeClipboard } from "./output";
import { applyTheme, isThemeSetting } from "./theme";

/** 選択なしのときに自動で閉じるまでの時間。 */
const NO_SELECTION_HIDE_DELAY = 2500;
/** invoke 返却前に届いたイベントを保持する上限。 */
const MAX_BUFFERED_EVENTS = 800;
/** コピー完了表示を戻すまでの時間。 */
const COPY_FEEDBACK_DURATION = 1500;
/** 差し替え済み jobId を覚えておく上限。 */
const MAX_SUPERSEDED_JOBS = 32;
/**
 * ヘッダーをドラッグしてもよいかの判定に使う、除外する要素。
 *
 * ボタンと言語ポップオーバー (role=listbox とその子要素) はクリック操作を
 * 優先する。ヘッダー背景・訳マーク・`.floating-drag` スペーサーは該当しない
 * ので、そこからはウィンドウをドラッグできる。
 */
const DRAG_EXCLUSIONS = "button, a, input, select, textarea, [role='listbox'], [role='option']";

type JobEvent =
  | { kind: "progress"; jobId: number; done: number; total: number }
  | { kind: "chunk"; jobId: number; delta: string }
  | { kind: "done"; jobId: number; text: string }
  | { kind: "error"; jobId: number; message: string; messageKey?: string };

interface InvokeWaiter {
  readonly seq: number;
  readonly buffer: JobEvent[];
}

interface FloatingUiState {
  /** 最初の capture が来るまでは何も表示しない (起動直後の一瞬のちらつき防止)。 */
  initialized: boolean;
  sourceText: string | null;
  targetLang: string;
  translation: string;
  translating: boolean;
  error: string | null;
  jobId: number | null;
  progress: { done: number; total: number } | null;
  /** 訳先言語のリストボックスが開いているか。 */
  langOpen: boolean;
  /** リストボックスでハイライトしている項目。 */
  activeLangIndex: number;
}

const state: FloatingUiState = {
  initialized: false,
  sourceText: null,
  targetLang: "ja",
  translation: "",
  translating: false,
  error: null,
  jobId: null,
  progress: null,
  langOpen: false,
  activeLangIndex: 0,
};

function byId<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (node === null) {
    throw new Error(`要素が見つかりません: #${id}`);
  }
  return node as T;
}

/** hide_floating の呼び出し元。Rust のデバッグログで経路を特定するために使う。 */
type HideOrigin = "close-button" | "escape" | "auto-hide";

function hideFloating(origin: HideOrigin): Promise<void> {
  return invoke<void>("hide_floating", { origin });
}

const langEl = byId<HTMLDivElement>("floating-lang");
const langButtonEl = byId<HTMLButtonElement>("floating-lang-button");
const langLabelEl = byId<HTMLSpanElement>("floating-lang-label");
const langPopoverEl = byId<HTMLUListElement>("floating-lang-popover");
const copyEl = byId<HTMLButtonElement>("floating-copy");
const copyLabelEl = byId<HTMLSpanElement>("floating-copy-label");
const closeEl = byId<HTMLButtonElement>("floating-close");
const barEl = byId<HTMLElement>("floating-bar");
const gripEl = byId<HTMLDivElement>("floating-grip");
const messageEl = byId<HTMLParagraphElement>("floating-message");
const textEl = byId<HTMLDivElement>("floating-text");
const contentEl = byId<HTMLElement>("floating-content");

let waiter: InvokeWaiter | null = null;
let staleJobId: number | null = null;
let invokeSeq = 0;
let autoHideTimer: number | undefined;
let copyLabelTimer: number | undefined;
/** capture の世代。古い auto-hide タイマーを無効化する。 */
let captureSeq = 0;
const supersededJobs = new Set<number>();

/** エンジンの状態。バックグラウンドから復帰した直後は starting から始まる。 */
let enginePhase: EnginePhase = "starting";

/**
 * この画面の翻訳を処理するエンジンの役割。
 *
 * 解決モデルがメイン用と同一ならメイン用を共有するため、capture の
 * `engine_role` で受け取る。イベントはこの役割の status だけを反映する。
 */
let servingRole: EngineRole = "floating";

function setEnginePhase(phase: EnginePhase): void {
  if (enginePhase === phase) {
    return;
  }
  enginePhase = phase;
  render();
}

function describeError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  if (
    typeof error === "object" &&
    error !== null &&
    "fallback" in error &&
    typeof (error as { fallback: unknown }).fallback === "string"
  ) {
    return (error as { fallback: string }).fallback;
  }
  return String(error);
}

// ---------------------------------------------------------------------------
// 訳先言語のリストボックス
// ---------------------------------------------------------------------------
//
// ネイティブの <select> は WebView2 が OS のポップアップウィンドウを開くため、
// Tauri が WebView2 のフォーカス喪失を `WindowEvent::Focused(false)` として
// 通知し、floating が閉じてしまう。HTML 内のリストボックスにして、フォーカスが
// WebView2 の外へ出ないようにする。

function languageName(code: string): string {
  return languageByCode(code)?.native ?? code;
}

function languageOptions(): HTMLLIElement[] {
  return Array.from(langPopoverEl.querySelectorAll<HTMLLIElement>(".floating-lang-option"));
}

function setActiveLanguage(index: number, scroll: boolean): void {
  const options = languageOptions();
  if (options.length === 0) {
    return;
  }
  const clamped = Math.max(0, Math.min(index, options.length - 1));
  state.activeLangIndex = clamped;
  options.forEach((option, optionIndex) => {
    option.dataset["active"] = String(optionIndex === clamped);
  });
  const active = options[clamped];
  if (active !== undefined) {
    langPopoverEl.setAttribute("aria-activedescendant", active.id);
    if (scroll) {
      active.scrollIntoView({ block: "nearest" });
    }
  }
}

function updateLanguageUi(focusList: boolean): void {
  langLabelEl.textContent = languageName(state.targetLang);
  langButtonEl.setAttribute(
    "aria-label",
    `${t("floating.targetLangAria")}: ${languageName(state.targetLang)}`,
  );
  langButtonEl.setAttribute("aria-expanded", String(state.langOpen));
  langPopoverEl.hidden = !state.langOpen;

  const options = languageOptions();
  for (const option of options) {
    option.setAttribute("aria-selected", String(option.dataset["code"] === state.targetLang));
  }

  if (!state.langOpen) {
    langPopoverEl.removeAttribute("aria-activedescendant");
    return;
  }

  setActiveLanguage(state.activeLangIndex, false);
  const active = options[state.activeLangIndex];
  if (active !== undefined) {
    active.scrollIntoView({ block: "nearest" });
  }
  if (focusList) {
    langPopoverEl.focus({ preventScroll: true });
  }
}

function openLanguageList(): void {
  if (state.langOpen) {
    return;
  }
  state.langOpen = true;
  const current = LANGUAGES.findIndex((language) => language.code === state.targetLang);
  state.activeLangIndex = current >= 0 ? current : 0;
  updateLanguageUi(true);
}

function closeLanguageList(returnFocus: boolean): void {
  if (!state.langOpen) {
    return;
  }
  state.langOpen = false;
  updateLanguageUi(false);
  if (returnFocus) {
    langButtonEl.focus({ preventScroll: true });
  }
}

function chooseLanguage(code: string): void {
  if (languageByCode(code) === undefined) {
    return;
  }
  closeLanguageList(true);
  if (state.targetLang === code) {
    return;
  }
  state.targetLang = code;
  updateLanguageUi(false);
  if (state.sourceText !== null) {
    void translate();
  }
}

function buildLanguageList(): void {
  langPopoverEl.replaceChildren();
  LANGUAGES.forEach((language, index) => {
    const option = document.createElement("li");
    option.id = `floating-lang-option-${index}`;
    option.className = "floating-lang-option";
    option.setAttribute("role", "option");
    option.dataset["code"] = language.code;
    option.textContent = language.native;
    option.addEventListener("click", () => {
      chooseLanguage(language.code);
    });
    option.addEventListener("pointermove", () => {
      if (state.langOpen) {
        setActiveLanguage(index, false);
      }
    });
    langPopoverEl.append(option);
  });
  updateLanguageUi(false);
}

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------

function render(): void {
  if (!state.initialized) {
    messageEl.textContent = "";
    messageEl.hidden = true;
    textEl.textContent = "";
    textEl.hidden = true;
    copyEl.setAttribute("aria-disabled", "true");
    return;
  }

  const hasText = state.translation.trim() !== "";
  let message = "";
  if (state.error !== null) {
    message = state.error;
  } else if (!hasText) {
    if (state.sourceText === null) {
      message = t("floating.noSelection");
    } else if (state.translating && enginePhase === "starting") {
      // バックグラウンドから復帰した直後。engine:status で進み具合が届く。
      message = t("floating.engineStarting");
    } else if (state.translating && enginePhase === "loading") {
      message = t("floating.engineLoading");
    } else {
      message = t("floating.translating");
      const progress = state.progress;
      if (progress !== null && progress.total > 1) {
        message = `${message} ${Math.min(progress.done, progress.total)}/${progress.total}`;
      }
    }
  }

  messageEl.textContent = message;
  messageEl.hidden = message === "";
  messageEl.dataset["state"] = state.error !== null ? "error" : "info";
  textEl.textContent = state.translation;
  textEl.hidden = !hasText;
  copyEl.setAttribute("aria-disabled", String(!hasText));

  if (state.translating) {
    contentEl.scrollTop = contentEl.scrollHeight;
  }
}

function relocalize(): void {
  if (copyLabelTimer === undefined) {
    copyLabelEl.textContent = t("floating.copy");
  }
  updateLanguageUi(false);
  render();
}

/** キャプチャのたびにテーマと UI 言語を取り直す (メイン画面の設定に追従)。 */
async function refreshAppearance(): Promise<void> {
  try {
    const settings = await ipc.getSettings();
    if (isThemeSetting(settings.theme)) {
      applyTheme(settings.theme);
    }
    if (isUiLanguage(settings.ui_language)) {
      setUiLanguage(settings.ui_language);
    }
    applyTranslations();
    relocalize();
  } catch {
    // 設定が読めなくても翻訳は続ける。
  }
  // エンジンはバックグラウンドで止められていることがある。状態を取り直す。
  // 共有モデルのときはメイン用エンジンが処理するので、その役割で問い合わせる。
  try {
    const status = await ipc.getEngineStatus(servingRole);
    setEnginePhase(status.state);
  } catch {
    // エンジンの状態が読めなくても翻訳は続ける。
  }
}

// ---------------------------------------------------------------------------
// 翻訳
// ---------------------------------------------------------------------------

function handleJobEvent(event: JobEvent): void {
  if (state.jobId !== null && event.jobId === state.jobId) {
    applyJobEvent(event);
    return;
  }
  // invoke が返る前にイベントが届くことがある。jobId が確定するまで保持する。
  if (waiter !== null && event.jobId !== staleJobId) {
    waiter.buffer.push(event);
    if (waiter.buffer.length > MAX_BUFFERED_EVENTS) {
      waiter.buffer.shift();
    }
  }
}

function applyJobEvent(event: JobEvent): void {
  switch (event.kind) {
    case "progress":
      state.progress = { done: event.done, total: event.total };
      render();
      break;
    case "chunk":
      state.translation += event.delta;
      state.translating = true;
      state.error = null;
      render();
      break;
    case "done":
      state.translation = event.text;
      state.translating = false;
      state.error = null;
      state.progress = null;
      state.jobId = null;
      render();
      break;
    case "error":
      state.translating = false;
      state.error =
        event.messageKey !== undefined
          ? localize({ key: event.messageKey, fallback: event.message })
          : event.message;
      state.progress = null;
      state.jobId = null;
      render();
      break;
  }
}

/** 進行中のジョブと、返却待ちの invoke を無効化する。 */
function abortPending(): void {
  if (waiter !== null) {
    waiter = null;
    invokeSeq += 1;
  }
  if (state.jobId !== null) {
    staleJobId = state.jobId;
    const previous = state.jobId;
    state.jobId = null;
    void ipc.cancelTranslation(previous).catch(() => undefined);
  }
}

/**
 * 閉じたときに、進行中の capture を無効化する。
 *
 * `captureSeq` を進めることで、待機中 (テーマ取得など) の capture が後から
 * 翻訳を始めないようにする。非表示のウィンドウで翻訳が走ると、エンジンだけが
 * 起動して VRAM を保持してしまう。
 */
function cancelPendingCapture(): void {
  captureSeq += 1;
  state.translating = false;
  state.progress = null;
  abortPending();
}

async function translate(): Promise<void> {
  const text = state.sourceText;
  if (text === null || text.trim() === "") {
    return;
  }
  abortPending();

  state.translating = true;
  state.translation = "";
  state.error = null;
  state.progress = null;
  render();

  const seq = ++invokeSeq;
  const myWaiter: InvokeWaiter = { seq, buffer: [] };
  waiter = myWaiter;

  try {
    const jobId = await ipc.translate({ text, target_lang: state.targetLang });
    if (waiter !== myWaiter || seq !== invokeSeq) {
      // より新しい要求に追い越された。このジョブは捨てる。
      void ipc.cancelTranslation(jobId).catch(() => undefined);
      return;
    }
    waiter = null;
    state.jobId = jobId;
    if (supersededJobs.delete(jobId)) {
      // invoke の返却前に別ウィンドウの翻訳へ差し替えられていた。
      state.translating = false;
      state.progress = null;
      state.jobId = null;
      render();
      return;
    }
    const buffered = myWaiter.buffer.splice(0);
    for (const event of buffered) {
      if (event.jobId === jobId) {
        applyJobEvent(event);
      }
    }
  } catch (error) {
    if (waiter === myWaiter) {
      waiter = null;
    }
    if (seq !== invokeSeq) {
      return;
    }
    state.translating = false;
    state.error = describeError(error);
    render();
  }
}

function handleSuperseded(jobId: number): void {
  if (state.jobId === jobId) {
    // 累積済みの訳文はそのまま残し、翻訳中表示だけ止める。
    state.translating = false;
    state.progress = null;
    state.jobId = null;
    render();
    return;
  }
  if (waiter !== null) {
    supersededJobs.add(jobId);
    if (supersededJobs.size > MAX_SUPERSEDED_JOBS) {
      const oldest = supersededJobs.values().next().value;
      if (oldest !== undefined) {
        supersededJobs.delete(oldest);
      }
    }
  }
}

// ---------------------------------------------------------------------------
// capture と操作
// ---------------------------------------------------------------------------

async function handleCapture(payload: FloatingCapturePayload): Promise<void> {
  const capture = ++captureSeq;
  window.clearTimeout(autoHideTimer);
  autoHideTimer = undefined;
  abortPending();
  closeLanguageList(false);

  state.initialized = true;
  // この翻訳を処理するエンジンの役割。共有モデルなら "main" が届く。
  if (payload.engine_role === "main" || payload.engine_role === "floating") {
    servingRole = payload.engine_role;
  }
  state.sourceText =
    payload.text !== null && payload.text.trim() !== "" ? payload.text : null;
  state.translation = "";
  state.translating = false;
  state.error = null;
  state.progress = null;

  if (languageByCode(payload.target_lang) !== undefined) {
    state.targetLang = payload.target_lang;
  }
  updateLanguageUi(false);
  render();

  // テーマと表示言語はキャプチャのたびに取り直す。
  await refreshAppearance();
  if (capture !== captureSeq) {
    // 待っている間に閉じられた / 新しい capture が来た。翻訳は始めない。
    return;
  }

  if (state.sourceText === null) {
    window.clearTimeout(autoHideTimer);
    autoHideTimer = window.setTimeout(() => {
      autoHideTimer = undefined;
      // このタイマーが古い capture のもの、または選択ありの表示に変わっていたら
      // 何もしない (前の表示のタイマーで新しいウィンドウを閉じない)。
      if (capture !== captureSeq || state.sourceText !== null) {
        return;
      }
      cancelPendingCapture();
      void hideFloating("auto-hide").catch(() => undefined);
    }, NO_SELECTION_HIDE_DELAY);
    return;
  }

  // エンジンが止まっていれば、Rust 側 (`ensure_ready`) が起動を待ってから訳す。
  // 待っている間の表示は `engine:status` で「起動しています」に切り替わる。
  await translate();
}

async function copyTranslation(): Promise<void> {
  if (state.translation.trim() === "") {
    return;
  }
  const copied = await writeClipboard(state.translation);
  window.clearTimeout(copyLabelTimer);
  copyLabelEl.textContent = copied ? t("floating.copied") : t("floating.copyFailed");
  copyLabelEl.dataset["state"] = copied ? "ok" : "error";
  copyLabelTimer = window.setTimeout(() => {
    copyLabelTimer = undefined;
    copyLabelEl.textContent = t("floating.copy");
    delete copyLabelEl.dataset["state"];
  }, COPY_FEEDBACK_DURATION);
}

function closeWindow(origin: "close-button" | "escape"): void {
  window.clearTimeout(autoHideTimer);
  autoHideTimer = undefined;
  closeLanguageList(false);
  // 閉じたあとに翻訳を始めない (待機中の capture を無効化する)。
  cancelPendingCapture();
  void hideFloating(origin).catch(() => undefined);
}

// ---------------------------------------------------------------------------
// ウィンドウ操作 (ヘッダードラッグ / 右下つまみのリサイズ)
// ---------------------------------------------------------------------------

/**
 * ヘッダーをつかんだときのウィンドウ移動。
 *
 * pointerdown で OS へドラッグ開始要求を送る。返る Promise の完了は
 * ドラッグ終了を意味しないため、pointerup は待たずに開始する。ボタンと言語
 * ポップオーバーの上は通常のクリック操作を優先し、ドラッグを開始しない。
 */
function startWindowDrag(event: PointerEvent): void {
  if (event.button !== 0) {
    return;
  }
  const target = event.target;
  if (target instanceof Element && target.closest(DRAG_EXCLUSIONS) !== null) {
    return;
  }
  // ドラッグ中のテキスト選択を防ぐ。
  event.preventDefault();
  void getCurrentWindow()
    .startDragging()
    .catch((error) => {
      console.error("ウィンドウのドラッグを開始できません", error);
    });
}

/**
 * 右下のつまみをつかんだときのウィンドウリサイズ (右下方向のみ)。
 *
 * `startWindowDrag` と同様、ここで返る Promise の完了はリサイズ終了を意味しない
 * (OS への開始要求のみ) ため、pointerup は待たない。
 */
function startWindowResize(event: PointerEvent): void {
  if (event.button !== 0) {
    return;
  }
  event.preventDefault();
  void getCurrentWindow()
    .startResizeDragging("SouthEast")
    .catch((error) => {
      console.error("ウィンドウのリサイズを開始できません", error);
    });
}

function bindEvents(): void {
  onFloatingCapture((payload) => {
    void handleCapture(payload);
  });
  // エンジンの起動状況は全ウィンドウに届く。この画面の翻訳を処理する役割
  // (共有モデルなら "main") の status だけを反映する。role 省略時は main 扱い。
  onEngineStatus((status) => {
    const role: EngineRole = status.role ?? "main";
    if (role !== servingRole) {
      return;
    }
    setEnginePhase(status.state);
  });
  onTranslationProgress((payload) => {
    handleJobEvent({
      kind: "progress",
      jobId: payload.jobId,
      done: payload.done,
      total: payload.total,
    });
  });
  onTranslationChunk((payload) => {
    handleJobEvent({ kind: "chunk", jobId: payload.jobId, delta: payload.delta });
  });
  onTranslationDone((payload) => {
    handleJobEvent({ kind: "done", jobId: payload.jobId, text: payload.text });
  });
  onTranslationError((payload) => {
    handleJobEvent({
      kind: "error",
      jobId: payload.jobId,
      message: payload.message,
      messageKey: payload.messageKey,
    });
  });
  onTranslationSuperseded((payload) => {
    handleSuperseded(payload.jobId);
  });

  langButtonEl.addEventListener("click", () => {
    if (state.langOpen) {
      closeLanguageList(false);
    } else {
      openLanguageList();
    }
  });

  langButtonEl.addEventListener("keydown", (event) => {
    if (state.langOpen) {
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      openLanguageList();
      if (event.key === "ArrowUp") {
        setActiveLanguage(languageOptions().length - 1, true);
      }
    }
  });

  langPopoverEl.addEventListener("keydown", (event) => {
    if (!state.langOpen) {
      return;
    }
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActiveLanguage(state.activeLangIndex + 1, true);
        break;
      case "ArrowUp":
        event.preventDefault();
        setActiveLanguage(state.activeLangIndex - 1, true);
        break;
      case "Home":
        event.preventDefault();
        setActiveLanguage(0, true);
        break;
      case "End":
        event.preventDefault();
        setActiveLanguage(languageOptions().length - 1, true);
        break;
      case "Enter":
      case " ": {
        event.preventDefault();
        const code = languageOptions()[state.activeLangIndex]?.dataset["code"];
        if (code !== undefined) {
          chooseLanguage(code);
        }
        break;
      }
      case "Escape":
        // リストだけ閉じる。ウィンドウは閉じない。
        event.preventDefault();
        event.stopPropagation();
        closeLanguageList(true);
        break;
      case "Tab":
        closeLanguageList(false);
        break;
      default:
        break;
    }
  });

  // リスト外をクリックしたら閉じる (ウィンドウは閉じない)。
  document.addEventListener("pointerdown", (event) => {
    if (!state.langOpen) {
      return;
    }
    const target = event.target;
    if (!(target instanceof Node)) {
      return;
    }
    if (langEl.contains(target)) {
      return;
    }
    closeLanguageList(false);
  });

  copyEl.addEventListener("click", () => {
    void copyTranslation();
  });

  closeEl.addEventListener("click", () => {
    closeWindow("close-button");
  });

  // ウィンドウ操作。リスナーはここで一度だけ登録する。
  barEl.addEventListener("pointerdown", startWindowDrag);
  gripEl.addEventListener("pointerdown", startWindowResize);

  // × ボタンと同じ。言語リストが開いていればリストだけ閉じる。
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") {
      return;
    }
    if (state.langOpen) {
      event.preventDefault();
      closeLanguageList(true);
      return;
    }
    event.preventDefault();
    closeWindow("escape");
  });
}

async function bootstrap(): Promise<void> {
  buildLanguageList();
  bindEvents();
  await refreshAppearance();
  render();
}

void bootstrap();
