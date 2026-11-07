/**
 * 画面全体の状態機械。
 *
 * 入力 → 700ms のデバウンス → 自動翻訳が ON かつエンジンが ready なら translate。
 * translate は常に jobId を返し、進捗と結果はイベントで届く。すべて jobId で絞る。
 * エンジンがまだ準備できていないときの翻訳要求は pending として保持し、ready で流す。
 */

import { GlossaryEditor } from "./glossary";
import { AUTO_CODE, AUTO_OPTION, LANGUAGES, detectedLabel } from "./languages";
import {
  ipc,
  onEngineStatus,
  onTranslationChunk,
  onTranslationDone,
  onTranslationError,
  onTranslationProgress,
  type DetectedLanguage,
  type EngineStatus,
  type GlossaryPair,
  type SettingsPatch,
  type ThemeSetting,
  type TranslateRequest,
} from "./ipc";
import { LanguagePicker } from "./language-picker";
import { OutputPane, type OutputActionKind, type OutputView } from "./output";
import { applyTheme, isThemeSetting, nextTheme, renderThemeButton } from "./theme";

const AUTO_TRANSLATE_DELAY = 700;
const DETECT_DELAY = 400;
const MAX_BUFFERED_EVENTS = 800;

const ENGINE_LABELS: Record<EngineStatus["state"], string> = {
  starting: "エンジンを起動しています",
  loading: "モデルを読み込んでいます",
  ready: "準備完了",
  error: "エンジンエラー",
};

type FormChoice = "" | "plain" | "polite";

type JobEvent =
  | { kind: "progress"; jobId: number; done: number; total: number }
  | { kind: "chunk"; jobId: number; delta: string }
  | { kind: "done"; jobId: number; text: string; tokens?: number }
  | { kind: "error"; jobId: number; message: string };

interface InvokeWaiter {
  readonly seq: number;
  readonly buffer: JobEvent[];
}

interface AppState {
  sourceLang: string;
  targetLang: string;
  form: FormChoice;
  glossary: GlossaryPair[];
  effectiveGlossary: string;
  translation: string;
  translationError: string | null;
  engine: EngineStatus;
  jobId: number | null;
  detected: DetectedLanguage | null;
  autoTranslate: boolean;
  translating: boolean;
  progress: { done: number; total: number } | null;
  theme: ThemeSetting;
}

const state: AppState = {
  sourceLang: AUTO_CODE,
  targetLang: "ja",
  form: "",
  glossary: [],
  effectiveGlossary: "[]",
  translation: "",
  translationError: null,
  engine: { state: "starting" },
  jobId: null,
  detected: null,
  autoTranslate: true,
  translating: false,
  progress: null,
  theme: "light",
};

function byId<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (node === null) {
    throw new Error(`要素が見つかりません: #${id}`);
  }
  return node as T;
}

const engineEl = byId<HTMLDivElement>("engine");
const engineDotEl = byId<HTMLSpanElement>("engine-dot");
const engineLabelEl = byId<HTMLSpanElement>("engine-label");
const appStatusEl = byId<HTMLSpanElement>("app-status");
const autoToggleEl = byId<HTMLButtonElement>("auto-toggle");
const glossaryToggleEl = byId<HTMLButtonElement>("glossary-toggle");
const themeToggleEl = byId<HTMLButtonElement>("theme-toggle");

const sourceLangLabelEl = byId<HTMLElement>("source-lang-label");
const targetLangLabelEl = byId<HTMLElement>("target-lang-label");
const detectedEl = byId<HTMLSpanElement>("detected");
const swapEl = byId<HTMLButtonElement>("swap");

const sourceInputEl = byId<HTMLTextAreaElement>("source");
const formChipsEl = byId<HTMLDivElement>("form-chips");
const charCountEl = byId<HTMLSpanElement>("char-count");
const clearEl = byId<HTMLButtonElement>("clear");
const translateEl = byId<HTMLButtonElement>("translate");

const chunkProgressEl = byId<HTMLDivElement>("chunk-progress");
const chunkLabelEl = byId<HTMLSpanElement>("chunk-label");
const chunkFillEl = byId<HTMLSpanElement>("chunk-fill");

const glossaryPanelEl = byId<HTMLElement>("glossary-panel");
const glossaryRowsEl = byId<HTMLDivElement>("glossary-rows");
const glossaryEmptyEl = byId<HTMLParagraphElement>("glossary-empty");
const glossaryAddEl = byId<HTMLButtonElement>("glossary-add");

