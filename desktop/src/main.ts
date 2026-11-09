/**
 * 画面全体の状態機械。
 *
 * 入力 → 700ms のデバウンス → 自動翻訳が ON かつエンジンが ready なら translate。
 * translate は常に jobId を返し、進捗と結果はイベントで届く。すべて jobId で絞る。
 * エンジンがまだ準備できていないときの翻訳要求は pending として保持し、ready で流す。
 */

import { GlossaryEditor } from "./glossary";
import { icon } from "./icons";
import {
  applyTranslations,
  getUiLanguage,
  isUiLanguage,
  localize,
  setUiLanguage,
  t,
  type LocalizedMessage,
  type UiLanguage,
} from "./i18n";
import { AUTO_CODE, AUTO_OPTION, LANGUAGES, languageLabel } from "./languages";
import {
  ipc,
  onDownloadDone,
  onDownloadError,
  onDownloadProgress,
  onEngineStatus,
  onOpenSettings,
  onTranslationChunk,
  onTranslationDone,
  onTranslationError,
  onTranslationProgress,
  onTranslationSuperseded,
  onUpdateDone,
  onUpdateError,
  onUpdateProgress,
  type CatalogEntry,
  type DetectedLanguage,
  type DownloadKind,
  type EngineStatus,
  type FloatingSettingsPatch,
  type GlossaryPair,
  type InstalledModel,
  type SettingsPatch,
  type SetupState,
  type ThemeSetting,
  type TranslateRequest,
  type UpdateCheck,
  type UpdateDonePayload,
  type UpdateErrorPayload,
  type UpdateProgressPayload,
} from "./ipc";
import { LanguagePicker } from "./language-picker";
import { OutputPane, type OutputActionKind, type OutputView } from "./output";
import { applyTheme, isThemeSetting, nextTheme, renderThemeButton } from "./theme";
import { open as openDirectoryDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

const AUTO_TRANSLATE_DELAY = 700;
const DETECT_DELAY = 400;
const MAX_BUFFERED_EVENTS = 800;

const ENGINE_LABELS: Record<EngineStatus["state"], string> = {
  starting: "engine.starting",
  loading: "engine.loading",
  ready: "engine.ready",
  error: "engine.error",
};

type FormChoice = "" | "plain" | "polite";

type ViewName = "translate" | "settings";

interface DownloadState {
  kind: DownloadKind;
  id: string;
  downloaded: number;
  total: number;
}

type UpdatePhase =
  | "idle"
  | "checking"
  | "latest"
  | "available"
  | "downloading"
  | "ready"
  | "installing"
  | "error";

interface UpdateUiState {
  phase: UpdatePhase;
  check: UpdateCheck | null;
  progress: { downloaded: number; total: number } | null;
  message: string | LocalizedMessage;
}

type JobEvent =
  | { kind: "progress"; jobId: number; done: number; total: number }
  | { kind: "chunk"; jobId: number; delta: string }
  | { kind: "done"; jobId: number; text: string; tokens?: number }
  | { kind: "error"; jobId: number; message: string; messageKey?: string };

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
  view: ViewName;
  setup: SetupState | null;
  catalog: CatalogEntry[];
  download: DownloadState | null;
  modelsDirSetting: string | null;
  autoUpdateCheck: boolean;
  uiLanguage: UiLanguage;
  update: UpdateUiState;
  floatingEnabled: boolean;
  floatingHotkey: string;
  floatingRecording: boolean;
  floatingRecordingWasEnabled: boolean;
  floatingError: string | null;
  /** フローティング用モデル (null は「アクティブモデルと同じ」)。 */
  floatingModel: string | null;
  /** フローティング用モデルの選択ポップオーバーが開いているか。 */
  floatingModelOpen: boolean;
  /** メインを閉じてもトレイに常駐する (background.enabled)。 */
  backgroundEnabled: boolean;
  /** フローティング用エンジンを常時起動しておく (background.resident_engine)。 */
  backgroundResidentEngine: boolean;
  backgroundError: string | null;
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
  view: "translate",
  setup: null,
  catalog: [],
  download: null,
  modelsDirSetting: null,
  autoUpdateCheck: true,
  uiLanguage: "ja",
  update: { phase: "idle", check: null, progress: null, message: "" },
  floatingEnabled: true,
  floatingHotkey: "Ctrl+Shift+T",
  floatingRecording: false,
  floatingRecordingWasEnabled: false,
  floatingError: null,
  floatingModel: null,
  floatingModelOpen: false,
  backgroundEnabled: true,
  backgroundResidentEngine: true,
  backgroundError: null,
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

const viewTranslateEl = byId<HTMLDivElement>("translate-view");
const viewSettingsEl = byId<HTMLElement>("settings-view");
const settingsToggleEl = byId<HTMLButtonElement>("settings-toggle");
const settingsBackEl = byId<HTMLButtonElement>("settings-back");
const runtimeStatusEl = byId<HTMLParagraphElement>("runtime-status");
const runtimeNoteEl = byId<HTMLParagraphElement>("runtime-note");
const runtimeInstallEl = byId<HTMLButtonElement>("runtime-install");
const runtimeDownloadEl = byId<HTMLDivElement>("runtime-download");
const runtimeBarEl = byId<HTMLSpanElement>("runtime-bar");
const runtimeProgressTextEl = byId<HTMLSpanElement>("runtime-progress-text");
const runtimeCancelEl = byId<HTMLButtonElement>("runtime-cancel");
const modelListEl = byId<HTMLDivElement>("model-list");
const modelsDirPathEl = byId<HTMLParagraphElement>("models-dir-path");
const modelsDirDefaultEl = byId<HTMLInputElement>("models-dir-default");
const modelsDirCustomEl = byId<HTMLInputElement>("models-dir-custom");
const modelsDirPickEl = byId<HTMLButtonElement>("models-dir-pick");
const licenseLinkEl = byId<HTMLButtonElement>("license-link");
const llamaLinkEl = byId<HTMLButtonElement>("llama-link");

const updateToggleEl = byId<HTMLButtonElement>("update-toggle");
const updatePopoverEl = byId<HTMLDivElement>("update-popover");
const updateDotEl = byId<HTMLSpanElement>("update-dot");
const updateStatusEl = byId<HTMLParagraphElement>("update-status");
const updateCurrentEl = byId<HTMLSpanElement>("update-current");
const updateActionsEl = byId<HTMLDivElement>("update-actions");
const updateInstallEl = byId<HTMLButtonElement>("update-install");
const updateRetryEl = byId<HTMLButtonElement>("update-retry");
const updateReleaseEl = byId<HTMLButtonElement>("update-release");
const updateDownloadEl = byId<HTMLDivElement>("update-download");
const updateBarEl = byId<HTMLSpanElement>("update-bar");
const updateProgressTextEl = byId<HTMLSpanElement>("update-progress-text");
const updateCancelEl = byId<HTMLButtonElement>("update-cancel");
const autoUpdateCheckEl = byId<HTMLInputElement>("auto-update-check");
const updateControlEl = updateToggleEl.parentElement;
const settingsVersionEl = byId<HTMLSpanElement>("settings-version");

const floatingEnabledEl = byId<HTMLInputElement>("floating-enabled");
const floatingHotkeyEl = byId<HTMLButtonElement>("floating-hotkey");
const floatingUsageEl = byId<HTMLParagraphElement>("floating-hotkey-usage");
const floatingErrorEl = byId<HTMLParagraphElement>("floating-hotkey-error");

const floatingModelButtonEl = byId<HTMLButtonElement>("floating-model-button");
const floatingModelLabelEl = byId<HTMLSpanElement>("floating-model-label");
const floatingModelPopoverEl = byId<HTMLDivElement>("floating-model-popover");
const floatingModelListEl = byId<HTMLUListElement>("floating-model-list");
const floatingModelEmptyEl = byId<HTMLParagraphElement>("floating-model-empty");
const floatingModelNoteEl = byId<HTMLParagraphElement>("floating-model-note");
const floatingModelSharedNoteEl = byId<HTMLParagraphElement>("floating-model-shared-note");

const backgroundEnabledEl = byId<HTMLInputElement>("background-enabled");
const backgroundResidentEngineEl = byId<HTMLInputElement>("background-resident-engine");
const backgroundResidentNoteEl = byId<HTMLParagraphElement>("background-resident-engine-note");
const backgroundDisabledNoteEl = byId<HTMLParagraphElement>("background-disabled-note");
const backgroundErrorEl = byId<HTMLParagraphElement>("background-error");

type SettingsSection = "runtime" | "language" | "floating" | "background" | "update" | "models";

const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  "runtime",
  "language",
  "floating",
  "background",
  "update",
  "models",
];

