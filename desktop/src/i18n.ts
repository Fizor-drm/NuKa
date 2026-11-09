/**
 * UI の表示言語 (ja / en / zh)。
 *
 * - 静的な文言は `data-i18n` / `data-i18n-aria-label` / `data-i18n-title` /
 *   `data-i18n-placeholder` を `applyTranslations()` で差し替える。
 * - 動的に作る文言は `t(key, params)` を使う。
 * - バックエンドから届く `{ key, params, fallback }` は `localize()` で解決する。
 *   辞書に無いキーは生の fallback を表示する。
 */

export type UiLanguage = "ja" | "en" | "zh";

export interface LocalizedMessage {
  key: string;
  params?: Record<string, string>;
  fallback: string;
}

export const UI_LANGUAGES: ReadonlyArray<{ code: UiLanguage; label: string }> = [
  { code: "ja", label: "日本語" },
  { code: "en", label: "English" },
  { code: "zh", label: "简体中文" },
];

type Dict = Record<string, string>;

const ja: Dict = {
  "app.title": "NuKa",
  "app.brandAria": "NuKa（Powered by PLaMo 3 Translate）",
  "app.brandTitle": "Powered by PLaMo 3 Translate",

  "header.autoTranslate": "自動翻訳",
  "header.autoTranslateTitle": "入力が止まったら自動で翻訳する",
  "header.glossary": "用語集",
  "header.glossaryTitle": "用語の訳し方を固定する",
  "header.themeTitle": "テーマ: {theme}",
  "header.themeAria": "テーマを切り替える（現在: {theme}）",
  "header.updateTitle": "アップデートを確認",
  "header.updateAria": "アップデートを確認",
  "header.settingsTitle": "設定",
  "header.settingsAria": "設定を開く",

  "engine.starting": "エンジンを起動しています",
  "engine.loading": "モデルを読み込んでいます",
  "engine.ready": "準備完了",
  "engine.error": "エンジンエラー",

  "update.title": "アップデート",
  "update.idle": "アップデートを確認します。",
  "update.checking": "最新リリースを確認しています…",
  "update.latest": "最新です ({version})",
  "update.latestUnknown": "最新です。",
  "update.available": "{version} に更新できます ({size})",
  "update.downloading": "ダウンロードしています…",
  "update.ready": "インストーラーを起動して NuKa を終了します…",
  "update.installing": "インストーラーを起動しています…",
  "update.unknownError": "更新を確認できませんでした。",
  "update.cancel": "取り消し",
  "update.update": "更新する",
  "update.retry": "再試行",
  "update.release": "リリースページを開く",
  "update.currentVersion": "現在のバージョン: v{version}",
  "update.autoCheck": "起動時にアップデートを確認 (GitHub にアクセスします)",
  "update.privacy":
    "確認は起動時に GitHub へ 1 回アクセスするだけです。IP アドレスは GitHub に伝わります。",
  "update.checkFailed": "更新を確認できませんでした: {detail}",
  "update.downloadStartFailed": "ダウンロードを開始できません: {detail}",
  "update.cancelFailed": "ダウンロードを取り消せません: {detail}",
  "update.installFailed": "インストーラーを起動できません: {detail}",
  "update.readyStatus": "v{version} の準備ができました。",

  "lang.autoDetect": "自動検出",
  "lang.searchPlaceholder": "言語を検索",
  "lang.sourceSearchAria": "原文の言語を検索",
  "lang.targetSearchAria": "訳文の言語を検索",
  "lang.sourceListAria": "原文の言語",
  "lang.targetListAria": "訳文の言語",
  "lang.noMatches": "該当する言語がありません",
  "lang.swapTitle": "言語を入れ替える",
  "lang.swapTitleDisabled": "自動検出のままでは入れ替えできません",
  "lang.swapAria": "原文と訳文の言語を入れ替える",
  "lang.detectedTitle": "確度 {percent}%",
  "lang.detected": "{language}を検出",

  "source.aria": "原文",
  "source.placeholder": "訳したい文章を入力してください",
  "form.groupAria": "文体",
  "form.none": "指定しない",
  "form.polite": "です・ます",
  "form.plain": "だ・である",
  "source.charCount": "{count} 文字",
  "source.clear": "原文をクリア",
  "translate.button": "翻訳する",

  "output.placeholder": "訳文がここに表示されます",
  "output.translating": "翻訳しています…",
  "output.loading": "モデルを読み込んでいます…",
  "output.copyTitle": "訳文をコピー",
  "output.copy": "コピー",
  "output.copied": "コピーしました",
  "output.copyNoText": "訳文がありません",
  "output.copyFailed": "コピーできませんでした",
  "output.retry": "再試行",
  "output.restart": "再起動",
  "output.openSettings": "設定を開く",
  "output.runtimeMissing": "ランタイムが未導入です。設定からインストールしてください。",
  "output.modelMissing": "モデルが導入されていません。設定からインストールしてください。",
  "output.modelNotSelected": "使用するモデルを選んでください。",
  "output.engineStartFailed": "エンジンを起動できませんでした。",
  "output.translationFailed": "翻訳できませんでした。",

  "floating.noSelection": "テキストが選択されていません",
  "floating.copy": "コピー",
  "floating.copied": "コピーしました",
  "floating.copyFailed": "コピーできませんでした",
  "floating.translating": "翻訳しています…",
  "floating.engineStarting": "エンジンを起動しています…",
  "floating.engineLoading": "モデルを読み込んでいます…",
  "floating.close": "閉じる",
  "floating.targetLangAria": "訳先の言語",

  "glossary.title": "用語集",
  "glossary.hint": "登録した用語はこの訳語で固定します。",
  "glossary.add": "行を追加",
  "glossary.empty": "用語がまだ登録されていません。「行を追加」から登録できます。",
  "glossary.sourcePlaceholder": "原文の用語",
  "glossary.targetPlaceholder": "訳文の用語",
  "glossary.remove": "この用語を削除",

  "theme.light": "ライト",
  "theme.dark": "ダーク",
  "theme.system": "システム",

  "settings.back": "翻訳に戻る",
  "settings.backLabel": "戻る",
  "settings.title": "設定",
  "settings.navAria": "設定セクション",
  "settings.runtimeHeading": "ランタイム",
  "settings.languageHeading": "言語",
  "settings.languageDescription": "画面の表示言語を選びます。",
  "settings.languageAria": "表示言語",
  "settings.updateHeading": "アップデート",
  "settings.modelsHeading": "モデル",
  "settings.licenseHeading": "ライセンス",
  "settings.floating.heading": "フローティングウィンドウ",
  "settings.floating.enable": "グローバルショートカットで翻訳ウィンドウを表示する",
  "settings.floating.hotkeyLabel": "ホットキー",
  "settings.floating.hotkeyRecord": "キーを押して記録",
  "settings.floating.hotkeyRecording": "キーを押してください… (Esc でキャンセル)",
  "settings.floating.usage": "テキストを選択して {hotkey} を押すと、翻訳ウィンドウが表示されます。",
  "settings.floating.hotkeyInvalid": "Ctrl・Alt・Shift + 英数字の組み合わせで指定してください。",
  "settings.floating.hotkeyConflict":
    "このホットキーはほかのアプリが使用中のため登録できませんでした。設定は保存されましたが、このキーはまだ有効ではありません。",
  "settings.floating.modelLabel": "フローティング用モデル",
  "settings.floating.modelSame": "アクティブモデルと同じ",
  "settings.floating.modelAria": "フローティング用モデル",
  "settings.floating.modelEmpty": "インストール済みのモデルがありません。",
  "settings.floating.modelNote":
    "常時起動を軽くするには、小型モデル (0.8B など) を選んでください。専用モデルを選ぶと、そのモデルで常時起動します。",
  "settings.floating.modelSharedNote":
    "「アクティブモデルと同じ」ファイルを専用モデルに選んだ場合、フローティング翻訳はメイン用のエンジンで処理します (2 本目は起動しません)。常時起動が ON のときは、そのエンジンを維持します。",
  "settings.floating.adminNote":
    "管理者権限で実行されているアプリの上では、選択テキストを取得できません。",
  "settings.floating.saveFailed": "設定を保存できませんでした: {detail}",

  "settings.backgroundHeading": "バックグラウンド",
  "settings.background.enable": "ウィンドウを閉じてもトレイに常駐する",
  "settings.background.residentEngine":
    "フローティング用エンジンを常時起動しておく (専用モデル選択時)",
  "settings.background.residentEngineNote":
    "翻訳ウィンドウ用の小型モデルを起動したままにします。オフにすると、使うたびにモデルを読み込み直します。",
  "settings.background.residentEngineNoteShared":
    "フローティング用モデルがアクティブモデルと同じファイルのため、2 本目は起動せず、メイン用のエンジン 1 本を維持します。オフにすると、メイン画面を閉じた時点で停止します。",
  "settings.background.residentEngineNeedsModel":
    "常時起動は、フローティング用モデルを選ぶと有効になります。未選択のときは、使うたびにモデルを読み込み直します。",
  "settings.background.quitNote": "アプリの終了は、トレイアイコンのメニュー「終了」から行います。",
  "settings.background.disabledNote":
    "常駐しない場合、メイン画面を閉じるとアプリが終了します。",
  "settings.background.saveFailed": "設定を保存できませんでした: {detail}",

  "runtime.checking": "確認しています…",
  "runtime.notInstalled": "未導入。ランタイムをインストールすると翻訳を始められます。",
  "runtime.installed": "導入済み: llama.cpp {version} (CUDA 12.4)",
  "runtime.external": "外部のランタイム",
  "runtime.usage": " / 使用量 {size}",
  "runtime.note": "導入済みです。再ダウンロードは不要です。",
  "runtime.install": "インストール",
  "runtime.cancel": "取消",
  "runtime.meta": "llama.cpp b12136 (CUDA 12.4) / 合計 約 650 MB",

  "models.aboutSize": "約 {size}",
  "models.dirLabel": "インストール先",
  "models.defaultDir": "既定の場所",
  "models.customDir": "別の場所",
  "models.pick": "参照…",
  "models.note": "変更は今後のダウンロードから適用されます。インストール済みのモデルは移動されません。",
  "models.install": "インストール",
  "models.use": "使用する",
  "models.active": "使用中",
  "models.delete": "削除",
  "models.deleteActiveTitle": "使用中のモデルは削除できません",
  "models.deleteTitle": "このモデルを削除",
  "models.cancel": "取消",
  "models.installAria": "{size} {quant} をインストール",
  "models.useAria": "{size} {quant} を使用する",
  "models.deleteAria": "{size} {quant} を削除",
  "models.cancelAria": "{size} {quant} のダウンロードを取り消す",
  "models.dirDialogTitle": "モデルのインストール先",

  "license.text":
    "モデルは PLaMo Community License です。個人利用と非商用は無償で、商用利用には登録が必要です。",
  "license.open": "PLaMo Community License を開く",
  "license.llama": "llama.cpp (MIT) を開く",

  "status.settingsSaveFailed": "設定を保存できませんでした: {detail}",
  "status.catalogFailed": "カタログを取得できません: {detail}",
  "status.setupFailed": "導入状態を取得できません: {detail}",
  "status.runtimeInstalled": "ランタイムをインストールしました。",
  "status.modelInstalled": "モデルをインストールしました。",
  "status.runtimeInstallFailed": "ランタイムをインストールできません: {detail}",
  "status.modelInstallFailed": "モデルをインストールできません: {detail}",
  "status.cancelFailed": "ダウンロードを取り消せません: {detail}",
  "status.modelSwitchFailed": "モデルを切り替えられません: {detail}",
  "status.modelDeleteFailed": "モデルを削除できません: {detail}",
  "status.dirChangeFailed": "インストール先を変更できません: {detail}",
  "status.dirPickFailed": "フォルダを選択できません: {detail}",

  // バックエンド: エンジン
  "engine.startingDetail": "llama-server を起動しています。",
  "engine.loadingDetail": "モデルを読み込んでいます。",
  "engine.readyDetail": "127.0.0.1:{port} で待ち受けています。",
  "engine.portUnavailable": "空きポートを確保できません: {detail}",
  "engine.readyTimeout":
    "エンジンの準備ができませんでした。もう一度お試しください。",
  "engine.startTimeout": "llama-server が {seconds} 秒以内に準備できませんでした。",
  "engine.exited": "llama-server が終了しました。",
  "engine.exitedWithCode": "llama-server が終了しました (終了コード {code})。",
  "engine.statusUnknown": "llama-server の状態を確認できません: {detail}",
  "engine.superseded": "エンジンの起動が置き換えられました。",
  "engine.modelNotInstalled":
    "モデルが未導入です。設定でインストールしてください。確認した場所: {paths}",
  "engine.runtimeNotInstalled":
    "ランタイムが未導入。設定でインストールしてください。確認した場所: {paths}",
  "engine.spawnFailed": "llama-server を起動できません: {detail}",
  "engine.logOpenFailed": "ログファイルを開けません: {detail}",
  "engine.logCloneFailed": "ログファイルを複製できません: {detail}",
  "engine.bothNotInstalled": "ランタイムとモデルが未導入です。設定からインストールしてください。(モデル: {model_paths} / ランタイム: {runtime_paths})",

  // バックエンド: インストーラー
  "installer.clientCreate": "ダウンロード用クライアントを作れません: {detail}",
  "installer.downloadStart": "ダウンロードを開始できません: {detail}",
  "installer.httpStatus": "配布元が {status} を返しました。",
  "installer.savePath": "保存先を作れません: {detail}",
  "installer.readFailed": "ダウンロードを読めません: {detail}",
  "installer.timeout": "ダウンロードがタイムアウトしました。",
  "installer.writeFailed": "ダウンロードを書き込めません: {detail}",
  "installer.verifyFailed": "ダウンロードを確認できません: {detail}",
  "installer.remoteSizeMismatch": "配布元のサイズが想定と違います ({actual} / {expected} バイト)。",
  "installer.sizeMismatch": "ダウンロードのサイズが一致しません ({actual} / {expected} バイト)。",
  "installer.cancelled": "ダウンロードを中止しました。",
  "installer.renameFailed": "ダウンロードを配置できません: {detail}",
  "installer.localDataFailed": "保存先を取得できません: {detail}",
  "installer.unzipFailed": "展開処理が落ちました: {detail}",
  "installer.zipOpen": "zip を開けません: {detail}",
  "installer.zipRead": "zip を読めません: {detail}",
  "installer.zipItem": "zip の項目を読めません: {detail}",
  "installer.zipWrite": "展開先に書けません: {detail}",
  "installer.zipCopy": "展開に失敗しました: {detail}",
  "installer.runtimeMissingFiles": "展開後に必要なファイルがありません: {files}",
  "installer.swapRemove": "既存のランタイムを置き換えられません: {detail}",
  "installer.swapRename": "ランタイムを配置できません: {detail}",
  "installer.catalogUnknown": "カタログにないモデルです: {id}",
  "installer.modelNotFound": "インストール済みのモデルにありません: {file}",
  "installer.modelActive": "使用中のモデルは削除できません。別のモデルを選んでから削除してください。",
  "installer.modelSymlink": "シンボリックリンクは削除できません。",
  "installer.deleteFailed": "モデルを削除できません: {detail}",
  "installer.dirAbsolute": "フォルダは絶対パスで指定してください。",
  "installer.dirLocalOnly": "フォルダはドライブ文字から始まるローカルパスで指定してください。",
  "installer.dirUnc": "UNC パス (\\\\server\\share) は指定できません。ローカルのフォルダを選んでください。",
  "installer.dirNoParent": "パスに「..」は指定できません。",
  "installer.dirCreate": "フォルダを作れません: {detail}",
  "installer.modelExistingMismatch":
    "サイズの合わないファイルが既にあります: {detail}。削除してから再実行してください。",
  "installer.modelPathNotFile": "保存先にファイル以外のものがあります: {detail}",
  "installer.existingCheck": "既存ファイルを確認できません: {detail}",
  "installer.busy": "別のダウンロードが進行中です ({id})。完了するか取消してから再試行してください。",
  "installer.verifyInfo": "配布ファイルの検証に失敗しました: {detail}",
  "installer.partSizeMismatch": "ダウンロードの書き込み量が一致しません ({actual} / {expected} バイト)。",
  "installer.hashMismatch": "配布ファイルの SHA-256 が一致しません: {file}",
  "installer.hashFailed": "SHA-256 を計算できませんでした。",
  "installer.archiveSizeMismatch": "配布ファイルのサイズが一致しません ({actual} / {expected} バイト): {file}",
  "installer.archiveMissing": "配布ファイルが見つかりません: {file}",
  "installer.runtimeSizeExpected": "配布元のサイズが想定と違います ({actual} / {expected} バイト) {url}",
  "installer.downloadSizeExpected": "ダウンロードのサイズが一致しません ({actual} / {expected} バイト) {url}",
  "installer.modelFileMissing": "モデルファイルが見つかりません。再インストールしてください: {detail}",
  "installer.deleteNameMismatch": "削除対象のファイル名が一致しません: {detail}",

  "installer.remoteSizeUnknown": "配布元のサイズを確認できませんでした。",
  "installer.dataTooLarge": "配布元のデータが想定より大きいため中止しました ({actual} / {expected} バイト)。",
  "installer.registerFailed": "モデルの登録に失敗しました: {detail}",
  "installer.modelExistingMismatchSimple":
    "サイズの合わないファイルが既にあります: {detail}",

  // バックエンド: アップデーター
  "updater.checkClient": "更新確認用クライアントを作れません: {detail}",
  "updater.checkFailed": "更新情報を取得できませんでした: {detail}",
  "updater.httpStatus": "更新情報を取得できませんでした (HTTP {status})。",
  "updater.rateLimited": "レート制限です。時間をおいて再試行してください。",
  "updater.releaseMissing": "リリースが見つかりませんでした。",
  "updater.responseTooLarge": "更新情報が大きすぎます。",
  "updater.responseRead": "更新情報を読めませんでした: {detail}",
  "updater.responseTimeout": "更新情報の取得がタイムアウトしました。",
  "updater.parseFailed": "更新情報を解析できませんでした: {detail}",
  "updater.assetMissing": "更新ファイル (NuKa_*_x64-setup.exe) が見つかりませんでした。",
  "updater.assetName": "更新ファイル名が想定と違います。",
  "updater.assetSize": "更新ファイルのサイズが想定の範囲外です。",
  "updater.assetUrl": "更新ファイルの取得先が想定と違います。",
  "updater.digestMissing": "更新ファイルの検証情報が取得できませんでした。",
  "updater.versionCompare": "バージョンを比較できませんでした (現在 {current} / 最新 {latest})。",
  "updater.hashMismatch": "更新ファイルの SHA-256 が一致しません。",
  "updater.hashFailed": "SHA-256 を計算できませんでした。",
  "updater.digestInvalid": "更新ファイルの検証情報が不正です。",
  "updater.sizeMismatch": "更新ファイルのサイズが一致しません ({actual} / {expected} バイト)。",
  "updater.downloadClient": "ダウンロード用クライアントを作れません: {detail}",
  "updater.downloadStart": "ダウンロードを開始できません: {detail}",
  "updater.savePath": "保存先を作れません: {detail}",
  "updater.readFailed": "ダウンロードを読めません: {detail}",
  "updater.timeout": "ダウンロードがタイムアウトしました。",
  "updater.writeFailed": "ダウンロードを書き込めません: {detail}",
  "updater.tooLarge": "更新ファイルが想定より大きいため中止しました。",
  "updater.renameFailed": "更新ファイルを配置できません: {detail}",
  "updater.cancelled": "ダウンロードを中止しました。",
  "updater.cachePath": "保存先を取得できません: {detail}",
  "updater.installPath": "更新ファイルの保存先が想定と違います。",
  "updater.installMissing": "更新ファイルが見つかりません。再ダウンロードしてください。",
  "updater.installSymlink": "更新ファイルがシンボリックリンクのため中止しました。",
  "updater.installSpawn": "インストーラーを起動できません: {detail}",
  "updater.fileCheck": "更新ファイルを確認できません: {detail}",
  "updater.notChecked": "先に更新を確認してください。",
  "updater.notDownloaded": "更新ファイルがありません。先にダウンロードしてください。",
  "updater.assetError": "更新ファイルを検証できませんでした: {detail}",
  "updater.downloadFailed": "更新ファイルをダウンロードできませんでした: {detail}",

  // バックエンド: 翻訳コマンド
  "commands.noModelOutput": "モデルが訳文を返しませんでした。",
  "commands.unreachable": "llama-server に届きません: {detail}",
  "commands.responseTimeout": "llama-server の応答が止まりました。",
  "commands.responseRead": "llama-server の応答を読めません: {detail}",
  "commands.responseTooLong": "llama-server の応答が長すぎます。",
  "commands.translationTooLong": "訳文が長すぎます。",
  "commands.settingsSave": "設定を保存できませんでした。",
  "commands.settingsRead": "設定を読み込めませんでした。",
  "error.raw": "{detail}",
};