const formChipButtons = Array.from(formChipsEl.querySelectorAll<HTMLButtonElement>(".chip"));

const sourcePicker = new LanguagePicker(
  {
    trigger: byId<HTMLButtonElement>("source-lang-button"),
    label: sourceLangLabelEl,
    popover: byId<HTMLElement>("source-lang-popover"),
    search: byId<HTMLInputElement>("source-lang-search"),
    list: byId<HTMLUListElement>("source-lang-list"),
    empty: byId<HTMLElement>("source-lang-empty"),
  },
  [AUTO_OPTION, ...LANGUAGES],
  state.sourceLang,
  handleSourceLanguage,
);

const targetPicker = new LanguagePicker(
  {
    trigger: byId<HTMLButtonElement>("target-lang-button"),
    label: targetLangLabelEl,
    popover: byId<HTMLElement>("target-lang-popover"),
    search: byId<HTMLInputElement>("target-lang-search"),
    list: byId<HTMLUListElement>("target-lang-list"),
    empty: byId<HTMLElement>("target-lang-empty"),
  },
  LANGUAGES,
  state.targetLang,
  handleTargetLanguage,
);

const glossaryEditor = new GlossaryEditor(glossaryRowsEl, glossaryEmptyEl, handleGlossaryChange);

const outputPane = new OutputPane(
  {
    notice: byId<HTMLDivElement>("output-notice"),
    noticeText: byId<HTMLSpanElement>("output-notice-text"),
    placeholder: byId<HTMLParagraphElement>("output-placeholder"),
    text: byId<HTMLDivElement>("output-text"),
    action: byId<HTMLButtonElement>("output-action"),
    copy: byId<HTMLButtonElement>("copy"),
    copyLabel: byId<HTMLSpanElement>("copy-label"),
    copyStatus: byId<HTMLSpanElement>("copy-status"),
  },
  (kind) => {
    void handleOutputAction(kind);
  },
);

let autoTimer: number | undefined;
let detectTimer: number | undefined;
let detectSeq = 0;
let invokeSeq = 0;
let waiter: InvokeWaiter | null = null;
let staleJobId: number | null = null;
let pendingTranslate = false;
let appStatusTimer: number | undefined;

function getSourceText(): string {
  return sourceInputEl.value;
}

function buildRequest(text: string): TranslateRequest {
  const request: TranslateRequest = {
    text,
    target_lang: state.targetLang,
  };
  if (state.sourceLang !== AUTO_CODE) {
    request.source_lang = state.sourceLang;
  }
  if (state.targetLang === "ja" && state.form !== "") {
    request.form = state.form;
  }
  const glossary = state.glossary
    .map((pair) => ({ source: pair.source.trim(), target: pair.target.trim() }))
    .filter((pair) => pair.source !== "" && pair.target !== "");
  if (glossary.length > 0) {
    request.glossary = glossary;
  }
  return request;
}

function computeOutputView(): OutputView {
  const text = state.translation;
  if (state.translationError !== null) {
    return {
      kind: "error",
      message: state.translationError,
      action: { kind: "retry", label: "再試行" },
      text,
    };
  }
  if (state.engine.state === "error") {
    return {
      kind: "error",
      message: state.engine.detail ?? "エンジンを起動できませんでした。",
      action: { kind: "restart", label: "再起動" },
      text,
    };
  }
  if (state.translating) {
    return { kind: "translating", message: text === "" ? "翻訳しています…" : "", action: null, text };
  }
  if (state.engine.state === "starting" || state.engine.state === "loading") {
    return { kind: "loading", message: "モデルを読み込んでいます…", action: null, text };
  }
  return { kind: "idle", message: "", action: null, text };
}

function renderEngine(): void {
  const { state: phase, detail } = state.engine;
  engineDotEl.dataset["state"] = phase;
  engineLabelEl.textContent = ENGINE_LABELS[phase];
  if (detail !== undefined && detail !== "") {
    engineEl.title = detail;
  } else {
    engineEl.removeAttribute("title");
  }
}

function renderAutoToggle(): void {
  autoToggleEl.setAttribute("aria-pressed", String(state.autoTranslate));
}

