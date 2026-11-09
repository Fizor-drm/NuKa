//! システムトレイ常駐 (バックグラウンド実行)。
//!
//! メイン画面を閉じてもプロセスを残すためのトレイアイコンとメニューを持つ。
//! 常駐中の「超軽量」はエンジンのライフサイクル分離 (メインを閉じたら
//! llama-server を止める) が担い、ここはその入口だけを提供する。
//!
//! - メニュー: 翻訳ウィンドウを表示 / NuKa を表示 / 設定 / 終了
//! - アイコンの左ダブルクリック: メイン画面を表示
//! - アイコン: `tauri.conf.json` の `bundle.icon` (`icons/icon.ico`) が実行ファイルに
//!   埋め込まれたもの (`default_window_icon`) を使う。追加の画像デコーダを
//!   足さずに済むため。
//!
//! メニューは Tauri 2 のコア API (`tauri::menu` / `tauri::tray`) だけで作る。
//! プラグインは要らない (`tauri` の `tray-icon` feature のみ)。
//!
//! # 文言
//!
//! ネイティブメニューは WebView の辞書 (`src/i18n.ts`) を通らないため、Rust 側に
//! 三言語を持つ。設定言語 (`settings.ui_language`) に追従させ、キーは Web 側と
//! 同じ命名 (`tray.*`) にして `message::LocalizedText` の契約に合わせてある。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::message::{self, LocalizedText};
use crate::{floating, AppState};

/// トレイアイコンの id。二重登録の防止と後片付けに使う。
pub const TRAY_ID: &str = "nuka-tray";

/// メニュー項目の id。`on_menu_event` で動作へ振り分ける。
pub const MENU_TRANSLATE: &str = "tray-translate";
pub const MENU_SHOW_MAIN: &str = "tray-show-main";
pub const MENU_SETTINGS: &str = "tray-settings";
pub const MENU_QUIT: &str = "tray-quit";

/// トレイのメニューから選ばれた動作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// 選択テキストを取得してフローティング翻訳を出す (ホットキーと同じ経路)。
    Translate,
    ShowMain,
    /// メイン画面を出して設定画面を開く。
    Settings,
    Quit,
}

/// メニュー id から動作を決める。未知の id は `None` (何もしない)。
pub fn action_for_menu_id(id: &str) -> Option<TrayAction> {
    match id {
        MENU_TRANSLATE => Some(TrayAction::Translate),
        MENU_SHOW_MAIN => Some(TrayAction::ShowMain),
        MENU_SETTINGS => Some(TrayAction::Settings),
        MENU_QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

/// 三言語の文言。`LocalizedText` と同じキー・日本語フォールバックを持つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayLabel {
    key: &'static str,
    ja: &'static str,
    en: &'static str,
    zh: &'static str,
}

impl TrayLabel {
    const fn new(key: &'static str, ja: &'static str, en: &'static str, zh: &'static str) -> Self {
        Self { key, ja, en, zh }
    }

    /// フロントエンドと同じ契約 (`{ key, fallback }`) のメッセージ。
    pub fn message(self) -> LocalizedText {
        LocalizedText::new(self.key, self.ja)
    }

    /// 設定言語で表示する文言。未知の言語は日本語 (既定) に落とす。
    pub fn resolve(self, language: &str) -> String {
        message::render(language, self.ja, self.en, self.zh).to_string()
    }
}

pub const TRANSLATE_LABEL: TrayLabel = TrayLabel::new(
    "tray.translate",
    "翻訳ウィンドウを表示",
    "Show translation window",
    "显示翻译窗口",
);
pub const SHOW_MAIN_LABEL: TrayLabel =
    TrayLabel::new("tray.showMain", "NuKa を表示", "Show NuKa", "显示 NuKa");
pub const SETTINGS_LABEL: TrayLabel = TrayLabel::new("tray.settings", "設定", "Settings", "设置");
pub const QUIT_LABEL: TrayLabel = TrayLabel::new("tray.quit", "終了", "Quit", "退出");
pub const TOOLTIP_LABEL: TrayLabel = TrayLabel::new(
    "tray.tooltip",
    "NuKa (Powered by PLaMo 3 Translate)",
    "NuKa (Powered by PLaMo 3 Translate)",
    "NuKa（Powered by PLaMo 3 Translate）",
);

/// 設定の表示言語。まだ AppState が無ければ既定 (日本語)。
fn ui_language(app: &AppHandle) -> String {
    app.try_state::<AppState>()
        .map(|state| state.settings.get().ui_language)
        .unwrap_or_else(|| crate::settings::UI_LANGUAGE_DEFAULT.to_string())
}

fn tray_error(key: &str, fallback: &str, error: impl std::fmt::Display) -> LocalizedText {
    LocalizedText::new(key, format!("{fallback}: {error}")).param("detail", error)
}

/// 設定言語に合わせたメニューを作る。
fn build_menu(app: &AppHandle, language: &str) -> tauri::Result<Menu<Wry>> {
    let translate = MenuItem::with_id(
        app,
        MENU_TRANSLATE,
        TRANSLATE_LABEL.resolve(language),
        true,
        None::<&str>,
    )?;
    let show_main = MenuItem::with_id(
        app,
        MENU_SHOW_MAIN,
        SHOW_MAIN_LABEL.resolve(language),
        true,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(
        app,
        MENU_SETTINGS,
        SETTINGS_LABEL.resolve(language),
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(
        app,
        MENU_QUIT,
        QUIT_LABEL.resolve(language),
        true,
        None::<&str>,
    )?;
    Menu::with_items(app, &[&translate, &show_main, &settings, &separator, &quit])
}

/// トレイアイコンを用意する。二重に呼ばれても増えない。
pub fn setup(app: &AppHandle) -> Result<(), LocalizedText> {
    if app.tray_by_id(TRAY_ID).is_some() {
        return Ok(());
    }
    let language = ui_language(app);
    let menu = build_menu(app, &language)
        .map_err(|error| tray_error("tray.menuFailed", "トレイメニューを作れません", error))?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(TOOLTIP_LABEL.resolve(&language))
        .menu(&menu)
        // 左クリックではメニューを開かない (左ダブルクリックでメインを出すため)。
        // 右クリックは従来どおりメニュー。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            on_menu_event(app, event.id().as_ref());
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                crate::show_main(tray.app_handle());
            }
        });

    // アイコンは実行ファイルに埋め込まれた icons/icon.ico。
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    } else {
        eprintln!("トレイのアイコン画像が見つかりません (アイコンなしで常駐します)");
    }

    builder
        .build(app)
        .map_err(|error| tray_error("tray.buildFailed", "トレイアイコンを作れません", error))?;
    Ok(())
}