const en: Dict = {
  "app.title": "NuKa",
  "app.brandAria": "NuKa (Powered by PLaMo 3 Translate)",
  "app.brandTitle": "Powered by PLaMo 3 Translate",

  "header.autoTranslate": "Auto translate",
  "header.autoTranslateTitle": "Translate automatically when you stop typing",
  "header.glossary": "Glossary",
  "header.glossaryTitle": "Pin translations for specific terms",
  "header.themeTitle": "Theme: {theme}",
  "header.themeAria": "Switch theme (current: {theme})",
  "header.updateTitle": "Check for updates",
  "header.updateAria": "Check for updates",
  "header.settingsTitle": "Settings",
  "header.settingsAria": "Open settings",

  "engine.starting": "Starting engine",
  "engine.loading": "Loading model",
  "engine.ready": "Ready",
  "engine.error": "Engine error",

  "update.title": "Updates",
  "update.idle": "Check for updates.",
  "update.checking": "Checking the latest release…",
  "update.latest": "You're up to date ({version})",
  "update.latestUnknown": "You're up to date.",
  "update.available": "{version} is available ({size})",
  "update.downloading": "Downloading…",
  "update.ready": "Starting the installer and quitting NuKa…",
  "update.installing": "Starting the installer…",
  "update.unknownError": "Could not check for updates.",
  "update.cancel": "Cancel",
  "update.update": "Update",
  "update.retry": "Retry",
  "update.release": "Open release page",
  "update.currentVersion": "Current version: v{version}",
  "update.autoCheck": "Check for updates on startup (contacts GitHub)",
  "update.privacy":
    "This makes one request to GitHub at startup. Your IP address is visible to GitHub.",
  "update.checkFailed": "Could not check for updates: {detail}",
  "update.downloadStartFailed": "Could not start the download: {detail}",
  "update.cancelFailed": "Could not cancel the download: {detail}",
  "update.installFailed": "Could not start the installer: {detail}",
  "update.readyStatus": "v{version} is ready.",

  "lang.autoDetect": "Auto-detect",
  "lang.searchPlaceholder": "Search languages",
  "lang.sourceSearchAria": "Search source languages",
  "lang.targetSearchAria": "Search target languages",
  "lang.sourceListAria": "Source languages",
  "lang.targetListAria": "Target languages",
  "lang.noMatches": "No matching languages",
  "lang.swapTitle": "Swap languages",
  "lang.swapTitleDisabled": "Cannot swap while the source is auto-detect",
  "lang.swapAria": "Swap source and target languages",
  "lang.detectedTitle": "Confidence {percent}%",
  "lang.detected": "Detected: {language}",

  "source.aria": "Source text",
  "source.placeholder": "Enter the text you want to translate",
  "form.groupAria": "Style",
  "form.none": "Unspecified",
  "form.polite": "Polite",
  "form.plain": "Plain",
  "source.charCount": "{count} characters",
  "source.clear": "Clear source text",
  "translate.button": "Translate",

  "output.placeholder": "The translation will appear here",
  "output.translating": "Translating…",
  "output.loading": "Loading the model…",
  "output.copyTitle": "Copy translation",
  "output.copy": "Copy",
  "output.copied": "Copied",
  "output.copyNoText": "Nothing to copy",
  "output.copyFailed": "Could not copy",
  "output.retry": "Retry",
  "output.restart": "Restart",
  "output.openSettings": "Open settings",
  "output.runtimeMissing": "The runtime is not installed. Install it from Settings.",
  "output.modelMissing": "No model is installed. Install one from Settings.",
  "output.modelNotSelected": "Choose the model to use.",
  "output.engineStartFailed": "Could not start the engine.",
  "output.translationFailed": "Translation failed.",

  "floating.noSelection": "No text is selected",
  "floating.copy": "Copy",
  "floating.copied": "Copied",
  "floating.copyFailed": "Could not copy",
  "floating.translating": "Translating…",
  "floating.engineStarting": "Starting the engine…",
  "floating.engineLoading": "Loading the model…",
  "floating.close": "Close",
  "floating.targetLangAria": "Target language",

  "glossary.title": "Glossary",
  "glossary.hint": "Registered terms are always translated this way.",
  "glossary.add": "Add row",
  "glossary.empty": "No terms yet. Use “Add row” to register one.",
  "glossary.sourcePlaceholder": "Source term",
  "glossary.targetPlaceholder": "Target term",
  "glossary.remove": "Remove this term",

  "theme.light": "Light",
  "theme.dark": "Dark",
  "theme.system": "System",

  "settings.back": "Back to translation",
  "settings.backLabel": "Back",
  "settings.title": "Settings",
  "settings.navAria": "Settings sections",
  "settings.runtimeHeading": "Runtime",
  "settings.languageHeading": "Language",
  "settings.languageDescription": "Choose the language used by the interface.",
  "settings.languageAria": "Interface language",
  "settings.updateHeading": "Updates",
  "settings.modelsHeading": "Models",
  "settings.licenseHeading": "License",
  "settings.floating.heading": "Floating window",
  "settings.floating.enable": "Show the translation window with a global shortcut",
  "settings.floating.hotkeyLabel": "Shortcut",
  "settings.floating.hotkeyRecord": "Press keys to record",
  "settings.floating.hotkeyRecording": "Press a key combination… (Esc to cancel)",
  "settings.floating.usage":
    "Select text and press {hotkey} to open the translation window.",
  "settings.floating.hotkeyInvalid":
    "Use Ctrl, Alt, or Shift together with a letter or digit.",
  "settings.floating.hotkeyConflict":
    "Another app already uses this shortcut, so it could not be registered. The setting was saved, but the shortcut is not active yet.",
  "settings.floating.modelLabel": "Model for the floating window",
  "settings.floating.modelSame": "Same as the active model",
  "settings.floating.modelAria": "Model for the floating window",
  "settings.floating.modelEmpty": "No models are installed.",
  "settings.floating.modelNote":
    "Choose a small model (for example 0.8B) to keep the always-on engine light. Selecting a dedicated model enables the always-on engine.",
  "settings.floating.modelSharedNote":
    "When the dedicated model is the same file as the active model, floating translations share the main engine (no second engine is started). With always-on enabled, that engine stays loaded.",
  "settings.floating.adminNote": "Text cannot be captured from apps running as administrator.",
  "settings.floating.saveFailed": "Could not save settings: {detail}",

  "settings.backgroundHeading": "Background",
  "settings.background.enable": "Keep NuKa running in the tray when the window is closed",
  "settings.background.residentEngine":
    "Keep the floating-window engine running (when a dedicated model is selected)",
  "settings.background.residentEngineNote":
    "Keeps the small model for the translation window loaded. Turn it off to reload the model each time you use it.",
  "settings.background.residentEngineNoteShared":
    "The floating model is the same file as the active model, so no second engine is started and the single main engine stays loaded. Turn this off and it stops when the main window is closed.",
  "settings.background.residentEngineNeedsModel":
    "Always-on starts once you pick a model for the floating window. Without one, the model is reloaded each time you use it.",
  "settings.background.quitNote": "To quit NuKa, use “Quit” in the tray icon menu.",
  "settings.background.disabledNote":
    "When NuKa does not stay resident, closing the main window quits the app.",
  "settings.background.saveFailed": "Could not save settings: {detail}",

  "runtime.checking": "Checking…",
  "runtime.notInstalled": "Not installed. Install the runtime to start translating.",
  "runtime.installed": "Installed: llama.cpp {version} (CUDA 12.4)",
  "runtime.external": "external runtime",
  "runtime.usage": " / {size} used",
  "runtime.note": "Installed. No need to download again.",
  "runtime.install": "Install",
  "runtime.cancel": "Cancel",
  "runtime.meta": "llama.cpp b12136 (CUDA 12.4) / about 650 MB total",

  "models.aboutSize": "About {size}",
  "models.dirLabel": "Install location",
  "models.defaultDir": "Default location",
  "models.customDir": "Custom location",
  "models.pick": "Browse…",
  "models.note":
    "Changes apply to future downloads. Installed models are not moved.",
  "models.install": "Install",
  "models.use": "Use",
  "models.active": "In use",
  "models.delete": "Delete",
  "models.deleteActiveTitle": "The active model cannot be deleted",
  "models.deleteTitle": "Delete this model",
  "models.cancel": "Cancel",
  "models.installAria": "Install {size} {quant}",
  "models.useAria": "Use {size} {quant}",
  "models.deleteAria": "Delete {size} {quant}",
  "models.cancelAria": "Cancel downloading {size} {quant}",
  "models.dirDialogTitle": "Model install location",

  "license.text":
    "PLaMo Community License: free for personal and non-commercial use; commercial use requires registration.",
  "license.open": "Open PLaMo Community License",
  "license.llama": "Open llama.cpp (MIT)",

  "status.settingsSaveFailed": "Could not save settings: {detail}",
  "status.catalogFailed": "Could not load the catalog: {detail}",
  "status.setupFailed": "Could not read the install state: {detail}",
  "status.runtimeInstalled": "Runtime installed.",
  "status.modelInstalled": "Model installed.",
  "status.runtimeInstallFailed": "Could not install the runtime: {detail}",
  "status.modelInstallFailed": "Could not install the model: {detail}",
  "status.cancelFailed": "Could not cancel the download: {detail}",
  "status.modelSwitchFailed": "Could not switch the model: {detail}",
  "status.modelDeleteFailed": "Could not delete the model: {detail}",
  "status.dirChangeFailed": "Could not change the install location: {detail}",
  "status.dirPickFailed": "Could not select the folder: {detail}",

  "engine.startingDetail": "Starting llama-server.",
  "engine.loadingDetail": "Loading the model.",
  "engine.readyDetail": "Listening on 127.0.0.1:{port}.",
  "engine.portUnavailable": "Could not reserve a free port: {detail}",
  "engine.readyTimeout": "The engine could not become ready. Please try again.",
  "engine.startTimeout": "llama-server did not become ready within {seconds} seconds.",
  "engine.exited": "llama-server exited.",
  "engine.exitedWithCode": "llama-server exited (exit code {code}).",
  "engine.statusUnknown": "Could not check llama-server: {detail}",
  "engine.superseded": "The engine start was superseded.",
  "engine.modelNotInstalled":
    "No model is installed. Install one from Settings. Checked: {paths}",
  "engine.runtimeNotInstalled":
    "The runtime is not installed. Install it from Settings. Checked: {paths}",
  "engine.spawnFailed": "Could not start llama-server: {detail}",
  "engine.logOpenFailed": "Could not open the log file: {detail}",
  "engine.logCloneFailed": "Could not duplicate the log file: {detail}",
  "engine.bothNotInstalled": "Neither the runtime nor a model is installed. Install them from Settings. (Model: {model_paths} / Runtime: {runtime_paths})",

  "installer.clientCreate": "Could not create the download client: {detail}",
  "installer.downloadStart": "Could not start the download: {detail}",
  "installer.httpStatus": "The server returned {status}.",
  "installer.savePath": "Could not create the destination: {detail}",
  "installer.readFailed": "Could not read the download: {detail}",
  "installer.timeout": "The download timed out.",
  "installer.writeFailed": "Could not write the download: {detail}",
  "installer.verifyFailed": "Could not verify the download: {detail}",
  "installer.remoteSizeMismatch": "The server size does not match ({actual} / {expected} bytes).",
  "installer.sizeMismatch": "The download size does not match ({actual} / {expected} bytes).",
  "installer.cancelled": "Download cancelled.",
  "installer.renameFailed": "Could not place the download: {detail}",
  "installer.localDataFailed": "Could not get the data folder: {detail}",
  "installer.unzipFailed": "Extraction failed: {detail}",
  "installer.zipOpen": "Could not open the zip: {detail}",
  "installer.zipRead": "Could not read the zip: {detail}",
  "installer.zipItem": "Could not read a zip entry: {detail}",
  "installer.zipWrite": "Could not write the extracted file: {detail}",
  "installer.zipCopy": "Extraction failed: {detail}",
  "installer.runtimeMissingFiles": "Required files are missing after extraction: {files}",
  "installer.swapRemove": "Could not replace the existing runtime: {detail}",
  "installer.swapRename": "Could not place the runtime: {detail}",
  "installer.catalogUnknown": "Unknown model in the catalog: {id}",
  "installer.modelNotFound": "Not an installed model: {file}",
  "installer.modelActive": "The active model cannot be deleted. Switch to another model first.",
  "installer.modelSymlink": "Symbolic links cannot be deleted.",
  "installer.deleteFailed": "Could not delete the model: {detail}",
  "installer.dirAbsolute": "Specify an absolute path.",
  "installer.dirLocalOnly": "Specify a local path that starts with a drive letter.",
  "installer.dirUnc": "UNC paths (\\\\server\\share) are not allowed. Choose a local folder.",
  "installer.dirNoParent": "Paths containing “..” are not allowed.",
  "installer.dirCreate": "Could not create the folder: {detail}",
  "installer.modelExistingMismatch":
    "A file with a different size already exists: {detail}. Delete it and try again.",
  "installer.modelPathNotFile": "The destination contains something other than a file: {detail}",
  "installer.existingCheck": "Could not check the existing file: {detail}",
  "installer.busy": "Another download is in progress ({id}). Wait for it or cancel it, then retry.",
  "installer.verifyInfo": "Could not verify the downloaded file: {detail}",
  "installer.partSizeMismatch": "The written size does not match ({actual} / {expected} bytes).",
  "installer.hashMismatch": "SHA-256 mismatch for {file}",
  "installer.hashFailed": "Could not compute SHA-256.",
  "installer.archiveSizeMismatch": "Size mismatch for {file} ({actual} / {expected} bytes)",
  "installer.archiveMissing": "Downloaded file not found: {file}",
  "installer.runtimeSizeExpected": "The server size does not match ({actual} / {expected} bytes) {url}",
  "installer.downloadSizeExpected": "The download size does not match ({actual} / {expected} bytes) {url}",
  "installer.remoteSizeUnknown": "Could not determine the download size.",
  "installer.dataTooLarge": "The server sent more data than expected ({actual} / {expected} bytes).",
  "installer.registerFailed": "Could not register the model: {detail}",
  "installer.modelFileMissing": "The model file is missing. Reinstall it: {detail}",
  "installer.deleteNameMismatch": "The file name to delete does not match: {detail}",

  "installer.modelExistingMismatchSimple": "A file with a different size already exists: {detail}",

  "updater.checkClient": "Could not create the update client: {detail}",
  "updater.checkFailed": "Could not fetch update information: {detail}",
  "updater.httpStatus": "Could not fetch update information (HTTP {status}).",
  "updater.rateLimited": "Rate limited. Please retry later.",
  "updater.releaseMissing": "No release was found.",
  "updater.responseTooLarge": "The update information is too large.",
  "updater.responseRead": "Could not read the update information: {detail}",
  "updater.responseTimeout": "Fetching update information timed out.",
  "updater.parseFailed": "Could not parse the update information: {detail}",
  "updater.assetMissing": "No update file (NuKa_*_x64-setup.exe) was found.",
  "updater.assetName": "The update file name is not expected.",
  "updater.assetSize": "The update file size is out of range.",
  "updater.assetUrl": "The update file location is not expected.",
  "updater.digestMissing": "Could not get verification information for the update file.",
  "updater.versionCompare":
    "Could not compare versions (current {current} / latest {latest}).",
  "updater.hashMismatch": "The update file's SHA-256 does not match.",
  "updater.hashFailed": "Could not compute SHA-256.",
  "updater.digestInvalid": "The update file verification information is invalid.",
  "updater.sizeMismatch": "The update file size does not match ({actual} / {expected} bytes).",
  "updater.downloadClient": "Could not create the download client: {detail}",
  "updater.downloadStart": "Could not start the download: {detail}",
  "updater.savePath": "Could not create the destination: {detail}",
  "updater.readFailed": "Could not read the download: {detail}",
  "updater.timeout": "The download timed out.",
  "updater.writeFailed": "Could not write the download: {detail}",
  "updater.tooLarge": "The update file is larger than expected, so it was aborted.",
  "updater.renameFailed": "Could not place the update file: {detail}",
  "updater.cancelled": "Download cancelled.",
  "updater.cachePath": "Could not get the cache folder: {detail}",
  "updater.installPath": "The update file is in an unexpected location.",
  "updater.installMissing": "The update file is missing. Download it again.",
  "updater.installSymlink": "The update file is a symbolic link, so it was aborted.",
  "updater.installSpawn": "Could not start the installer: {detail}",
  "updater.fileCheck": "Could not check the update file: {detail}",
  "updater.notChecked": "Check for updates first.",
  "updater.notDownloaded": "The update file is missing. Download it first.",
  "updater.assetError": "Could not validate the update file: {detail}",
  "updater.downloadFailed": "Could not download the update file: {detail}",

  "commands.noModelOutput": "The model returned no translation.",
  "commands.unreachable": "Could not reach llama-server: {detail}",
  "commands.responseTimeout": "llama-server stopped responding.",
  "commands.responseRead": "Could not read llama-server's response: {detail}",
  "commands.responseTooLong": "llama-server's response is too long.",
  "commands.translationTooLong": "The translation is too long.",
  "commands.settingsSave": "Could not save settings.",
  "commands.settingsRead": "Could not load settings.",
  "error.raw": "{detail}",
};

