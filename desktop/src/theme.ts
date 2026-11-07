/**
 * テーマの切り替え。light → dark → system を循環する。
 * 実際の配色は html[data-theme] に対する CSS custom properties が持つ。
 */

import { setIcon, type IconName } from "./icons";
import { t } from "./i18n";
import type { ThemeSetting } from "./ipc";

const CYCLE: readonly ThemeSetting[] = ["light", "dark", "system"];

const ICONS: Record<ThemeSetting, IconName> = {
  light: "sun",
  dark: "moon",
  system: "monitor",
};

export function isThemeSetting(value: string): value is ThemeSetting {
  return CYCLE.includes(value as ThemeSetting);
}

export function themeLabel(theme: ThemeSetting): string {
  return t(`theme.${theme}`);
}

export function nextTheme(theme: ThemeSetting): ThemeSetting {
  const index = CYCLE.indexOf(theme);
  return CYCLE[(index + 1) % CYCLE.length] ?? "light";
}

export function applyTheme(theme: ThemeSetting): void {
  document.documentElement.dataset["theme"] = theme;
}

export function renderThemeButton(button: HTMLButtonElement, theme: ThemeSetting): void {
  const label = themeLabel(theme);
  setIcon(button, ICONS[theme], 16);
  button.title = t("header.themeTitle", { theme: label });
  button.setAttribute("aria-label", t("header.themeAria", { theme: label }));
}
