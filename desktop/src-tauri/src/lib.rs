//! NuKa デスクトップアプリ (Powered by PLaMo 3 Translate)。
//!
//! Rust 側は PFML3 のプロンプト組み立て (`prompt`)、llama-server の子プロセス管理
//! (`engine`)、言語検出 (`detect`)、設定の永続化 (`settings`) を持ち、
//! `commands` がフロントエンドとの IPC を束ねる。
//!
//! # トレイ常駐 (バックグラウンド実行)
//!
//! `settings.background.enabled` (既定 true) が有効なとき、メイン画面を閉じても
//! プロセスは終了せずトレイに常駐する (`tray`)。アプリの終了はトレイメニューの
//! 「終了」だけ。バックグラウンド化したときは、`background.keep_engine` が false
//! (既定) なら llama-server を止めて VRAM / メモリを解放し、次に使うときに
//! 起動し直す。

pub mod catalog;
pub mod commands;
pub mod detect;
pub mod engine;
pub mod floating;
pub mod installer;
pub mod message;
pub mod prompt;
pub mod settings;
pub mod tray;
pub mod updater;

use tauri::{Emitter, Manager};

use crate::commands::JobManager;
use crate::engine::Engine;
use crate::floating::FloatingState;
use crate::installer::DownloadManager;
use crate::settings::SettingsStore;
use crate::updater::UpdateState;

/// トレイの「設定」でメイン画面に設定画面を開かせるイベント。
pub const OPEN_SETTINGS_EVENT: &str = "ui:open-settings";

/// トレイの「終了」で、WebView2 の破棄が戻らない場合にプロセスを確実に終える猶予。
const QUIT_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

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
        // 二重起動の防止。2 個目のプロセスはここで終了し、既存のメイン画面を前に出す。
        // ほかの plugin より先に登録し、2 個目が余計な初期化 (ホットキー登録など) を
        // しないようにする。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        floating::on_hotkey(app, shortcut);
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

            // トレイアイコン (バックグラウンド実行の入口)。設定言語に追従する。
            if let Err(error) = tray::setup(app.handle()) {
                eprintln!("トレイを用意できません: {}", error.fallback);
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
        // 最後のウィンドウが閉じられた (code が None)。
        //
        // トレイ常駐が有効なら「終了」ではなくバックグラウンド化として扱い、プロセスを
        // 残す (終了はトレイメニューの「終了」= code Some(0) の経路だけ)。メイン画面が
        // 破棄されていて復帰できない場合は、トレイだけ残っても操作できないため終了する。
        tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            stop_engine(app_handle);
            let can_restore = app_handle
                .get_webview_window(floating::MAIN_LABEL)
                .is_some();
            if floating::background_enabled(app_handle) && can_restore {
                api.prevent_exit();
            } else {
                // 子プロセスを止めたら、Tauri/wry の後片付けに進まずプロセスを終える。
                // WebView2 の破棄が戻らずアプリが残ることがあるための安全策で、
                // 通常の終了は 1 秒ほどで終わる。
                std::process::exit(0);
            }
        }
        // プログラム側からの終了要求 (トレイの「終了」= code あり) や Exit では
        // 子プロセスだけ止める。
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
            stop_engine(app_handle);
        }
        _ => {}
    });
}

/// メインウィンドウを表示して前面に出す。トレイと二重起動の通知から呼ぶ。
///
/// バックグラウンド化で止めていたエンジンもここで起こす (起動完了は待たない)。
pub fn show_main(app_handle: &tauri::AppHandle) {
    let Some(window) = app_handle.get_webview_window(floating::MAIN_LABEL) else {
        eprintln!("メインウィンドウが見つかりません");
        return;
    };
    let _ = window.unminimize();
    if let Err(error) = window.show() {
        eprintln!("メインウィンドウを表示できません: {error}");
    }
    if let Err(error) = window.set_focus() {
        eprintln!("メインウィンドウにフォーカスできません: {error}");
    }
    ensure_engine_started(app_handle);
}

/// メイン画面へ「設定画面を開く」を依頼する (トレイの「設定」)。
pub fn request_settings_view(app_handle: &tauri::AppHandle) {
    if let Err(error) = app_handle.emit_to(floating::MAIN_LABEL, OPEN_SETTINGS_EVENT, ()) {
        eprintln!("{OPEN_SETTINGS_EVENT} を送れません: {error}");
    }
}

/// アプリを終了する。トレイメニューの「終了」から呼ぶ。
///
/// トレイを先に破棄してからイベントループへ終了を要求する。`app.exit` は
/// `ExitRequested { code: Some(0) }` を経由するので、lib.rs のハンドラで
/// llama-server も止まる。
pub fn quit(app_handle: &tauri::AppHandle) {
    // 通知領域にアイコンを残さないよう、終了前にトレイを片付ける。
    tray::destroy(app_handle);
    stop_engine(app_handle);
    app_handle.exit(0);
    // WebView2 の破棄が戻らずプロセスが残ることがあるため、猶予後に確実に終える。
    // (エンジンは停止済みで、Job Object もプロセス終了で閉じる)
    std::thread::spawn(|| {
        std::thread::sleep(QUIT_GRACE);
        std::process::exit(0);
    });
}

/// エンジンが止まっていれば起こす。準備完了は待たずに戻る。
///
/// バックグラウンド化で止めたエンジンを、メインの再表示やホットキー発火で
/// 読み込み直すための入口。進み具合は `engine:status` で全ウィンドウへ流れる。
pub(crate) fn ensure_engine_started(app_handle: &tauri::AppHandle) {
    let Some(state) = app_handle.try_state::<AppState>() else {
        return;
    };
    let engine = state.engine.clone();
    let settings = state.settings.get();
    tauri::async_runtime::spawn(async move {
        engine.ensure_started(&settings).await;
    });
}

/// llama-server を確実に止める。二重に呼ばれても安全。
pub(crate) fn stop_engine(app_handle: &tauri::AppHandle) {
    if let Some(state) = app_handle.try_state::<AppState>() {
        state.engine.kill_now();
    }
}