/// 設定言語が変わったときにメニューを作り直す。トレイが無ければ何もしない。
pub fn refresh_labels(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let language = ui_language(app);
    match build_menu(app, &language) {
        Ok(menu) => {
            if let Err(error) = tray.set_menu(Some(menu)) {
                eprintln!("トレイメニューを差し替えられません: {error}");
            }
        }
        Err(error) => eprintln!("トレイメニューを作り直せません: {error}"),
    }
    if let Err(error) = tray.set_tooltip(Some(TOOLTIP_LABEL.resolve(&language))) {
        eprintln!("トレイのツールチップを変えられません: {error}");
    }
}

/// トレイアイコンを片付ける。終了前と、更新インストーラーの起動前に呼ぶ。
pub fn destroy(app: &AppHandle) {
    if let Some(tray) = app.remove_tray_by_id(TRAY_ID) {
        drop(tray);
    }
}

fn on_menu_event(app: &AppHandle, id: &str) {
    let Some(action) = action_for_menu_id(id) else {
        eprintln!("未知のトレイメニュー: {id}");
        return;
    };
    match action {
        // ホットキーと同じ capture フロー (選択テキスト取得 → 翻訳) を呼ぶ。
        TrayAction::Translate => floating::trigger_capture(app, None),
        TrayAction::ShowMain => crate::show_main(app),
        TrayAction::Settings => {
            crate::show_main(app);
            crate::request_settings_view(app);
        }
        TrayAction::Quit => crate::quit(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_actions() {
        assert_eq!(
            action_for_menu_id(MENU_TRANSLATE),
            Some(TrayAction::Translate)
        );
        assert_eq!(
            action_for_menu_id(MENU_SHOW_MAIN),
            Some(TrayAction::ShowMain)
        );
        assert_eq!(
            action_for_menu_id(MENU_SETTINGS),
            Some(TrayAction::Settings)
        );
        assert_eq!(action_for_menu_id(MENU_QUIT), Some(TrayAction::Quit));

        // 未知の id と空文字は何もしない (メニュー以外のイベントを拾わない)。
        assert_eq!(action_for_menu_id(""), None);
        assert_eq!(action_for_menu_id("tray-unknown"), None);
        assert_eq!(action_for_menu_id("quit"), None);
    }

    #[test]
    fn labels_follow_the_ui_language() {
        assert_eq!(TRANSLATE_LABEL.resolve("ja"), "翻訳ウィンドウを表示");
        assert_eq!(TRANSLATE_LABEL.resolve("en"), "Show translation window");
        assert_eq!(TRANSLATE_LABEL.resolve("zh"), "显示翻译窗口");
        // 未知・空の言語コードは日本語 (既定) に落とす。
        assert_eq!(TRANSLATE_LABEL.resolve("fr"), "翻訳ウィンドウを表示");
        assert_eq!(TRANSLATE_LABEL.resolve(""), "翻訳ウィンドウを表示");

        assert_eq!(QUIT_LABEL.resolve("en"), "Quit");
        assert_eq!(SHOW_MAIN_LABEL.resolve("zh"), "显示 NuKa");
        assert_eq!(SETTINGS_LABEL.resolve("en"), "Settings");
    }

    #[test]
    fn labels_carry_the_localized_text_contract() {
        for label in [
            TRANSLATE_LABEL,
            SHOW_MAIN_LABEL,
            SETTINGS_LABEL,
            QUIT_LABEL,
            TOOLTIP_LABEL,
        ] {
            let message = label.message();
            assert!(message.key.starts_with("tray."), "{}", message.key);
            // フォールバックは日本語 (辞書に key が無いときの表示)。
            assert_eq!(message.fallback, label.resolve("ja"));
            assert!(message.params.is_empty());
            assert!(!message.fallback.is_empty());
        }
        // 三言語すべてが埋まっている (空文字の取りこぼしがない)。
        for label in [
            TRANSLATE_LABEL,
            SHOW_MAIN_LABEL,
            SETTINGS_LABEL,
            QUIT_LABEL,
            TOOLTIP_LABEL,
        ] {
            for language in ["ja", "en", "zh"] {
                assert!(!label.resolve(language).is_empty(), "{language}");
            }
        }
    }
}
