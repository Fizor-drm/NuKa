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
}

export interface SettingsPatch {
  gpu_layers?: number;
  context_tokens?: number;
  theme?: ThemeSetting;
  auto_translate?: boolean;
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