const zh: Dict = {
  "app.title": "NuKa",
  "app.brandAria": "NuKa（Powered by PLaMo 3 Translate）",
  "app.brandTitle": "Powered by PLaMo 3 Translate",

  "header.autoTranslate": "自动翻译",
  "header.autoTranslateTitle": "停止输入后自动翻译",
  "header.glossary": "术语表",
  "header.glossaryTitle": "固定特定术语的译法",
  "header.themeTitle": "主题：{theme}",
  "header.themeAria": "切换主题（当前：{theme}）",
  "header.updateTitle": "检查更新",
  "header.updateAria": "检查更新",
  "header.settingsTitle": "设置",
  "header.settingsAria": "打开设置",

  "engine.starting": "正在启动引擎",
  "engine.loading": "正在加载模型",
  "engine.ready": "准备就绪",
  "engine.error": "引擎错误",

  "update.title": "更新",
  "update.idle": "检查更新。",
  "update.checking": "正在检查最新版本…",
  "update.latest": "已是最新版本（{version}）",
  "update.latestUnknown": "已是最新版本。",
  "update.available": "可更新到 {version}（{size}）",
  "update.downloading": "正在下载…",
  "update.ready": "正在启动安装程序并退出 NuKa…",
  "update.installing": "正在启动安装程序…",
  "update.unknownError": "无法检查更新。",
  "update.cancel": "取消",
  "update.update": "更新",
  "update.retry": "重试",
  "update.release": "打开发布页面",
  "update.currentVersion": "当前版本：v{version}",
  "update.autoCheck": "启动时检查更新（会访问 GitHub）",
  "update.privacy": "启动时仅访问 GitHub 一次。GitHub 可看到您的 IP 地址。",
  "update.checkFailed": "无法检查更新：{detail}",
  "update.downloadStartFailed": "无法开始下载：{detail}",
  "update.cancelFailed": "无法取消下载：{detail}",
  "update.installFailed": "无法启动安装程序：{detail}",
  "update.readyStatus": "v{version} 已准备好。",

  "lang.autoDetect": "自动检测",
  "lang.searchPlaceholder": "搜索语言",
  "lang.sourceSearchAria": "搜索原文语言",
  "lang.targetSearchAria": "搜索译文语言",
  "lang.sourceListAria": "原文语言",
  "lang.targetListAria": "译文语言",
  "lang.noMatches": "没有匹配的语言",
  "lang.swapTitle": "交换语言",
  "lang.swapTitleDisabled": "自动检测时无法交换",
  "lang.swapAria": "交换原文和译文语言",
  "lang.detectedTitle": "置信度 {percent}%",
  "lang.detected": "检测到{language}",

  "source.aria": "原文",
  "source.placeholder": "请输入要翻译的文本",
  "form.groupAria": "文体",
  "form.none": "不指定",
  "form.polite": "敬体",
  "form.plain": "常体",
  "source.charCount": "{count} 个字符",
  "source.clear": "清空原文",
  "translate.button": "翻译",

  "output.placeholder": "译文将显示在这里",
  "output.translating": "正在翻译…",
  "output.loading": "正在加载模型…",
  "output.copyTitle": "复制译文",
  "output.copy": "复制",
  "output.copied": "已复制",
  "output.copyNoText": "没有可复制的译文",
  "output.copyFailed": "复制失败",
  "output.retry": "重试",
  "output.restart": "重启",
  "output.openSettings": "打开设置",
  "output.runtimeMissing": "运行时尚未安装。请在设置中安装。",
  "output.modelMissing": "模型尚未安装。请在设置中安装。",
  "output.modelNotSelected": "请选择要使用的模型。",
  "output.engineStartFailed": "无法启动引擎。",
  "output.translationFailed": "翻译失败。",

  "floating.noSelection": "未选择文本",
  "floating.copy": "复制",
  "floating.copied": "已复制",
  "floating.copyFailed": "复制失败",
  "floating.translating": "正在翻译…",
  "floating.engineStarting": "正在启动引擎…",
  "floating.engineLoading": "正在加载模型…",
  "floating.close": "关闭",
  "floating.targetLangAria": "目标语言",

  "glossary.title": "术语表",
  "glossary.hint": "已登记的术语将固定使用该译法。",
  "glossary.add": "添加一行",
  "glossary.empty": "尚未登记术语。请点击“添加一行”。",
  "glossary.sourcePlaceholder": "原文术语",
  "glossary.targetPlaceholder": "译文术语",
  "glossary.remove": "删除此术语",

  "theme.light": "浅色",
  "theme.dark": "深色",
  "theme.system": "跟随系统",

  "settings.back": "返回翻译",
  "settings.backLabel": "返回",
  "settings.title": "设置",
  "settings.navAria": "设置分区",
  "settings.runtimeHeading": "运行时",
  "settings.languageHeading": "语言",
  "settings.languageDescription": "选择界面的显示语言。",
  "settings.languageAria": "界面语言",
  "settings.updateHeading": "更新",
  "settings.modelsHeading": "模型",
  "settings.licenseHeading": "许可证",
  "settings.floating.heading": "浮动窗口",
  "settings.floating.enable": "使用全局快捷键显示翻译窗口",
  "settings.floating.hotkeyLabel": "快捷键",
  "settings.floating.hotkeyRecord": "按下按键进行记录",
  "settings.floating.hotkeyRecording": "请按下组合键…（Esc 取消）",
  "settings.floating.usage": "选中文本后按 {hotkey}，即可显示翻译窗口。",
  "settings.floating.hotkeyInvalid": "请使用 Ctrl、Alt 或 Shift 与字母或数字的组合。",
  "settings.floating.hotkeyConflict":
    "该快捷键已被其他应用占用，无法注册。设置已保存，但此快捷键尚未生效。",
  "settings.floating.modelLabel": "浮动窗口模型",
  "settings.floating.modelSame": "与当前模型相同",
  "settings.floating.modelAria": "浮动窗口模型",
  "settings.floating.modelEmpty": "没有已安装的模型。",
  "settings.floating.modelNote":
    "为减轻常驻引擎的负担，请选择小模型（如 0.8B）。选择专用模型后，将使用该模型常驻运行。",
  "settings.floating.modelSharedNote":
    "当专用模型与当前模型为同一文件时，浮动翻译会共用主引擎（不会启动第二个引擎）。开启常驻运行时，该引擎会保持运行。",
  "settings.floating.adminNote": "无法从以管理员权限运行的应用中获取所选文本。",
  "settings.floating.saveFailed": "无法保存设置：{detail}",

  "settings.backgroundHeading": "后台运行",
  "settings.background.enable": "关闭窗口后仍驻留在系统托盘",
  "settings.background.residentEngine": "保持浮动窗口引擎常驻运行（选择专用模型时）",
  "settings.background.residentEngineNote":
    "保持翻译窗口用的小模型处于加载状态。关闭后，每次使用时都会重新加载模型。",
  "settings.background.residentEngineNoteShared":
    "浮动模型与当前模型为同一文件，因此不会启动第二个引擎，只保持一个主引擎。关闭后，关闭主窗口时即停止。",
  "settings.background.residentEngineNeedsModel":
    "选择浮动窗口模型后即可常驻运行。未选择时，每次使用都会重新加载模型。",
  "settings.background.quitNote": "退出应用请使用托盘图标菜单中的“退出”。",
  "settings.background.disabledNote": "不驻留时，关闭主窗口即退出应用。",
  "settings.background.saveFailed": "无法保存设置：{detail}",

  "runtime.checking": "正在检查…",
  "runtime.notInstalled": "未安装。安装运行时后即可开始翻译。",
  "runtime.installed": "已安装：llama.cpp {version} (CUDA 12.4)",
  "runtime.external": "外部运行时",
  "runtime.usage": " / 已用 {size}",
  "runtime.note": "已安装，无需重新下载。",
  "runtime.install": "安装",
  "runtime.cancel": "取消",
  "runtime.meta": "llama.cpp b12136 (CUDA 12.4) / 共约 650 MB",

  "models.aboutSize": "约 {size}",
  "models.dirLabel": "安装位置",
  "models.defaultDir": "默认位置",
  "models.customDir": "自定义位置",
  "models.pick": "浏览…",
  "models.note": "更改仅影响之后的下载。已安装的模型不会被移动。",
  "models.install": "安装",
  "models.use": "使用",
  "models.active": "使用中",
  "models.delete": "删除",
  "models.deleteActiveTitle": "使用中的模型无法删除",
  "models.deleteTitle": "删除此模型",
  "models.cancel": "取消",
  "models.installAria": "安装 {size} {quant}",
  "models.useAria": "使用 {size} {quant}",
  "models.deleteAria": "删除 {size} {quant}",
  "models.cancelAria": "取消下载 {size} {quant}",
  "models.dirDialogTitle": "模型安装位置",

  "license.text":
    "模型采用 PLaMo Community License。个人及非商业用途免费，商业用途需要注册。",
  "license.open": "打开 PLaMo Community License",
  "license.llama": "打开 llama.cpp (MIT)",

  "status.settingsSaveFailed": "无法保存设置：{detail}",
  "status.catalogFailed": "无法获取模型列表：{detail}",
  "status.setupFailed": "无法获取安装状态：{detail}",
  "status.runtimeInstalled": "运行时已安装。",
  "status.modelInstalled": "模型已安装。",
  "status.runtimeInstallFailed": "无法安装运行时：{detail}",
  "status.modelInstallFailed": "无法安装模型：{detail}",
  "status.cancelFailed": "无法取消下载：{detail}",
  "status.modelSwitchFailed": "无法切换模型：{detail}",
  "status.modelDeleteFailed": "无法删除模型：{detail}",
  "status.dirChangeFailed": "无法更改安装位置：{detail}",
  "status.dirPickFailed": "无法选择文件夹：{detail}",

  "engine.startingDetail": "正在启动 llama-server。",
  "engine.loadingDetail": "正在加载模型。",
  "engine.readyDetail": "正在 127.0.0.1:{port} 上监听。",
  "engine.portUnavailable": "无法获取空闲端口：{detail}",
  "engine.readyTimeout": "引擎未能准备就绪。请再试一次。",
  "engine.startTimeout": "llama-server 未能在 {seconds} 秒内准备就绪。",
  "engine.exited": "llama-server 已退出。",
  "engine.exitedWithCode": "llama-server 已退出（退出码 {code}）。",
  "engine.statusUnknown": "无法检查 llama-server 状态：{detail}",
  "engine.superseded": "引擎启动已被替换。",
  "engine.modelNotInstalled": "模型尚未安装。请在设置中安装。已检查：{paths}",
  "engine.runtimeNotInstalled": "运行时尚未安装。请在设置中安装。已检查：{paths}",
  "engine.spawnFailed": "无法启动 llama-server：{detail}",
  "engine.logOpenFailed": "无法打开日志文件：{detail}",
  "engine.logCloneFailed": "无法复制日志文件：{detail}",
  "engine.bothNotInstalled": "运行时和模型均未安装。请在设置中安装。（模型：{model_paths} / 运行时：{runtime_paths}）",

  "installer.clientCreate": "无法创建下载客户端：{detail}",
  "installer.downloadStart": "无法开始下载：{detail}",
  "installer.httpStatus": "服务器返回了 {status}。",
  "installer.savePath": "无法创建保存位置：{detail}",
  "installer.readFailed": "无法读取下载内容：{detail}",
  "installer.timeout": "下载超时。",
  "installer.writeFailed": "无法写入下载内容：{detail}",
  "installer.verifyFailed": "无法确认下载内容：{detail}",
  "installer.remoteSizeMismatch": "服务器返回的大小与预期不符（{actual} / {expected} 字节）。",
  "installer.sizeMismatch": "下载大小不一致（{actual} / {expected} 字节）。",
  "installer.cancelled": "下载已取消。",
  "installer.renameFailed": "无法放置下载文件：{detail}",
  "installer.localDataFailed": "无法获取保存位置：{detail}",
  "installer.unzipFailed": "解压失败：{detail}",
  "installer.zipOpen": "无法打开 zip：{detail}",
  "installer.zipRead": "无法读取 zip：{detail}",
  "installer.zipItem": "无法读取 zip 项目：{detail}",
  "installer.zipWrite": "无法写入解压文件：{detail}",
  "installer.zipCopy": "解压失败：{detail}",
  "installer.runtimeMissingFiles": "解压后缺少必要文件：{files}",
  "installer.swapRemove": "无法替换现有运行时：{detail}",
  "installer.swapRename": "无法放置运行时：{detail}",
  "installer.catalogUnknown": "模型列表中不存在：{id}",
  "installer.modelNotFound": "已安装的模型中没有：{file}",
  "installer.modelActive": "使用中的模型无法删除。请先切换到其他模型。",
  "installer.modelSymlink": "无法删除符号链接。",
  "installer.deleteFailed": "无法删除模型：{detail}",
  "installer.dirAbsolute": "请指定绝对路径。",
  "installer.dirLocalOnly": "请指定以盘符开头的本地路径。",
  "installer.dirUnc": "不能指定 UNC 路径（\\\\server\\share）。请选择本地文件夹。",
  "installer.dirNoParent": "路径中不能包含“..”。",
  "installer.dirCreate": "无法创建文件夹：{detail}",
  "installer.modelExistingMismatch": "已存在大小不符的文件：{detail}。请删除后重试。",
  "installer.modelPathNotFile": "保存位置存在非文件内容：{detail}",
  "installer.existingCheck": "无法检查现有文件：{detail}",
  "installer.busy": "另一个下载正在进行（{id}）。请等待完成或取消后重试。",
  "installer.verifyInfo": "下载文件校验失败：{detail}",
  "installer.partSizeMismatch": "写入大小不一致（{actual} / {expected} 字节）。",
  "installer.hashMismatch": "文件 {file} 的 SHA-256 不一致。",
  "installer.hashFailed": "无法计算 SHA-256。",
  "installer.archiveSizeMismatch": "文件 {file} 大小不一致（{actual} / {expected} 字节）。",
  "installer.archiveMissing": "找不到下载文件：{file}",
  "installer.runtimeSizeExpected": "服务器返回的大小与预期不符（{actual} / {expected} 字节）{url}",
  "installer.downloadSizeExpected": "下载大小不一致（{actual} / {expected} 字节）{url}",
  "installer.remoteSizeUnknown": "无法确认下载大小。",
  "installer.dataTooLarge": "服务器返回的数据超出预期（{actual} / {expected} 字节）。",
  "installer.registerFailed": "模型注册失败：{detail}",
  "installer.modelFileMissing": "找不到模型文件。请重新安装：{detail}",
  "installer.deleteNameMismatch": "要删除的文件名不一致：{detail}",

  "installer.modelExistingMismatchSimple": "已存在大小不符的文件：{detail}",

  "updater.checkClient": "无法创建更新检查客户端：{detail}",
  "updater.checkFailed": "无法获取更新信息：{detail}",
  "updater.httpStatus": "无法获取更新信息（HTTP {status}）。",
  "updater.rateLimited": "已达速率限制，请稍后重试。",
  "updater.releaseMissing": "找不到发布版本。",
  "updater.responseTooLarge": "更新信息过大。",
  "updater.responseRead": "无法读取更新信息：{detail}",
  "updater.responseTimeout": "获取更新信息超时。",
  "updater.parseFailed": "无法解析更新信息：{detail}",
  "updater.assetMissing": "找不到更新文件（NuKa_*_x64-setup.exe）。",
  "updater.assetName": "更新文件名不符合预期。",
  "updater.assetSize": "更新文件大小超出范围。",
  "updater.assetUrl": "更新文件的位置不符合预期。",
  "updater.digestMissing": "无法获取更新文件的校验信息。",
  "updater.versionCompare": "无法比较版本（当前 {current} / 最新 {latest}）。",
  "updater.hashMismatch": "更新文件的 SHA-256 不一致。",
  "updater.hashFailed": "无法计算 SHA-256。",
  "updater.digestInvalid": "更新文件的校验信息无效。",
  "updater.sizeMismatch": "更新文件大小不一致（{actual} / {expected} 字节）。",
  "updater.downloadClient": "无法创建下载客户端：{detail}",
  "updater.downloadStart": "无法开始下载：{detail}",
  "updater.savePath": "无法创建保存位置：{detail}",
  "updater.readFailed": "无法读取下载内容：{detail}",
  "updater.timeout": "下载超时。",
  "updater.writeFailed": "无法写入下载内容：{detail}",
  "updater.tooLarge": "更新文件超出预期大小，已中止。",
  "updater.renameFailed": "无法放置更新文件：{detail}",
  "updater.cancelled": "下载已取消。",
  "updater.cachePath": "无法获取缓存位置：{detail}",
  "updater.installPath": "更新文件的保存位置不符合预期。",
  "updater.installMissing": "找不到更新文件。请重新下载。",
  "updater.installSymlink": "更新文件是符号链接，已中止。",
  "updater.installSpawn": "无法启动安装程序：{detail}",
  "updater.fileCheck": "无法检查更新文件：{detail}",
  "updater.notChecked": "请先检查更新。",
  "updater.notDownloaded": "没有更新文件。请先下载。",
  "updater.assetError": "无法验证更新文件：{detail}",
  "updater.downloadFailed": "无法下载更新文件：{detail}",

  "commands.noModelOutput": "模型没有返回译文。",
  "commands.unreachable": "无法连接 llama-server：{detail}",
  "commands.responseTimeout": "llama-server 停止响应。",
  "commands.responseRead": "无法读取 llama-server 的响应：{detail}",
  "commands.responseTooLong": "llama-server 的响应过长。",
  "commands.translationTooLong": "译文过长。",
  "commands.settingsSave": "无法保存设置。",
  "commands.settingsRead": "无法读取设置。",
  "error.raw": "{detail}",
};