function renderLanguageBar(): void {
  sourcePicker.setValue(state.sourceLang, false);
  targetPicker.setValue(state.targetLang, false);

  const detected = state.detected;
  const showDetected = state.sourceLang === AUTO_CODE && detected !== null;
  detectedEl.hidden = !showDetected;
  if (showDetected && detected !== null) {
    detectedEl.textContent = detectedLabel(detected.code);
    detectedEl.title = `確度 ${Math.round(detected.confidence * 100)}%`;
  }

  const canSwap = state.sourceLang !== AUTO_CODE || detected !== null;
  swapEl.disabled = !canSwap;
  swapEl.title = canSwap ? "言語を入れ替える" : "自動検出のままでは入れ替えできません";
}

function renderFormChips(): void {
  formChipsEl.hidden = state.targetLang !== "ja";
  for (const chip of formChipButtons) {
    const form = chip.dataset["form"] ?? "";
    chip.setAttribute("aria-pressed", String(form === state.form));
  }
}

function renderCharCount(): void {
  charCountEl.textContent = `${Array.from(sourceInputEl.value).length} 文字`;
}

function renderChunkProgress(): void {
  const progress = state.progress;
  const visible = progress !== null && progress.total > 1;
  chunkProgressEl.hidden = !visible;
  if (progress === null || !visible) {
    return;
  }
  const done = Math.min(progress.done, progress.total);
  chunkLabelEl.textContent = `${done}/${progress.total}`;
  const ratio = Math.max(0, Math.min(1, done / progress.total));
  chunkFillEl.style.width = `${Math.round(ratio * 100)}%`;
}

function renderOutput(): void {
  outputPane.render(computeOutputView());
}

function renderAll(): void {
  renderEngine();
  renderAutoToggle();
  renderLanguageBar();
  renderFormChips();
  renderCharCount();
  renderOutput();
  renderChunkProgress();
}

function describeError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

function showAppStatus(message: string): void {
  appStatusEl.textContent = message;
  window.clearTimeout(appStatusTimer);
  appStatusTimer = window.setTimeout(() => {
    appStatusEl.textContent = "";
  }, 4000);
}

// ---------------------------------------------------------------------------
// エンジン
// ---------------------------------------------------------------------------

function handleEngineStatus(status: EngineStatus): void {
  const wasReady = state.engine.state === "ready";
  state.engine = status;
  renderEngine();
  renderOutput();
  if (status.state === "ready" && !wasReady && pendingTranslate) {
    pendingTranslate = false;
    void requestTranslate();
  }
}

