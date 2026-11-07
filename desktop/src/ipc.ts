/**
 * Rust コアとの IPC。Task A で確定したコントラクトをそのまま写す。
 *
 * - 引数は Tauri の既定どおり camelCase のキーで渡す (`jobId`)。
 * - translate だけは単一の `req` オブジェクトを渡す。
 * - すべての成功/失敗は jobId 付きのイベントで流れてくる。
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type EnginePhase = "starting" | "loading" | "ready" | "error";

export interface EngineStatus {
  state: EnginePhase;
  detail?: string;
}

export interface DetectedLanguage {
  code: string;
  confidence: number;
}

export type ThemeSetting = "light" | "dark" | "system";

export interface Settings {
  model_path: string | null;
  server_path: string | null;
  gpu_layers: number;
  context_tokens: number;
  theme: ThemeSetting;
  auto_translate: boolean;
  auto_update_check: boolean;
  models_dir: string | null;
  installed_runtime: InstalledRuntime | null;
  installed_models: InstalledModel[];
  active_model: string | null;
}

export interface SettingsPatch {
  gpu_layers?: number;
  context_tokens?: number;
  theme?: ThemeSetting;
  auto_translate?: boolean;
  auto_update_check?: boolean;
}

// ---------------------------------------------------------------------------
// 設定画面: カタログとインストール
// ---------------------------------------------------------------------------

export interface CatalogEntry {
  id: string;
  size: string;
  quant: string;
  bytes: number;
  url: string;
  repo: string;
  file: string;
}

export interface InstalledRuntime {
  server_path: string;
  version: string;
  installed_at: number;
}

export interface InstalledModel {
  file: string;
  repo: string;
  bytes: number;
  path: string;
  installed_at: number;
}

export interface RuntimeState {
  installed: boolean;
  server_path?: string;
  version?: string;
  bytes?: number;
}

export interface ModelState {
  resolved: boolean;
}

export interface SetupState {
  runtime: RuntimeState;
  models: InstalledModel[];
  active_model: string | null;
  models_dir: string;
  model: ModelState;
}

export type DownloadKind = "runtime" | "model";

export interface DownloadProgressPayload {
  kind: DownloadKind;
  id: string;
  downloaded: number;
  total: number;
}

export interface DownloadDonePayload {
  kind: DownloadKind;
  id: string;
}

export interface DownloadErrorPayload {
  kind: DownloadKind;
  id: string;
  message: string;
}

// ---------------------------------------------------------------------------
// アップデート
// ---------------------------------------------------------------------------

export interface UpdateAsset {
  name: string;
  size: number;
  url: string;
  digest?: string;
}

export interface UpdateCheck {
  current: string;
  latest?: string;
  available: boolean;
  asset?: UpdateAsset;
  error?: string;
}

export interface UpdateProgressPayload {
  downloaded: number;
  total: number;
}

export interface UpdateDonePayload {
  path: string;
  version: string;
}

export interface UpdateErrorPayload {
  message: string;
}

export interface GlossaryPair {
  source: string;
  target: string;
}

export interface TranslateRequest {
  text: string;
  source_lang?: string;
  target_lang: string;
  form?: "plain" | "polite";
  glossary?: GlossaryPair[];
}

export interface ProgressPayload {
  jobId: number;
  done: number;
  total: number;
}

export interface ChunkPayload {
  jobId: number;
  delta: string;
}

export interface DonePayload {
  jobId: number;
  text: string;
  tokens?: number;
}

export interface ErrorPayload {
  jobId: number;
  message: string;
}

export const ipc = {
  getEngineStatus: (): Promise<EngineStatus> => invoke<EngineStatus>("get_engine_status"),
  restartEngine: (): Promise<void> => invoke<void>("restart_engine"),
  getSettings: (): Promise<Settings> => invoke<Settings>("get_settings"),
  setSettings: (patch: SettingsPatch): Promise<Settings> =>
    invoke<Settings>("set_settings", { patch }),
  detectLanguage: (text: string): Promise<DetectedLanguage | null> =>
    invoke<DetectedLanguage | null>("detect_language", { text }),
  translate: (request: TranslateRequest): Promise<number> =>
    invoke<number>("translate", { req: request }),
  cancelTranslation: (jobId: number): Promise<void> =>
    invoke<void>("cancel_translation", { jobId }),
  getCatalog: (): Promise<CatalogEntry[]> => invoke<CatalogEntry[]>("get_catalog"),
  getSetupState: (): Promise<SetupState> => invoke<SetupState>("get_setup_state"),
  installRuntime: (): Promise<void> => invoke<void>("install_runtime"),
  installModel: (id: string): Promise<InstalledModel> =>
    invoke<InstalledModel>("install_model", { id }),
  cancelDownload: (id: string): Promise<void> => invoke<void>("cancel_download", { id }),
  deleteModel: (file: string): Promise<Settings> => invoke<Settings>("delete_model", { file }),
  setActiveModel: (file: string): Promise<Settings> =>
    invoke<Settings>("set_active_model", { file }),
  setModelsDir: (path: string): Promise<Settings> => invoke<Settings>("set_models_dir", { path }),
  checkUpdate: (): Promise<UpdateCheck> => invoke<UpdateCheck>("check_update"),
  downloadUpdate: (): Promise<void> => invoke<void>("download_update"),
  cancelUpdateDownload: (): Promise<void> => invoke<void>("cancel_update_download"),
  installUpdate: (): Promise<void> => invoke<void>("install_update"),
};

function subscribe<T>(event: string, handler: (payload: T) => void): void {
  void listen<T>(event, (message) => {
    handler(message.payload);
  }).catch(() => undefined);
}

export function onEngineStatus(handler: (status: EngineStatus) => void): void {
  subscribe("engine:status", handler);
}

export function onTranslationProgress(handler: (payload: ProgressPayload) => void): void {
  subscribe("translation:progress", handler);
}

export function onTranslationChunk(handler: (payload: ChunkPayload) => void): void {
  subscribe("translation:chunk", handler);
}

export function onTranslationDone(handler: (payload: DonePayload) => void): void {
  subscribe("translation:done", handler);
}

export function onTranslationError(handler: (payload: ErrorPayload) => void): void {
  subscribe("translation:error", handler);
}

export function onDownloadProgress(handler: (payload: DownloadProgressPayload) => void): void {
  subscribe("download:progress", handler);
}

export function onDownloadDone(handler: (payload: DownloadDonePayload) => void): void {
  subscribe("download:done", handler);
}

export function onDownloadError(handler: (payload: DownloadErrorPayload) => void): void {
  subscribe("download:error", handler);
}

export function onUpdateProgress(handler: (payload: UpdateProgressPayload) => void): void {
  subscribe("update:progress", handler);
}

export function onUpdateDone(handler: (payload: UpdateDonePayload) => void): void {
  subscribe("update:done", handler);
}

export function onUpdateError(handler: (payload: UpdateErrorPayload) => void): void {
  subscribe("update:error", handler);
}
