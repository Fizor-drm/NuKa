//! NuKa デスクトップアプリ (Powered by PLaMo 3 Translate)。
//!
//! Rust 側は PFML3 のプロンプト組み立て (`prompt`)、llama-server の子プロセス管理
//! (`engine`)、言語検出 (`detect`)、設定の永続化 (`settings`) を持ち、
//! `commands` がフロントエンドとの IPC を束ねる。

pub mod catalog;
pub mod commands;
pub mod detect;
pub mod engine;
pub mod floating;
pub mod installer;
pub mod message;
pub mod prompt;
pub mod settings;
pub mod updater;

use tauri::Manager;

use crate::commands::JobManager;
use crate::engine::Engine;
use crate::floating::FloatingState;
use crate::installer::DownloadManager;
use crate::settings::SettingsStore;
use crate::updater::UpdateState;

/// Tauri が管理する共有状態。
pub struct AppState {
    pub engine: Engine,
    pub settings: SettingsStore,
    pub jobs: JobManager,
    pub downloads: DownloadManager,
    pub updates: UpdateState,
    pub floating: FloatingState,
}

pub fn run() {
    let application = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        floating::on_hotkey(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let settings = match SettingsStore::for_app(app.handle()) {
                Ok(store) => store,
                // 保存先が使えないときは temp_dir に逃げず、メモリ内だけで動かす。
                Err(error) => {
                    eprintln!("設定の保存先を用意できません (この起動では保存しません): {error}");
                    SettingsStore::in_memory()
                }
            };
            match settings.path() {
                Some(path) => eprintln!("設定ファイル: {}", path.display()),
                None => eprintln!("設定ファイル: 永続化なし (メモリ内のみ)"),
            }

            let engine = Engine::new(app.handle().clone());
            let initial = settings.get();
            // 中断されたアップデートの残骸を起動時に掃除する。
            updater::clean_cache(app.handle());
            app.manage(AppState {
                engine: engine.clone(),
                settings,
                jobs: JobManager::new(),
                downloads: DownloadManager::new(),
                updates: UpdateState::new(),
                floating: FloatingState::new(),
            });

            // 非表示のフローティングウィンドウを用意し、ホットキーを登録する。
            // 登録はプラグインの都合で別スレッドに逃がしてある (setup を止めない)。
            if let Err(error) = floating::setup(app.handle()) {
                eprintln!(
                    "フローティングウィンドウを初期化できません: {}",
                    error.fallback
                );
            }

            // 起動と同時に llama-server を起こす。準備できるまで UI には starting が見える。
            tauri::async_runtime::spawn(async move {
                engine.start(&initial).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            floating::handle_window_event(window, event);
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_engine_status,
            commands::restart_engine,
            commands::get_settings,
            commands::set_settings,
            commands::detect_language,
            commands::translate,
            commands::cancel_translation,
            commands::set_floating_settings,
            commands::hide_floating,
            commands::set_main_target_lang,
            commands::get_catalog,
            commands::get_setup_state,
            commands::install_runtime,
            commands::install_model,
            commands::cancel_download,
            commands::delete_model,
            commands::set_active_model,
            commands::set_models_dir,
            commands::check_update,
            commands::download_update,
            commands::cancel_update_download,
            commands::install_update,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri アプリを構築できません");

    application.run(|app_handle, event| match event {
        // ユーザーが最後のウィンドウを閉じた (code が None)。子プロセスを止めたら、
        // Tauri/wry の後片付けに進まずプロセスを終える。
        // WebView2 の破棄が戻らずアプリが残ることがあるための安全策で、
        // 通常の終了は 1 秒ほどで終わる。Exit の処理は防御として残す。
        tauri::RunEvent::ExitRequested { code: None, .. } => {
            stop_engine(app_handle);
            std::process::exit(0);
        }
        // プログラム側からの終了要求 (code あり) や Exit では子プロセスだけ止める。
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
            stop_engine(app_handle);
        }
        _ => {}
    });
}

/// llama-server を確実に止める。二重に呼ばれても安全。
fn stop_engine(app_handle: &tauri::AppHandle) {
    if let Some(state) = app_handle.try_state::<AppState>() {
        state.engine.kill_now();
    }
}