const settingsNavEl = byId<HTMLElement>("settings-nav");
const settingsNavItems = Array.from(
  document.querySelectorAll<HTMLButtonElement>(".settings-nav-item"),
);
const settingsPanels = Array.from(document.querySelectorAll<HTMLElement>("[data-panel]"));

function isSettingsSection(value: string): value is SettingsSection {
  return (SETTINGS_SECTIONS as readonly string[]).includes(value);
}

function selectSettingsSection(section: SettingsSection, focusItem = false): void {
  for (const item of settingsNavItems) {
    const active = item.dataset["section"] === section;
    item.setAttribute("aria-current", String(active));
    if (active && focusItem) {
      item.focus();
    }
  }
  for (const panel of settingsPanels) {
    panel.hidden = panel.dataset["panel"] !== section;
  }
}

const uiLanguageInputs = Array.from(
  document.querySelectorAll<HTMLInputElement>('input[name="ui-language"]'),
);

const APP_VERSION = "1.3.4";
const RELEASES_PAGE_URL = "https://github.com/Fizor-drm/NuKa/releases/latest";

const LICENSE_URL = "https://plamo.preferredai.jp/info/plamo-community-license-en";
const LLAMA_URL = "https://github.com/ggml-org/llama.cpp";

interface ModelRow {
  readonly entry: CatalogEntry;
  readonly root: HTMLDivElement;
  readonly actions: HTMLDivElement;
  readonly install: HTMLButtonElement;
  readonly use: HTMLButtonElement;
  readonly remove: HTMLButtonElement;
  readonly badge: HTMLSpanElement;
  readonly progress: HTMLDivElement;
  readonly fill: HTMLSpanElement;
  readonly progressText: HTMLSpanElement;
  readonly cancel: HTMLButtonElement;
}

const modelRows = new Map<string, ModelRow>();

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
/** invoke 返却前に見た superseded の jobId (取りこぼし防止)。 */
const supersededJobs = new Set<number>();
/** 記録開始時にホットキーを外す処理。完了まではキー入力を受け付けない。 */
let hotkeySuspendTask: Promise<unknown> | null = null;
let hotkeySuspendPending = false;

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
      action: { kind: "retry", label: t("output.retry") },
      text,
    };
  }
  if (state.engine.state === "error") {
    const setup = state.setup;
    if (setup !== null && !setup.runtime.installed) {
      return {
        kind: "error",
        message: t("output.runtimeMissing"),
        action: { kind: "settings", label: t("output.openSettings") },
        text,
      };
    }
    if (setup !== null && !setup.model.resolved) {
      return {
        kind: "error",
        message:
          setup.models.length === 0 ? t("output.modelMissing") : t("output.modelNotSelected"),
        action: { kind: "settings", label: t("output.openSettings") },
        text,
      };
    }
    return {
      kind: "error",
      message:
        state.engine.detail !== undefined
          ? localize(state.engine.detail)
          : t("output.engineStartFailed"),
      action: { kind: "restart", label: t("output.restart") },
      text,
    };
  }
  if (state.translating) {
    return {
      kind: "translating",
      message: text === "" ? t("output.translating") : "",
      action: null,
      text,
    };
  }
  if (state.engine.state === "starting" || state.engine.state === "loading") {
    return { kind: "loading", message: t("output.loading"), action: null, text };
  }
  return { kind: "idle", message: "", action: null, text };
}

function renderEngine(): void {
  const { state: phase, detail } = state.engine;
  engineDotEl.dataset["state"] = phase;
  engineLabelEl.textContent = t(ENGINE_LABELS[phase]);
  const localizedDetail = detail !== undefined ? localize(detail) : "";
  if (localizedDetail !== "") {
    engineEl.title = localizedDetail;
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
    detectedEl.textContent = t("lang.detected", { language: languageLabel(detected.code) });
    detectedEl.title = t("lang.detectedTitle", {
      percent: Math.round(detected.confidence * 100),
    });
  }

  const canSwap = state.sourceLang !== AUTO_CODE || detected !== null;
  swapEl.disabled = !canSwap;
  swapEl.title = canSwap ? t("lang.swapTitle") : t("lang.swapTitleDisabled");
  swapEl.setAttribute("aria-label", t("lang.swapAria"));
}

function renderFormChips(): void {
  formChipsEl.hidden = state.targetLang !== "ja";
  for (const chip of formChipButtons) {
    const form = chip.dataset["form"] ?? "";
    chip.setAttribute("aria-pressed", String(form === state.form));
  }
}

function renderCharCount(): void {
  charCountEl.textContent = t("source.charCount", {
    count: Array.from(sourceInputEl.value).length,
  });
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
  if (
    typeof error === "object" &&
    error !== null &&
    "key" in error &&
    "fallback" in error
  ) {
    return localize(error as LocalizedMessage);
  }
  return String(error);
}

/** フロントで組み立てた文字列を、言語切替でも再描画できるメッセージにする。 */
function localizedMessage(key: string, params: Record<string, string> = {}): LocalizedMessage {
  return { key, params, fallback: t(key, params) };
}

function rawMessage(message: string): LocalizedMessage {
  return { key: "error.raw", params: { detail: message }, fallback: message };
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

/**
 * メイン画面用エンジンの status か。
 *
 * `engine:status` はフローティング用も含めて全ウィンドウへ流れる。role が無い
 * payload は後方互換のためメイン扱いにする。
 */
function isMainEngineStatus(status: EngineStatus): boolean {
  return status.role === undefined || status.role === "main";
}

function handleEngineStatus(status: EngineStatus): void {
  if (!isMainEngineStatus(status)) {
    return;
  }
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
    handleEngineStatus(await ipc.getEngineStatus("main"));
  } catch (error) {
    handleEngineStatus({ role: "main", state: "error", detail: rawMessage(describeError(error)) });
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
      state.translationError =
        event.messageKey !== undefined
          ? localize({ key: event.messageKey, fallback: event.message })
          : event.message;
      state.progress = null;
      state.jobId = null;
      renderAll();
      break;
    }
  }
}