const DICTS: Record<UiLanguage, Dict> = { ja, en, zh };

let current: UiLanguage = "ja";

export function isUiLanguage(value: unknown): value is UiLanguage {
  return value === "ja" || value === "en" || value === "zh";
}

export function setUiLanguage(language: UiLanguage): void {
  current = language;
  document.documentElement.lang = language === "zh" ? "zh-Hans" : language;
}

export function getUiLanguage(): UiLanguage {
  return current;
}

function interpolate(template: string, params?: Record<string, string | number>): string {
  if (params === undefined) {
    return template;
  }
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}

export function hasKey(key: string): boolean {
  return key in DICTS[current] || key in ja;
}

export function t(key: string, params?: Record<string, string | number>): string {
  const template = DICTS[current][key] ?? ja[key];
  return template === undefined ? key : interpolate(template, params);
}

/** バックエンドの {key, params, fallback} を現在の言語で表示する。 */
export function localize(message: string | LocalizedMessage | null | undefined): string {
  if (message === null || message === undefined) {
    return "";
  }
  if (typeof message === "string") {
    return message;
  }
  const template = DICTS[current][message.key] ?? ja[message.key];
  if (template === undefined) {
    return message.fallback;
  }
  return interpolate(template, message.params);
}

/** index.html の data-i18n* 属性を現在の言語で更新する。 */
export function applyTranslations(root: ParentNode = document): void {
  document.title = t("app.title");
  root.querySelectorAll<HTMLElement>("[data-i18n]").forEach((element) => {
    const key = element.dataset["i18n"];
    if (key !== undefined) {
      element.textContent = t(key);
    }
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-aria-label]").forEach((element) => {
    const key = element.dataset["i18nAriaLabel"];
    if (key !== undefined) {
      element.setAttribute("aria-label", t(key));
    }
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((element) => {
    const key = element.dataset["i18nTitle"];
    if (key !== undefined) {
      element.title = t(key);
    }
  });
  root.querySelectorAll<HTMLInputElement>("[data-i18n-placeholder]").forEach((element) => {
    const key = element.dataset["i18nPlaceholder"];
    if (key !== undefined) {
      element.placeholder = t(key);
    }
  });
}
