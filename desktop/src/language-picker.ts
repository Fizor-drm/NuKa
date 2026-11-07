/**
 * 言語ボタン + 検索付きリストボックス。
 *
 * ARIA は combobox / listbox:
 * - トリガー: aria-haspopup="listbox" / aria-expanded / aria-controls
 * - 検索欄: role="combobox" / aria-autocomplete="list" / aria-activedescendant
 * - リスト: role="listbox"、各行は role="option" + aria-selected
 *
 * キーボードは ↑↓ で移動、Enter で決定、Escape で閉じる、文字入力で絞り込み。
 */

import { icon } from "./icons";
import { languageLabel, type LanguageOption } from "./languages";

export interface LanguagePickerElements {
  readonly trigger: HTMLButtonElement;
  readonly label: HTMLElement;
  readonly popover: HTMLElement;
  readonly search: HTMLInputElement;
  readonly list: HTMLUListElement;
  readonly empty: HTMLElement;
}

export class LanguagePicker {
  private readonly elements: LanguagePickerElements;
  private readonly options: readonly LanguageOption[];
  private readonly onSelect: (code: string) => void;
  private readonly optionIdPrefix: string;
  private value = "";
  private filtered: readonly LanguageOption[] = [];
  private activeIndex = -1;
  private open = false;

  constructor(
    elements: LanguagePickerElements,
    options: readonly LanguageOption[],
    initialValue: string,
    onSelect: (code: string) => void,
  ) {
    this.elements = elements;
    this.options = options;
    this.onSelect = onSelect;
    this.optionIdPrefix = `${elements.list.id}-opt`;
    this.setValue(initialValue, false);
    this.bind();
  }

  /** 表示だけ更新する。ユーザー操作由来のときだけ `notify` を true にする。 */
  setValue(code: string, notify = true): void {
    this.value = code;
    this.elements.label.textContent = languageLabel(code);
    if (notify) {
      this.onSelect(code);
    }
  }

  private bind(): void {
    const { trigger, search, list } = this.elements;

    trigger.addEventListener("click", () => {
      if (this.open) {
        this.closeMenu(true);
      } else {
        this.openMenu();
      }
    });

    trigger.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        this.openMenu();
      }
    });

    search.addEventListener("input", () => {
      this.filter(search.value);
    });

    search.addEventListener("keydown", (event) => {
      this.onSearchKeyDown(event);
    });

    list.addEventListener("click", (event) => {
      const item = findOption(event.target);
      if (item === null) {
        return;
      }
      const code = item.dataset["code"];
      if (code !== undefined) {
        this.choose(code);
      }
    });

    // マウスが乗った行をキーボードのアクティブ位置と揃える (スクロールはしない)。
    list.addEventListener("pointermove", (event) => {
      const item = findOption(event.target);
      if (item === null) {
        return;
      }
      const index = Number(item.dataset["index"]);
      if (Number.isInteger(index) && index >= 0 && index !== this.activeIndex) {
        this.setActive(index, false);
      }
    });

    document.addEventListener("pointerdown", (event) => {
      if (!this.open) {
        return;
      }
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }
      if (this.elements.popover.contains(target) || trigger.contains(target)) {
        return;
      }
      this.closeMenu(false);
    });
  }

  private openMenu(): void {
    if (this.open) {
      return;
    }
    this.open = true;
    this.elements.popover.hidden = false;
    this.elements.trigger.setAttribute("aria-expanded", "true");
    this.elements.search.setAttribute("aria-expanded", "true");
    this.elements.search.value = "";
    this.filter("");
    const selected = this.filtered.findIndex((option) => option.code === this.value);
    this.setActive(selected >= 0 ? selected : 0);
    this.elements.search.focus();
  }

  private closeMenu(focusTrigger: boolean): void {
    if (!this.open) {
      return;
    }
    this.open = false;
    this.elements.popover.hidden = true;
    this.elements.trigger.setAttribute("aria-expanded", "false");
    this.elements.search.setAttribute("aria-expanded", "false");
    this.elements.search.removeAttribute("aria-activedescendant");
    this.elements.search.value = "";
    this.activeIndex = -1;
    if (focusTrigger) {
      this.elements.trigger.focus();
    }
  }

  private choose(code: string): void {
    this.setValue(code, true);
    this.closeMenu(true);
  }

  private filter(term: string): void {
    const query = term.trim().toLowerCase();
    this.filtered =
      query === ""
        ? this.options
        : this.options.filter((option) =>
            `${option.label} ${option.native} ${option.code}`.toLowerCase().includes(query),
          );
    this.renderList();
    this.elements.empty.hidden = this.filtered.length > 0;
    if (this.filtered.length === 0) {
      this.activeIndex = -1;
      this.elements.search.removeAttribute("aria-activedescendant");
      return;
    }
    this.setActive(0);
  }

  private renderList(): void {
    const fragment = document.createDocumentFragment();
    this.filtered.forEach((option, index) => {
      const item = document.createElement("li");
      item.id = `${this.optionIdPrefix}-${index}`;
      item.className = "lang-option";
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", String(option.code === this.value));
      item.dataset["code"] = option.code;
      item.dataset["index"] = String(index);

      const text = document.createElement("span");
      text.className = "lang-option-text";

      const label = document.createElement("span");
      label.className = "lang-option-label";
      label.textContent = option.label;

      const native = document.createElement("span");
      native.className = "lang-option-native";
      native.textContent = option.native;

      text.append(label, native);

      const check = icon("check", 14);
      check.classList.add("lang-option-check");

      item.append(text, check);
      fragment.append(item);
    });
    this.elements.list.replaceChildren(fragment);
  }

  private setActive(index: number, scroll = true): void {
    const items = Array.from(
      this.elements.list.querySelectorAll<HTMLElement>('[role="option"]'),
    );
    const item = items[index];
    if (item === undefined) {
      this.activeIndex = -1;
      this.elements.search.removeAttribute("aria-activedescendant");
      return;
    }
    this.activeIndex = index;
    items.forEach((option, optionIndex) => {
      if (optionIndex === index) {
        option.setAttribute("data-active", "true");
      } else {
        option.removeAttribute("data-active");
      }
    });
    this.elements.search.setAttribute("aria-activedescendant", item.id);
    if (scroll) {
      item.scrollIntoView({ block: "nearest" });
    }
  }

  private move(delta: number): void {
    if (this.filtered.length === 0) {
      return;
    }
    const current = this.activeIndex < 0 ? -1 : this.activeIndex;
    const next = (current + delta + this.filtered.length) % this.filtered.length;
    this.setActive(next);
  }

  private onSearchKeyDown(event: KeyboardEvent): void {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        this.move(1);
        break;
      case "ArrowUp":
        event.preventDefault();
        this.move(-1);
        break;
      case "Home":
        event.preventDefault();
        this.setActive(0);
        break;
      case "End":
        event.preventDefault();
        this.setActive(this.filtered.length - 1);
        break;
      case "Enter": {
        event.preventDefault();
        const option = this.filtered[this.activeIndex];
        if (option !== undefined) {
          this.choose(option.code);
        }
        break;
      }
      case "Escape":
        event.preventDefault();
        this.closeMenu(true);
        break;
      case "Tab":
        this.closeMenu(false);
        break;
      default:
        break;
    }
  }
}

function findOption(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) {
    return null;
  }
  return target.closest<HTMLElement>('[role="option"]');
}