/** 自ジョブが新しい翻訳に差し替えられた。done も error も来ないので表示を止める。 */
function handleTranslationSuperseded(jobId: number): void {
  if (state.jobId === jobId) {
    // 累積済みの訳文は残し、翻訳中表示だけ止める。
    state.translating = false;
    state.progress = null;
    state.jobId = null;
    renderAll();
    return;
  }
  if (waiter !== null) {
    supersededJobs.add(jobId);
    if (supersededJobs.size > 32) {
      const oldest = supersededJobs.values().next().value;
      if (oldest !== undefined) {
        supersededJobs.delete(oldest);
      }
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
    if (supersededJobs.delete(jobId)) {
      // invoke の返却前に別ウィンドウの翻訳へ差し替えられていた。
      state.translating = false;
      state.progress = null;
      state.jobId = null;
      renderAll();
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
  reportMainTargetLang();
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
  if (kind === "settings") {
    setView("settings");
    return;
  }
  state.translationError = null;
  state.engine = { state: "starting", detail: localizedMessage("engine.startingDetail") };
  renderEngine();
  renderOutput();
  try {
    await ipc.restartEngine();
  } catch (error) {
    state.engine = { state: "error", detail: rawMessage(describeError(error)) };
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
    showAppStatus(t("status.settingsSaveFailed", { detail: describeError(error) }));
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

async function changeUiLanguage(language: UiLanguage): Promise<void> {
  if (!isUiLanguage(language) || language === getUiLanguage()) {
    return;
  }
  state.uiLanguage = language;
  setUiLanguage(language);
  applyTranslations();
  relocalizeDynamicUi();
  try {
    await ipc.setSettings({ ui_language: language });
  } catch (error) {
    showAppStatus(t("status.settingsSaveFailed", { detail: describeError(error) }));
  }
  await refreshEngineStatus();
  await refreshSetup();
}

/** 言語切替時に、JS が組み立てている文言をすべて作り直す。 */
function relocalizeDynamicUi(): void {
  renderThemeButton(themeToggleEl, state.theme);
  renderAutoToggle();
  renderEngine();
  renderLanguageBar();
  sourcePicker.refresh();
  targetPicker.refresh();
  renderCharCount();
  glossaryEditor.relocalize();
  buildModelRows();
  renderSettings();
  renderUpdate();
  outputPane.relocalize();
  renderOutput();
}

async function loadSettings(): Promise<void> {
  try {
    const settings = await ipc.getSettings();
    if (isThemeSetting(settings.theme)) {
      state.theme = settings.theme;
    }
    state.autoTranslate = settings.auto_translate;
    state.autoUpdateCheck = settings.auto_update_check;
    autoUpdateCheckEl.checked = state.autoUpdateCheck;
    if (isUiLanguage(settings.ui_language)) {
      state.uiLanguage = settings.ui_language;
    }
    setUiLanguage(state.uiLanguage);
    applyTranslations();
    state.modelsDirSetting = settings.models_dir;
    state.floatingEnabled = settings.floating_window.enabled;
    state.floatingHotkey = settings.floating_window.hotkey;
    state.floatingModel = settings.floating_window.model;
    state.backgroundEnabled = settings.background.enabled;
    state.backgroundResidentEngine = settings.background.resident_engine;
  } catch {
    // 設定が読めなくても既定値で動かす
  }
  applyTheme(state.theme);
  renderThemeButton(themeToggleEl, state.theme);
  renderAutoToggle();
  reportMainTargetLang();
}

// ---------------------------------------------------------------------------
// フローティングウィンドウ (Phase 1)
// ---------------------------------------------------------------------------

/** メイン画面の訳先言語を Rust に知らせる (フローティングの初期訳先)。 */
function reportMainTargetLang(): void {
  void ipc.setMainTargetLang(state.targetLang).catch(() => undefined);
}

function renderFloatingSettings(): void {
  floatingEnabledEl.checked = state.floatingEnabled || state.floatingRecording;
  floatingEnabledEl.disabled = state.floatingRecording;
  floatingHotkeyEl.textContent = state.floatingRecording
    ? t("settings.floating.hotkeyRecording")
    : state.floatingHotkey;
  floatingHotkeyEl.dataset["recording"] = String(state.floatingRecording);
  floatingHotkeyEl.setAttribute("aria-pressed", String(state.floatingRecording));
  floatingHotkeyEl.setAttribute("aria-label", t("settings.floating.hotkeyLabel"));
  floatingHotkeyEl.title = state.floatingRecording
    ? t("settings.floating.hotkeyRecording")
    : t("settings.floating.hotkeyRecord");
  floatingUsageEl.textContent = t("settings.floating.usage", {
    hotkey: state.floatingHotkey,
  });
  floatingErrorEl.hidden = state.floatingError === null;
  floatingErrorEl.textContent = state.floatingError ?? "";
  renderFloatingModel();
}

// ---------------------------------------------------------------------------
// フローティング用モデルの選択 (HTML 内ポップオーバー)
// ---------------------------------------------------------------------------
//
// ネイティブの <select> は使わない (WebView2 が OS のポップアップを開き、
// フォーカスイベントがウィンドウ管理と干渉する)。既存の言語リストと同じ
// ボタン + listbox で作る。

/** インストール済みモデルの表示名。カタログに無ければファイル名。 */
function modelDisplayName(file: string): string {
  const entry = state.catalog.find((item) => item.file === file);
  return entry !== undefined ? `${entry.size.toUpperCase()} · ${entry.quant}` : file;
}

/** セレクタの選択肢。先頭は「アクティブモデルと同じ」(値は空文字)。 */
function floatingModelOptions(): Array<{ value: string; label: string }> {
  const installed = state.setup?.models ?? [];
  return [
    { value: "", label: t("settings.floating.modelSame") },
    ...installed.map((model) => ({
      value: model.file,
      label: modelDisplayName(model.file),
    })),
  ];
}

function renderFloatingModel(): void {
  const current = state.floatingModel ?? "";
  const options = floatingModelOptions();
  const selected = options.find((option) => option.value === current);
  floatingModelLabelEl.textContent =
    selected?.label ?? (current === "" ? t("settings.floating.modelSame") : modelDisplayName(current));
  floatingModelButtonEl.setAttribute("aria-expanded", String(state.floatingModelOpen));
  // 専用モデルにアクティブと同じファイルを選んだときは、2 本目は起動せず
  // メイン用エンジンを共有する。既定の注記 (専用モデルで常時起動) と食い違う
  // ため、共有時の説明へ差し替える。
  const sameAsActive =
    state.floatingModel !== null && state.setup?.active_model === state.floatingModel;
  floatingModelNoteEl.hidden = sameAsActive;
  floatingModelSharedNoteEl.hidden = !sameAsActive;

  floatingModelListEl.replaceChildren();
  options.forEach((option, index) => {
    const item = document.createElement("li");
    item.id = `floating-model-option-${index}`;
    item.className = "lang-option";
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", String(option.value === current));
    item.dataset["value"] = option.value;
    item.tabIndex = -1;

    const text = document.createElement("span");
    text.className = "lang-option-text";
    const label = document.createElement("span");
    label.className = "lang-option-label";
    label.textContent = option.label;
    text.append(label);

    const check = icon("check", 14);
    check.classList.add("lang-option-check");
    item.append(text, check);
    // マウスクリックでも選べること (キーボードの Enter だけにしない)。
    item.addEventListener("click", () => {
      void chooseFloatingModel(option.value);
    });
    floatingModelListEl.append(item);
  });
  floatingModelEmptyEl.hidden = options.length > 1;
  floatingModelPopoverEl.hidden = !state.floatingModelOpen;
}

function setFloatingModelOpen(open: boolean, focusTrigger: boolean): void {
  if (state.floatingModelOpen === open) {
    return;
  }
  state.floatingModelOpen = open;
  renderFloatingModel();
  if (open) {
    const selected = floatingModelListEl.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]',
    );
    (selected ?? floatingModelListEl.querySelector<HTMLElement>('[role="option"]'))?.focus();
  } else if (focusTrigger) {
    floatingModelButtonEl.focus();
  }
}

/** 選択肢の指定位置へフォーカスする (範囲外は端に丸める)。 */
function focusFloatingModelOption(index: number): void {
  const options = Array.from(
    floatingModelListEl.querySelectorAll<HTMLElement>('[role="option"]'),
  );
  if (options.length === 0) {
    return;
  }
  const clamped = Math.max(0, Math.min(index, options.length - 1));
  options[clamped]?.focus();
}

/** 先頭または末尾の選択肢へフォーカスする (Home / End 用)。 */
function focusFloatingModelEdge(last: boolean): void {
  const options = Array.from(
    floatingModelListEl.querySelectorAll<HTMLElement>('[role="option"]'),
  );
  if (options.length === 0) {
    return;
  }
  options[last ? options.length - 1 : 0]?.focus();
}

function moveFloatingModelFocus(delta: number): void {
  const options = Array.from(
    floatingModelListEl.querySelectorAll<HTMLElement>('[role="option"]'),
  );
  if (options.length === 0) {
    return;
  }
  const index = options.indexOf(document.activeElement as HTMLElement);
  if (index < 0) {
    focusFloatingModelOption(delta > 0 ? 0 : options.length - 1);
    return;
  }
  focusFloatingModelOption((index + delta + options.length) % options.length);
}

async function chooseFloatingModel(value: string): Promise<void> {
  setFloatingModelOpen(false, true);
  if ((state.floatingModel ?? "") === value) {
    return;
  }
  state.floatingModel = value === "" ? null : value;
  renderFloatingSettings();
  // 失敗しても保存自体は成功していることがある (ホットキー競合)。表示は
  // applyFloatingSettings 内の syncFloatingSettings が真の値へ合わせ直す。
  await applyFloatingSettings({ model: value });
  renderFloatingSettings();
  // 常時起動の注記はモデル選択に依存する。
  renderBackgroundSettings();
}

/** 設定を保存し直し、失敗したら真の値を取り直す。 */
async function applyFloatingSettings(patch: FloatingSettingsPatch): Promise<boolean> {
  try {
    const settings = await ipc.setFloatingSettings(patch);
    state.floatingEnabled = settings.floating_window.enabled;
    state.floatingHotkey = settings.floating_window.hotkey;
    state.floatingModel = settings.floating_window.model;
    state.floatingError = null;
    return true;
  } catch (error) {
    state.floatingError = describeError(error);
    // 保存は成功して登録だけ失敗したケース (ホットキー競合) があるため、
    // 画面の値を保存済みの設定へ合わせ直す。
    await syncFloatingSettings();
    return false;
  }
}

async function syncFloatingSettings(): Promise<void> {
  try {
    const settings = await ipc.getSettings();
    state.floatingEnabled = settings.floating_window.enabled;
    state.floatingHotkey = settings.floating_window.hotkey;
    state.floatingModel = settings.floating_window.model;
  } catch {
    // 取得できなければ現在の表示を保つ。
  }
}

async function handleFloatingToggle(): Promise<void> {
  if (state.floatingRecording) {
    return;
  }
  const desired = floatingEnabledEl.checked;
  state.floatingEnabled = desired;
  floatingEnabledEl.disabled = true;
  await applyFloatingSettings({ enabled: desired });
  floatingEnabledEl.disabled = false;
  renderFloatingSettings();
}

/**
 * ホットキーの記録を始める。
 *
 * 記録中は設定を一時的に「無効」にして OS からホットキーを外す。そうしないと、
 * いま登録されている組み合わせを押しても RegisterHotKey に横取りされて keydown が
 * 届かず、同じキーを記録し直せない。終了時 (Esc・保存・フォーカス移動) に元へ戻す。
 */
function startHotkeyRecording(): void {
  if (state.floatingRecording) {
    return;
  }
  state.floatingRecording = true;
  state.floatingRecordingWasEnabled = state.floatingEnabled;
  state.floatingError = null;
  renderFloatingSettings();
  floatingHotkeyEl.focus();
  if (state.floatingRecordingWasEnabled) {
    hotkeySuspendPending = true;
    // 記録中だけ OS からホットキーを外す。常駐エンジンは止めない (suspend)。
    hotkeySuspendTask = applyFloatingSettings({ enabled: false, suspend: true }).then(() => {
      hotkeySuspendPending = false;
      hotkeySuspendTask = null;
      if (state.floatingRecording) {
        renderFloatingSettings();
      }
    });
  }
}

async function stopHotkeyRecording(restoreEnabled: boolean): Promise<void> {
  if (!state.floatingRecording) {
    return;
  }
  state.floatingRecording = false;
  hotkeySuspendPending = false;
  // 記録開始時の「無効化」がまだ終わっていない場合は、先に完了を待つ。
  // 待たずに有効化すると、あとから無効化が適用されてホットキーが消える。
  if (hotkeySuspendTask !== null) {
    await hotkeySuspendTask;
  }
  if (restoreEnabled && state.floatingRecordingWasEnabled) {
    await applyFloatingSettings({ enabled: true });
  }
  renderFloatingSettings();
}

/** keydown から "Ctrl+Shift+T" を作る。対象外なら null。 */
function comboFromEvent(event: KeyboardEvent): string | null {
  if (event.metaKey) {
    // Win キー併用は設定画面の案内 (Ctrl/Alt/Shift + 英数字) に含めない。
    return null;
  }
  if (!event.ctrlKey && !event.altKey && !event.shiftKey) {
    return null;
  }

  let key: string | null = null;
  if (event.key.length === 1 && /^[a-zA-Z0-9]$/.test(event.key)) {
    key = event.key.toUpperCase();
  } else {
    // Shift + 数字では event.key が "!" などの記号になる。物理キー (Digit0-9)
    // から数字を復元し、バックエンドの parse_hotkey が受け付ける形にする。
    const digitKey = /^Digit([0-9])$/.exec(event.code)?.[1];
    if (digitKey !== undefined) {
      key = digitKey;
    } else if (event.keyCode >= 48 && event.keyCode <= 57) {
      // 合成入力 (agent-desktop の SendInput など) では event.code が DigitN に
      // ならないことがある。VK (0x30-0x39) は正しく届くので数字を復元する。
      key = String(event.keyCode - 48);
    }
  }
  if (key === null) {
    return null;
  }

  const parts: string[] = [];
  if (event.ctrlKey) {
    parts.push("Ctrl");
  }
  if (event.altKey) {
    parts.push("Alt");
  }
  if (event.shiftKey) {
    parts.push("Shift");
  }
  parts.push(key);
  return parts.join("+");
}

async function saveRecordedHotkey(combo: string): Promise<void> {
  const restoreEnabled = state.floatingRecordingWasEnabled;
  state.floatingRecording = false;
  hotkeySuspendPending = false;
  state.floatingHotkey = combo;
  state.floatingError = null;
  renderFloatingSettings();

  // 記録開始時の無効化がまだ終わっていない場合は先に待つ。
  if (hotkeySuspendTask !== null) {
    await hotkeySuspendTask;
  }
  // 記録開始時に無効化していたなら、ホットキー保存後に有効へ戻す。
  // この再有効化で登録を試みるので、競合はここでエラーになる。
  await applyFloatingSettings({ hotkey: combo });
  if (restoreEnabled && !state.floatingEnabled) {
    await applyFloatingSettings({ enabled: true });
  }
  renderFloatingSettings();
}

// ---------------------------------------------------------------------------
// バックグラウンド (トレイ常駐)
// ---------------------------------------------------------------------------

function renderBackgroundSettings(): void {
  backgroundEnabledEl.checked = state.backgroundEnabled;
  backgroundResidentEngineEl.checked = state.backgroundResidentEngine;
  // 常駐しないなら「エンジンを常時起動する」は意味を持たない。
  backgroundResidentEngineEl.disabled = !state.backgroundEnabled;
  // 常時起動は「フローティング用モデル」を選んで初めて働く (二重ロード防止)。
  const residentReady = state.floatingModel !== null;
  // 専用モデルがアクティブと同じファイルなら、2 本目は起こさずメイン用 1 本を
  // 維持する。注記もその実挙動に合わせる。
  const sameAsActiveModel =
    state.floatingModel !== null && state.setup?.active_model === state.floatingModel;
  backgroundResidentNoteEl.textContent = t(
    residentReady
      ? sameAsActiveModel
        ? "settings.background.residentEngineNoteShared"
        : "settings.background.residentEngineNote"
      : "settings.background.residentEngineNeedsModel",
  );
  backgroundResidentNoteEl.dataset["state"] = residentReady
    ? sameAsActiveModel
      ? "shared"
      : "ready"
    : "needs-model";
  backgroundDisabledNoteEl.hidden = state.backgroundEnabled;
  backgroundErrorEl.hidden = state.backgroundError === null;
  backgroundErrorEl.textContent = state.backgroundError ?? "";
}

/** バックグラウンド設定を保存する。失敗したら保存済みの値へ戻す。 */
async function saveBackgroundSettings(): Promise<void> {
  state.backgroundError = null;
  try {
    const settings = await ipc.setSettings({
      background: {
        enabled: state.backgroundEnabled,
        resident_engine: state.backgroundResidentEngine,
      },
    });
    state.backgroundEnabled = settings.background.enabled;
    state.backgroundResidentEngine = settings.background.resident_engine;
  } catch (error) {
    state.backgroundError = t("settings.background.saveFailed", {
      detail: describeError(error),
    });
    try {
      const settings = await ipc.getSettings();
      state.backgroundEnabled = settings.background.enabled;
      state.backgroundResidentEngine = settings.background.resident_engine;
    } catch {
      // 取得できなければ現在の表示を保つ。
    }
  }
  renderBackgroundSettings();
}

async function handleBackgroundEnabledChange(): Promise<void> {
  const desired = backgroundEnabledEl.checked;
  state.backgroundEnabled = desired;
  if (!desired) {
    // 常駐を切ったら「常時起動」は効かないので見た目も合わせる。
    backgroundResidentEngineEl.checked = state.backgroundResidentEngine;
  }
  backgroundEnabledEl.disabled = true;
  renderBackgroundSettings();
  await saveBackgroundSettings();
  backgroundEnabledEl.disabled = false;
}

async function handleBackgroundResidentEngineChange(): Promise<void> {
  state.backgroundResidentEngine = backgroundResidentEngineEl.checked;
  backgroundResidentEngineEl.disabled = true;
  await saveBackgroundSettings();
  backgroundResidentEngineEl.disabled = false;
}

// ---------------------------------------------------------------------------
// 設定ビュー
// ---------------------------------------------------------------------------

function setView(view: ViewName): void {
  state.view = view;
  viewTranslateEl.hidden = view !== "translate";
  viewSettingsEl.hidden = view !== "settings";
  settingsToggleEl.setAttribute("aria-pressed", String(view === "settings"));
  if (view === "settings") {
    // 設定を開くたびに先頭 (ランタイム) から表示する。
    selectSettingsSection("runtime");
    void refreshSetup().finally(() => {
      renderSettings();
      settingsBackEl.focus();
    });
  } else {
    sourceInputEl.focus();
  }
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 MB";
  }
  const megabytes = bytes / (1024 * 1024);
  if (megabytes >= 1024) {
    return `${(megabytes / 1024).toFixed(2)} GB`;
  }
  if (megabytes >= 10) {
    return `${Math.round(megabytes)} MB`;
  }
  return `${megabytes.toFixed(1)} MB`;
}

async function refreshCatalog(): Promise<void> {
  try {
    state.catalog = await ipc.getCatalog();
    buildModelRows();
  } catch (error) {
    showAppStatus(t("status.catalogFailed", { detail: describeError(error) }));
  }
  renderSettings();
}

async function refreshSetup(): Promise<void> {
  try {
    state.setup = await ipc.getSetupState();
  } catch (error) {
    state.setup = null;
    showAppStatus(t("status.setupFailed", { detail: describeError(error) }));
  }
  renderOutput();
  renderSettings();
}

function isModelResolved(setup: SetupState | null): boolean {
  return setup?.model.resolved === true;
}

function buildModelRows(): void {
  modelListEl.replaceChildren();
  modelRows.clear();
  for (const entry of state.catalog) {
    const root = document.createElement("div");
    root.className = "model-row";
    root.dataset["id"] = entry.id;

    const main = document.createElement("div");
    main.className = "model-main";
    const name = document.createElement("span");
    name.className = "model-name";
    name.textContent = `${entry.size.toUpperCase()} · ${entry.quant}`;
    const size = document.createElement("span");
    size.className = "model-size";
    size.textContent = t("models.aboutSize", { size: formatBytes(entry.bytes) });
    main.append(name, size);

    const actions = document.createElement("div");
    actions.className = "model-actions";

    const badge = document.createElement("span");
    badge.className = "model-badge";
    badge.textContent = "使用中";
    badge.hidden = true;

    const install = document.createElement("button");
    install.type = "button";
    install.className = "primary-button";
    install.textContent = t("models.install");
    install.setAttribute(
      "aria-label",
      t("models.installAria", { size: entry.size.toUpperCase(), quant: entry.quant }),
    );

    const use = document.createElement("button");
    use.type = "button";
    use.className = "secondary-button";
    use.textContent = t("models.use");
    use.setAttribute(
      "aria-label",
      t("models.useAria", { size: entry.size.toUpperCase(), quant: entry.quant }),
    );

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "secondary-button";
    remove.textContent = t("models.delete");
    remove.setAttribute(
      "aria-label",
      t("models.deleteAria", { size: entry.size.toUpperCase(), quant: entry.quant }),
    );

    actions.append(badge, install, use, remove);

    const progress = document.createElement("div");
    progress.className = "download-row model-download";
    progress.hidden = true;
    const bar = document.createElement("span");
    bar.className = "download-bar";
    const fill = document.createElement("span");
    fill.className = "download-fill";
    bar.append(fill);
    const progressText = document.createElement("span");
    progressText.className = "download-text";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "secondary-button";
    cancel.textContent = t("models.cancel");
    cancel.setAttribute(
      "aria-label",
      t("models.cancelAria", { size: entry.size.toUpperCase(), quant: entry.quant }),
    );
    progress.append(bar, progressText, cancel);

    root.append(main, actions, progress);
    modelListEl.append(root);

    modelRows.set(entry.id, {
      entry,
      root,
      actions,
      install,
      use,
      remove,
      badge,
      progress,
      fill,
      progressText,
      cancel,
    });

    install.addEventListener("click", () => {
      void startModelInstall(entry);
    });
    use.addEventListener("click", () => {
      void activateModel(entry);
    });
    remove.addEventListener("click", () => {
      void removeModel(entry);
    });
    cancel.addEventListener("click", () => {
      void cancelDownload(entry.id);
    });
  }
}

function renderSettings(): void {
  const setup = state.setup;
  if (setup === null) {
    runtimeStatusEl.textContent = t("runtime.checking");
    runtimeNoteEl.hidden = true;
    runtimeInstallEl.hidden = false;
    runtimeInstallEl.disabled = true;
  } else if (!setup.runtime.installed) {
    runtimeStatusEl.textContent = t("runtime.notInstalled");
    runtimeNoteEl.hidden = true;
    runtimeInstallEl.hidden = false;
    runtimeInstallEl.disabled = state.download !== null;
  } else {
    const version = setup.runtime.version ?? t("runtime.external");
    const usage =
      setup.runtime.bytes !== undefined
        ? t("runtime.usage", { size: formatBytes(setup.runtime.bytes) })
        : "";
    runtimeStatusEl.textContent = t("runtime.installed", { version }) + usage;
    runtimeNoteEl.hidden = false;
    runtimeInstallEl.hidden = true;
    runtimeInstallEl.disabled = true;
  }

  const runtimeDownloading = state.download?.kind === "runtime";
  runtimeDownloadEl.hidden = !runtimeDownloading;
  if (runtimeDownloading && state.download !== null) {
    const { downloaded, total } = state.download;
    const ratio = total > 0 ? Math.min(1, downloaded / total) : 0;
    runtimeBarEl.style.width = `${Math.round(ratio * 100)}%`;
    runtimeProgressTextEl.textContent =
      total > 0 ? `${formatBytes(downloaded)} / ${formatBytes(total)}` : formatBytes(downloaded);
  }

  const installed = new Map<string, InstalledModel>();
  for (const model of setup?.models ?? []) {
    installed.set(model.file, model);
  }
  const active = setup?.active_model ?? null;
  for (const row of modelRows.values()) {
    const model = installed.get(row.entry.file);
    const isActive = model !== undefined && active === model.file;
    const isDownloading = state.download?.kind === "model" && state.download.id === row.entry.id;
    const busy = state.download !== null;

    row.install.hidden = model !== undefined || isDownloading;
    row.install.disabled = busy;
    row.use.hidden = model === undefined || isActive;
    row.use.disabled = busy;
    row.remove.hidden = model === undefined;
    row.remove.disabled = isActive || busy;
    row.remove.title = isActive ? t("models.deleteActiveTitle") : t("models.deleteTitle");
    row.badge.hidden = !isActive;
    row.badge.textContent = t("models.active");

    row.progress.hidden = !isDownloading;
    if (isDownloading && state.download !== null) {
      const { downloaded, total } = state.download;
      const ratio = total > 0 ? Math.min(1, downloaded / total) : 0;
      row.fill.style.width = `${Math.round(ratio * 100)}%`;
      row.progressText.textContent =
        total > 0 ? `${formatBytes(downloaded)} / ${formatBytes(total)}` : formatBytes(downloaded);
    }
  }

  modelsDirPathEl.textContent = setup?.models_dir ?? "";
  const custom = state.modelsDirSetting !== null && state.modelsDirSetting !== "";
  modelsDirDefaultEl.checked = !custom;
  modelsDirCustomEl.checked = custom;
  modelsDirPickEl.disabled = state.download !== null;
  autoUpdateCheckEl.checked = state.autoUpdateCheck;
  settingsVersionEl.textContent = `v${APP_VERSION}`;
  for (const input of uiLanguageInputs) {
    input.checked = input.value === state.uiLanguage;
  }
  renderFloatingSettings();
  renderBackgroundSettings();
}

// ---------------------------------------------------------------------------
// アップデート
// ---------------------------------------------------------------------------

function setUpdateOpen(open: boolean): void {
  updatePopoverEl.hidden = !open;
  updateToggleEl.setAttribute("aria-expanded", String(open));
  if (open) {
    renderUpdate();
  }
}

function renderUpdate(): void {
  const update = state.update;
  const check = update.check;
  const latest = check?.latest !== undefined ? `v${check.latest}` : "";
  updateCurrentEl.textContent = `v${check?.current ?? APP_VERSION}`;
  updateToggleEl.classList.toggle("is-checking", update.phase === "checking");
  updateDotEl.hidden = update.phase !== "available" && update.phase !== "ready";

  let message: string;
  switch (update.phase) {
    case "idle":
      message = t("update.idle");
      break;
    case "checking":
      message = t("update.checking");
      break;
    case "latest":
      message = check?.latest !== undefined ? t("update.latest", { version: latest }) : t("update.latestUnknown");
      break;
    case "available": {
      const size = check?.asset !== undefined ? formatBytes(check.asset.size) : "";
      message = t("update.available", { version: latest, size });
      break;
    }
    case "downloading":
      message = t("update.downloading");
      break;
    case "ready":
      message = t("update.ready");
      break;
    case "installing":
      message = t("update.installing");
      break;
    case "error":
      if (typeof update.message === "string") {
        message = update.message !== "" ? update.message : t("update.unknownError");
      } else {
        message = localize(update.message);
      }
      break;
  }
  updateStatusEl.textContent = message;

  const downloading = update.phase === "downloading";
  updateDownloadEl.hidden = !downloading;
  if (downloading && update.progress !== null) {
    const { downloaded, total } = update.progress;
    const ratio = total > 0 ? Math.min(1, downloaded / total) : 0;
    updateBarEl.style.width = `${Math.round(ratio * 100)}%`;
    updateProgressTextEl.textContent =
      total > 0 ? `${formatBytes(downloaded)} / ${formatBytes(total)}` : formatBytes(downloaded);
  }

  updateActionsEl.hidden = update.phase !== "available" && update.phase !== "error";
  updateInstallEl.hidden = update.phase !== "available";
  updateRetryEl.hidden = update.phase !== "error";
  updateReleaseEl.hidden = update.phase !== "available";
}

async function checkForUpdates(manual: boolean): Promise<void> {
  if (
    state.update.phase === "checking" ||
    state.update.phase === "downloading" ||
    state.update.phase === "installing"
  ) {
    return;
  }
  state.update.phase = "checking";
  state.update.progress = null;
  state.update.message = "";
  renderUpdate();
  try {
    const check = await ipc.checkUpdate();
    state.update.check = check;
    if (check.error !== undefined) {
      state.update.phase = "error";
      state.update.message = check.error;
    } else if (check.available) {
      state.update.phase = "available";
    } else {
      state.update.phase = "latest";
    }
  } catch (error) {
    state.update.phase = "error";
    state.update.message = localizedMessage("update.checkFailed", { detail: describeError(error) });
  }
  renderUpdate();
  if (manual && state.update.phase === "error") {
    showAppStatus(typeof state.update.message === "string" ? state.update.message : localize(state.update.message));
  }
}

async function startUpdateDownload(): Promise<void> {
  const asset = state.update.check?.asset;
  if (state.update.phase !== "available" || asset === undefined) {
    return;
  }
  state.update.phase = "downloading";
  state.update.progress = { downloaded: 0, total: asset.size };
  state.update.message = "";
  renderUpdate();
  try {
    await ipc.downloadUpdate();
  } catch (error) {
    if (state.update.phase === "downloading") {
      state.update.phase = "error";
      state.update.progress = null;
      state.update.message = localizedMessage("update.downloadStartFailed", { detail: describeError(error) });
      renderUpdate();
    }
  }
}

async function cancelUpdateDownload(): Promise<void> {
  try {
    await ipc.cancelUpdateDownload();
  } catch (error) {
    showAppStatus(t("update.cancelFailed", { detail: describeError(error) }));
  }
}

async function installDownloadedUpdate(): Promise<void> {
  if (state.update.phase !== "ready") {
    return;
  }
  state.update.phase = "installing";
  renderUpdate();
  try {
    await ipc.installUpdate();
  } catch (error) {
    state.update.phase = "error";
    state.update.message = localizedMessage("update.installFailed", { detail: describeError(error) });
    renderUpdate();
  }
}

function handleUpdateProgress(payload: UpdateProgressPayload): void {
  if (state.update.phase !== "downloading") {
    return;
  }
  state.update.progress = {
    downloaded: payload.downloaded,
    total: payload.total > 0 ? payload.total : (state.update.progress?.total ?? 0),
  };
  renderUpdate();
}

function handleUpdateDone(payload: UpdateDonePayload): void {
  state.update.phase = "ready";
  state.update.progress = null;
  state.update.message = "";
  renderUpdate();
  showAppStatus(t("update.readyStatus", { version: payload.version }));
  void installDownloadedUpdate();
}

function handleUpdateError(payload: UpdateErrorPayload): void {
  const cancelled =
    typeof payload.message === "object" &&
    payload.message !== null &&
    (payload.message.key === "updater.cancelled" || payload.message.key === "installer.cancelled");
  if (state.update.phase === "downloading" && cancelled) {
    state.update.phase = "available";
    state.update.progress = null;
    renderUpdate();
    return;
  }
  state.update.phase = "error";
  state.update.progress = null;
  state.update.message = payload.message;
  renderUpdate();
}

function toggleUpdatePopover(): void {
  const open = updatePopoverEl.hidden;
  setUpdateOpen(open);
  if (open && state.update.phase === "idle") {
    void checkForUpdates(true);
  }
}

async function handleAutoUpdateCheckChange(): Promise<void> {
  const desired = autoUpdateCheckEl.checked;
  state.autoUpdateCheck = desired;
  try {
    await ipc.setSettings({ auto_update_check: desired });
  } catch (error) {
    state.autoUpdateCheck = !desired;
    autoUpdateCheckEl.checked = !desired;
    showAppStatus(t("status.settingsSaveFailed", { detail: describeError(error) }));
  }
}

async function startRuntimeInstall(): Promise<void> {
  if (state.download !== null) {
    return;
  }
  state.download = { kind: "runtime", id: "runtime", downloaded: 0, total: 0 };
  renderSettings();
  try {
    await ipc.installRuntime();
  } catch (error) {
    if (state.download?.kind === "runtime") {
      state.download = null;
      renderSettings();
      showAppStatus(t("status.runtimeInstallFailed", { detail: describeError(error) }));
    }
  }
}

async function startModelInstall(entry: CatalogEntry): Promise<void> {
  if (state.download !== null) {
    return;
  }
  state.download = { kind: "model", id: entry.id, downloaded: 0, total: entry.bytes };
  renderSettings();
  try {
    await ipc.installModel(entry.id);
  } catch (error) {
    if (state.download?.kind === "model" && state.download.id === entry.id) {
      state.download = null;
      renderSettings();
      showAppStatus(t("status.modelInstallFailed", { detail: describeError(error) }));
    }
  }
}

async function cancelDownload(id: string): Promise<void> {
  try {
    await ipc.cancelDownload(id);
  } catch (error) {
    showAppStatus(t("update.cancelFailed", { detail: describeError(error) }));
  }
}

async function activateModel(entry: CatalogEntry): Promise<void> {
  try {
    await ipc.setActiveModel(entry.file);
    await refreshSetup();
    await refreshEngineStatus();
  } catch (error) {
    showAppStatus(t("status.modelSwitchFailed", { detail: describeError(error) }));
  }
}

async function removeModel(entry: CatalogEntry): Promise<void> {
  try {
    const settings = await ipc.deleteModel(entry.file);
    // 削除したモデルがフローティング用に指定されていた場合、バックエンドが
    // 「アクティブモデルと同じ」へ戻す。表示もその値へ合わせる。
    state.floatingEnabled = settings.floating_window.enabled;
    state.floatingHotkey = settings.floating_window.hotkey;
    state.floatingModel = settings.floating_window.model;
    await refreshSetup();
    renderSettings();
  } catch (error) {
    showAppStatus(t("status.modelDeleteFailed", { detail: describeError(error) }));
  }
}

async function changeModelsDir(path: string): Promise<void> {
  try {
    const settings = await ipc.setModelsDir(path);
    state.modelsDirSetting = settings.models_dir;
    await refreshSetup();
  } catch (error) {
    showAppStatus(t("status.dirChangeFailed", { detail: describeError(error) }));
    renderSettings();
  }
}

async function pickModelsDir(): Promise<void> {
  try {
    const selected = await openDirectoryDialog({
      directory: true,
      multiple: false,
      title: t("models.dirDialogTitle"),
    });
    if (typeof selected === "string" && selected !== "") {
      await changeModelsDir(selected);
    }
  } catch (error) {
    showAppStatus(t("status.dirPickFailed", { detail: describeError(error) }));
  }
}

function handleDownloadProgress(payload: {
  kind: DownloadKind;
  id: string;
  downloaded: number;
  total: number;
}): void {
  if (state.download === null) {
    return;
  }
  if (payload.kind !== state.download.kind || payload.id !== state.download.id) {
    return;
  }
  state.download = {
    ...state.download,
    downloaded: payload.downloaded,
    total: payload.total > 0 ? payload.total : state.download.total,
  };
  renderSettings();
}

async function handleDownloadDone(payload: { kind: DownloadKind; id: string }): Promise<void> {
  state.download = null;
  renderSettings();
  if (payload.kind === "runtime") {
    await refreshSetup();
    showAppStatus(t("status.runtimeInstalled"));
    // 再起動はバックエンド (install_runtime) が行う。状態だけ取り直す。
    await refreshEngineStatus();
    return;
  }

  const entry = state.catalog.find((item) => item.id === payload.id);
  await refreshSetup();
  showAppStatus(t("status.modelInstalled"));
  const active = state.setup?.active_model ?? null;
  if (entry !== undefined && active === null && !isModelResolved(state.setup)) {
    try {
      await ipc.setActiveModel(entry.file);
      await refreshSetup();
      await refreshEngineStatus();
    } catch (error) {
      showAppStatus(t("status.modelSwitchFailed", { detail: describeError(error) }));
    }
  }
}

function handleDownloadError(payload: {
  kind: DownloadKind;
  id: string;
  message: LocalizedMessage;
}): void {
  const matches =
    state.download !== null &&
    payload.kind === state.download.kind &&
    payload.id === state.download.id;
  if (state.download !== null && !matches) {
    return;
  }
  state.download = null;
  renderSettings();
  showAppStatus(localize(payload.message));
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
    handleJobEvent({
      kind: "error",
      jobId: payload.jobId,
      message: payload.message,
      messageKey: payload.messageKey,
    });
  });
  onTranslationSuperseded((payload) => {
    handleTranslationSuperseded(payload.jobId);
  });
}