async function refreshEngineStatus(): Promise<void> {
  try {
    handleEngineStatus(await ipc.getEngineStatus());
  } catch (error) {
    handleEngineStatus({ state: "error", detail: describeError(error) });
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
  // invoke が返る前にイベントが届くことがある。新しい jobId と突き合わせるまで保持する。
  if (waiter !== null && event.jobId !== staleJobId) {
    waiter.buffer.push(event);
    if (waiter.buffer.length > MAX_BUFFERED_EVENTS) {
      waiter.buffer.shift();
    }
  }
}

function applyJobEvent(event: JobEvent): void {
  switch (event.kind) {
    case "progress": {
      state.progress = { done: event.done, total: event.total };
      renderChunkProgress();
      break;
    }
    case "chunk": {
      state.translation += event.delta;
      state.translating = true;
      state.translationError = null;
      renderOutput();
      break;
    }
    case "done": {
      state.translation = event.text;
      state.translating = false;
      state.translationError = null;
      state.progress = null;
      state.jobId = null;
      renderAll();
      break;
    }
    case "error": {
      state.translating = false;
      state.translationError = event.message;
      state.progress = null;
      state.jobId = null;
      renderAll();
      break;
    }
  }
}

async function requestTranslate(): Promise<void> {
  const text = getSourceText();
  if (text.trim() === "") {
    return;
  }

  if (state.engine.state !== "ready") {
    pendingTranslate = true;
    state.translationError = null;
    renderOutput();
    return;
  }
  pendingTranslate = false;

  if (state.jobId !== null) {
    staleJobId = state.jobId;
    const previous = state.jobId;
    state.jobId = null;
    void ipc.cancelTranslation(previous).catch(() => undefined);
  }

  state.translating = true;
  state.translation = "";
  state.translationError = null;
  state.progress = null;
  renderAll();

  const seq = ++invokeSeq;
  const myWaiter: InvokeWaiter = { seq, buffer: [] };
  waiter = myWaiter;

  try {
    const jobId = await ipc.translate(buildRequest(text));
    if (waiter !== myWaiter || seq !== invokeSeq) {
      // より新しい要求に追い越された。このジョブは捨てる。
      void ipc.cancelTranslation(jobId).catch(() => undefined);
      return;
    }
    waiter = null;
    state.jobId = jobId;
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
    state.translationError = describeError(error);
    renderAll();
  }
}

function maybeRetranslate(): void {
  if (!state.autoTranslate) {
    return;
  }
  if (getSourceText().trim() === "") {
    return;
  }
  void requestTranslate();
}

function scheduleAutoTranslate(): void {
  window.clearTimeout(autoTimer);
  if (!state.autoTranslate) {
    return;
  }
  if (getSourceText().trim() === "") {
    return;
  }
  autoTimer = window.setTimeout(() => {
    void requestTranslate();
  }, AUTO_TRANSLATE_DELAY);
}

// ---------------------------------------------------------------------------
// 言語検出
// ---------------------------------------------------------------------------

function scheduleDetect(): void {
  window.clearTimeout(detectTimer);
  if (state.sourceLang !== AUTO_CODE) {
    state.detected = null;
    renderLanguageBar();
    return;
  }
  if (getSourceText().trim() === "") {
    state.detected = null;
    renderLanguageBar();
    return;
  }
  detectTimer = window.setTimeout(() => {
    void runDetect();
  }, DETECT_DELAY);
}

async function runDetect(): Promise<void> {
  if (state.sourceLang !== AUTO_CODE) {
    return;
  }
  const text = getSourceText();
  if (text.trim() === "") {
    state.detected = null;
    renderLanguageBar();
    return;
  }
  const seq = ++detectSeq;
  try {
    const detected = await ipc.detectLanguage(text);
    if (seq !== detectSeq || state.sourceLang !== AUTO_CODE) {
      return;
    }
    state.detected = detected;
  } catch {
    if (seq !== detectSeq) {
      return;
    }
    state.detected = null;
  }
  renderLanguageBar();
}

// ---------------------------------------------------------------------------
// 言語の選択と入れ替え
// ---------------------------------------------------------------------------

function handleSourceLanguage(code: string): void {
  state.sourceLang = code;
  if (code !== AUTO_CODE) {
    state.detected = null;
    window.clearTimeout(detectTimer);
  }
  renderLanguageBar();
  scheduleDetect();
  maybeRetranslate();
}

function handleTargetLanguage(code: string): void {
  state.targetLang = code;
  renderFormChips();
  renderLanguageBar();
  maybeRetranslate();
}

function handleFormChoice(form: FormChoice): void {
  state.form = form;
  renderFormChips();
  maybeRetranslate();
}

function handleGlossaryChange(): void {
  state.glossary = glossaryEditor.getPairs();
  const effective = JSON.stringify(
    state.glossary.filter((pair) => pair.source.trim() !== "" && pair.target.trim() !== ""),
  );
  if (effective === state.effectiveGlossary) {
    return;
  }
  state.effectiveGlossary = effective;
  scheduleAutoTranslate();
}

function swapLanguages(): void {
  const detected = state.detected;
  const sourceCode = state.sourceLang === AUTO_CODE ? detected?.code ?? null : state.sourceLang;
  if (sourceCode === null) {
    return;
  }

  const previousTarget = state.targetLang;
  const previousText = getSourceText();
  const translation = state.translation;
  const hadTranslation = translation.trim() !== "";

  if (state.jobId !== null) {
    staleJobId = state.jobId;
    const previous = state.jobId;
    state.jobId = null;
    void ipc.cancelTranslation(previous).catch(() => undefined);
  }
  window.clearTimeout(autoTimer);
  window.clearTimeout(detectTimer);

  state.translating = false;
  state.translation = "";
  state.translationError = null;
  state.progress = null;
  state.sourceLang = previousTarget;
  state.targetLang = sourceCode;
  state.detected = null;
  sourceInputEl.value = hadTranslation ? translation : previousText;

  renderAll();
  if (state.autoTranslate && getSourceText().trim() !== "") {
    void requestTranslate();
  }
}

function clearSource(): void {
  if (state.jobId !== null) {
    staleJobId = state.jobId;
    const previous = state.jobId;
    state.jobId = null;
    void ipc.cancelTranslation(previous).catch(() => undefined);
  }
  window.clearTimeout(autoTimer);
  window.clearTimeout(detectTimer);
  pendingTranslate = false;
  state.translating = false;
  state.translation = "";
  state.translationError = null;
  state.progress = null;
  state.detected = null;
  sourceInputEl.value = "";
  renderAll();
  sourceInputEl.focus();
}

// ---------------------------------------------------------------------------
// 出力ペインの操作
// ---------------------------------------------------------------------------

async function handleOutputAction(kind: OutputActionKind): Promise<void> {
  if (kind === "retry") {
    void requestTranslate();
    return;
  }
  state.translationError = null;
  state.engine = { state: "starting", detail: "llama-server を起動し直しています。" };
  renderEngine();
  renderOutput();
  try {
    await ipc.restartEngine();
  } catch (error) {
    state.engine = { state: "error", detail: describeError(error) };
    renderEngine();
    renderOutput();
  }
}

// ---------------------------------------------------------------------------
// 設定とテーマ
// ---------------------------------------------------------------------------

async function persistSettings(patch: SettingsPatch): Promise<void> {
  try {
    await ipc.setSettings(patch);
  } catch (error) {
    showAppStatus(`設定を保存できませんでした: ${describeError(error)}`);
  }
}

function updateAutoTranslate(next: boolean): void {
  state.autoTranslate = next;
  renderAutoToggle();
  void persistSettings({ auto_translate: next });
}

function cycleTheme(): void {
  state.theme = nextTheme(state.theme);
  applyTheme(state.theme);
  renderThemeButton(themeToggleEl, state.theme);
  void persistSettings({ theme: state.theme });
}

async function loadSettings(): Promise<void> {
  try {
    const settings = await ipc.getSettings();
    if (isThemeSetting(settings.theme)) {
      state.theme = settings.theme;
    }
    state.autoTranslate = settings.auto_translate;
  } catch {
    // 設定が読めなくても既定値で動かす
  }
  applyTheme(state.theme);
  renderThemeButton(themeToggleEl, state.theme);
  renderAutoToggle();
}

// ---------------------------------------------------------------------------
// DOM の配線
// ---------------------------------------------------------------------------

function setGlossaryOpen(open: boolean): void {
  glossaryPanelEl.dataset["open"] = String(open);
  glossaryToggleEl.setAttribute("aria-pressed", String(open));
  glossaryToggleEl.setAttribute("aria-expanded", String(open));
  glossaryPanelEl.toggleAttribute("inert", !open);
  if (open) {
    const firstInput = glossaryRowsEl.querySelector<HTMLInputElement>(".glossary-input");
    (firstInput ?? glossaryAddEl).focus();
  } else if (glossaryPanelEl.contains(document.activeElement)) {
    glossaryToggleEl.focus();
  }
}

function bindTranslationEvents(): void {
  onEngineStatus((status) => {
    handleEngineStatus(status);
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
    handleJobEvent({
      kind: "done",
      jobId: payload.jobId,
      text: payload.text,
      tokens: payload.tokens,
    });
  });
  onTranslationError((payload) => {
    handleJobEvent({ kind: "error", jobId: payload.jobId, message: payload.message });
  });
}

function bindUi(): void {
  sourceInputEl.addEventListener("input", () => {
    renderCharCount();
    scheduleDetect();
    scheduleAutoTranslate();
  });

  translateEl.addEventListener("click", () => {
    void requestTranslate();
  });

  clearEl.addEventListener("click", () => {
    clearSource();
  });

  swapEl.addEventListener("click", () => {
    swapLanguages();
  });

  autoToggleEl.addEventListener("click", () => {
    updateAutoTranslate(!state.autoTranslate);
  });

  glossaryToggleEl.addEventListener("click", () => {
    setGlossaryOpen(glossaryPanelEl.dataset["open"] !== "true");
  });

  glossaryAddEl.addEventListener("click", () => {
    glossaryEditor.addRow(undefined, true);
  });

  themeToggleEl.addEventListener("click", () => {
    cycleTheme();
  });

  for (const chip of formChipButtons) {
    chip.addEventListener("click", () => {
      const form = chip.dataset["form"] ?? "";
      if (form === "" || form === "plain" || form === "polite") {
        handleFormChoice(form);
      }
    });
  }
}

async function bootstrap(): Promise<void> {
  bindTranslationEvents();
  bindUi();
  glossaryEditor.addRow();
  applyTheme(state.theme);
  renderThemeButton(themeToggleEl, state.theme);
  renderAll();
  await loadSettings();
  await refreshEngineStatus();
}

void bootstrap();