function bindDownloadEvents(): void {
  onDownloadProgress((payload) => {
    handleDownloadProgress(payload);
  });
  onDownloadDone((payload) => {
    void handleDownloadDone(payload);
  });
  onDownloadError((payload) => {
    handleDownloadError(payload);
  });
}

function bindUpdateEvents(): void {
  onUpdateProgress((payload) => {
    handleUpdateProgress(payload);
  });
  onUpdateDone((payload) => {
    handleUpdateDone(payload);
  });
  onUpdateError((payload) => {
    handleUpdateError(payload);
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

  settingsToggleEl.addEventListener("click", () => {
    setView(state.view === "settings" ? "translate" : "settings");
  });

  settingsBackEl.addEventListener("click", () => {
    setView("translate");
  });

  runtimeInstallEl.addEventListener("click", () => {
    void startRuntimeInstall();
  });

  runtimeCancelEl.addEventListener("click", () => {
    void cancelDownload("runtime");
  });

  modelsDirPickEl.addEventListener("click", () => {
    void pickModelsDir().finally(() => {
      renderSettings();
    });
  });

  modelsDirDefaultEl.addEventListener("change", () => {
    if (modelsDirDefaultEl.checked) {
      void changeModelsDir("");
    }
  });

  modelsDirCustomEl.addEventListener("change", () => {
    if (modelsDirCustomEl.checked) {
      void pickModelsDir().finally(() => {
        renderSettings();
      });
    }
  });

  licenseLinkEl.addEventListener("click", () => {
    void openUrl(LICENSE_URL);
  });

  llamaLinkEl.addEventListener("click", () => {
    void openUrl(LLAMA_URL);
  });

  updateToggleEl.addEventListener("click", () => {
    toggleUpdatePopover();
  });

  updateInstallEl.addEventListener("click", () => {
    void startUpdateDownload();
  });

  updateRetryEl.addEventListener("click", () => {
    void checkForUpdates(true);
  });

  updateReleaseEl.addEventListener("click", () => {
    void openUrl(RELEASES_PAGE_URL);
  });

  updateCancelEl.addEventListener("click", () => {
    void cancelUpdateDownload();
  });

  autoUpdateCheckEl.addEventListener("change", () => {
    void handleAutoUpdateCheckChange();
  });

  floatingEnabledEl.addEventListener("change", () => {
    void handleFloatingToggle();
  });

  backgroundEnabledEl.addEventListener("change", () => {
    void handleBackgroundEnabledChange();
  });

  backgroundResidentEngineEl.addEventListener("change", () => {
    void handleBackgroundResidentEngineChange();
  });

  floatingModelButtonEl.addEventListener("click", () => {
    setFloatingModelOpen(!state.floatingModelOpen, true);
  });

  floatingModelButtonEl.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setFloatingModelOpen(true, false);
    } else if (event.key === "Escape" && state.floatingModelOpen) {
      event.preventDefault();
      setFloatingModelOpen(false, true);
    }
  });

  floatingModelListEl.addEventListener("keydown", (event) => {
    if (!state.floatingModelOpen) {
      return;
    }
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        moveFloatingModelFocus(1);
        break;
      case "ArrowUp":
        event.preventDefault();
        moveFloatingModelFocus(-1);
        break;
      case "Home":
        event.preventDefault();
        focusFloatingModelEdge(false);
        break;
      case "End":
        event.preventDefault();
        focusFloatingModelEdge(true);
        break;
      case "Enter": {
        event.preventDefault();
        const option = document.activeElement;
        if (option instanceof HTMLElement) {
          const value = option.dataset["value"];
          if (value !== undefined) {
            void chooseFloatingModel(value);
          }
        }
        break;
      }
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        setFloatingModelOpen(false, true);
        break;
      case "Tab":
        setFloatingModelOpen(false, false);
        break;
      default:
        break;
    }
  });

  // ポップオーバーの外をクリックしたら閉じる。
  document.addEventListener("pointerdown", (event) => {
    if (!state.floatingModelOpen) {
      return;
    }
    const target = event.target;
    if (!(target instanceof Node)) {
      return;
    }
    if (
      floatingModelPopoverEl.contains(target) ||
      floatingModelButtonEl.contains(target)
    ) {
      return;
    }
    setFloatingModelOpen(false, false);
  });

  // トレイメニューの「設定」で設定ビューを開く。
  onOpenSettings(() => {
    setView("settings");
  });

  floatingHotkeyEl.addEventListener("click", () => {
    if (!state.floatingRecording) {
      startHotkeyRecording();
    }
  });

  floatingHotkeyEl.addEventListener("keydown", (event) => {
    if (!state.floatingRecording) {
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      void stopHotkeyRecording(true);
      return;
    }
    if (event.key === "Tab") {
      // フォーカス移動は妨げない。blur 側で記録を終える。
      return;
    }
    if (hotkeySuspendPending) {
      // 現在のホットキーを OS から外し終えるまで、押下は記録しない。
      event.preventDefault();
      return;
    }
    if (
      event.key === "Control" ||
      event.key === "Shift" ||
      event.key === "Alt" ||
      event.key === "Meta" ||
      event.key === "CapsLock"
    ) {
      event.preventDefault();
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const combo = comboFromEvent(event);
    if (combo === null) {
      state.floatingError = t("settings.floating.hotkeyInvalid");
      renderFloatingSettings();
      return;
    }
    void saveRecordedHotkey(combo);
  });

  floatingHotkeyEl.addEventListener("blur", () => {
    if (state.floatingRecording) {
      void stopHotkeyRecording(true);
    }
  });

  for (const item of settingsNavItems) {
    item.addEventListener("click", () => {
      const section = item.dataset["section"] ?? "";
      if (isSettingsSection(section)) {
        selectSettingsSection(section);
      }
    });
  }

  settingsNavEl.addEventListener("keydown", (event) => {
    const currentIndex = settingsNavItems.findIndex(
      (item) => item.getAttribute("aria-current") === "true",
    );
    let nextIndex: number;
    switch (event.key) {
      case "ArrowDown":
        nextIndex = (currentIndex + 1) % settingsNavItems.length;
        break;
      case "ArrowUp":
        nextIndex = (currentIndex - 1 + settingsNavItems.length) % settingsNavItems.length;
        break;
      case "Home":
        nextIndex = 0;
        break;
      case "End":
        nextIndex = settingsNavItems.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    const next = settingsNavItems[nextIndex];
    const section = next?.dataset["section"] ?? "";
    if (isSettingsSection(section)) {
      selectSettingsSection(section, true);
    }
  });

  for (const input of uiLanguageInputs) {
    input.addEventListener("change", () => {
      if (input.checked) {
        const value = input.value;
        if (isUiLanguage(value)) {
          void changeUiLanguage(value);
        }
      }
    });
  }

  document.addEventListener("pointerdown", (event) => {
    if (updatePopoverEl.hidden) {
      return;
    }
    const target = event.target;
    if (!(target instanceof Node)) {
      return;
    }
    if (updateControlEl?.contains(target) === true) {
      return;
    }
    setUpdateOpen(false);
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !updatePopoverEl.hidden) {
      setUpdateOpen(false);
      updateToggleEl.focus();
    }
  });
}

async function bootstrap(): Promise<void> {
  bindTranslationEvents();
  bindDownloadEvents();
  bindUpdateEvents();
  bindUi();
  glossaryEditor.addRow();
  await loadSettings();
  applyTheme(state.theme);
  renderThemeButton(themeToggleEl, state.theme);
  glossaryEditor.relocalize();
  applyTranslations();
  renderAll();
  renderUpdate();
  await refreshEngineStatus();
  await refreshCatalog();
  await refreshSetup();
  if (state.autoUpdateCheck) {
    void checkForUpdates(false);
  }
}

void bootstrap();
