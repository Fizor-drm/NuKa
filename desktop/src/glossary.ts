/**
 * 用語集の行エディタ。
 * 行は「原文の用語 → 訳文の用語」の 1 対で、空の対は送信時に落とす。
 */

import { icon } from "./icons";
import { t } from "./i18n";
import type { GlossaryPair } from "./ipc";

/**
 * 用語集に登録できる対の上限。
 *
 * Rust 側の `settings::MAX_GLOSSARY_PAIRS` と同じ 200。UI で上限に達したら
 * 行の追加を止める (保存時に黙って切り捨てられて再起動で消えるのを防ぐ)。
 */
export const MAX_GLOSSARY_PAIRS = 200;

export class GlossaryEditor {
  private readonly rows: HTMLElement;
  private readonly emptyHint: HTMLElement;
  private readonly onChange: () => void;

  constructor(rows: HTMLElement, emptyHint: HTMLElement, onChange: () => void) {
    this.rows = rows;
    this.emptyHint = emptyHint;
    this.onChange = onChange;
    this.updateEmptyHint();
  }

  addRow(pair?: GlossaryPair, focus = false): void {
    // 上限に達したら空行を増やさない (保存できない行を作らない)。
    // 保存済みの復元 (pair あり) は上限までしか来ない (Rust 側も 200 で止める)。
    if (pair === undefined && this.isFull()) {
      this.updateEmptyHint();
      return;
    }
    const row = document.createElement("div");
    row.className = "glossary-row";

    const source = document.createElement("input");
    source.type = "text";
    source.className = "glossary-input";
    source.placeholder = t("glossary.sourcePlaceholder");
    source.setAttribute("aria-label", t("glossary.sourcePlaceholder"));
    source.spellcheck = false;

    const arrow = icon("arrowRight", 14);
    arrow.classList.add("glossary-arrow");

    const target = document.createElement("input");
    target.type = "text";
    target.className = "glossary-input";
    target.placeholder = t("glossary.targetPlaceholder");
    target.setAttribute("aria-label", t("glossary.targetPlaceholder"));
    target.spellcheck = false;

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "icon-button";
    remove.title = t("glossary.remove");
    remove.setAttribute("aria-label", t("glossary.remove"));
    remove.append(icon("trash", 14));

    if (pair !== undefined) {
      source.value = pair.source;
      target.value = pair.target;
    }

    source.addEventListener("input", () => {
      this.onChange();
    });
    target.addEventListener("input", () => {
      this.onChange();
    });
    remove.addEventListener("click", () => {
      row.remove();
      this.updateEmptyHint();
      this.onChange();
    });

    row.append(source, arrow, target, remove);
    this.rows.append(row);
    this.updateEmptyHint();

    if (focus) {
      source.focus();
    }
  }

  /**
   * 保存済みの用語集で行を作り直す (起動時の復元)。
   *
   * `onChange` は呼ばない。復元しただけで「編集された」扱いになり、
   * settings.json への保存や再翻訳が走るのを避ける。空なら入力用の空行を 1 つ置く。
   */
  setPairs(pairs: readonly GlossaryPair[]): void {
    this.rows.replaceChildren();
    if (pairs.length === 0) {
      this.addRow();
      return;
    }
    for (const pair of pairs) {
      this.addRow(pair);
    }
  }

  /** 今の行数 (空行も 1 行として数える)。 */
  count(): number {
    return this.rows.childElementCount;
  }

  /** 上限 [`MAX_GLOSSARY_PAIRS`] に達しているか。 */
  isFull(): boolean {
    return this.count() >= MAX_GLOSSARY_PAIRS;
  }

  getPairs(): GlossaryPair[] {
    return Array.from(this.rows.querySelectorAll<HTMLElement>(".glossary-row")).map((row) => {
      const inputs = row.querySelectorAll<HTMLInputElement>("input");
      return {
        source: inputs[0]?.value ?? "",
        target: inputs[1]?.value ?? "",
      };
    });
  }

  /** 表示言語の変更に合わせて、既存行のプレースホルダー等を更新する。 */
  relocalize(): void {
    for (const row of this.rows.querySelectorAll<HTMLElement>(".glossary-row")) {
      const inputs = row.querySelectorAll<HTMLInputElement>("input");
      inputs[0]?.setAttribute("placeholder", t("glossary.sourcePlaceholder"));
      inputs[0]?.setAttribute("aria-label", t("glossary.sourcePlaceholder"));
      inputs[1]?.setAttribute("placeholder", t("glossary.targetPlaceholder"));
      inputs[1]?.setAttribute("aria-label", t("glossary.targetPlaceholder"));
      const remove = row.querySelector<HTMLButtonElement>(".icon-button");
      if (remove !== null) {
        remove.title = t("glossary.remove");
        remove.setAttribute("aria-label", t("glossary.remove"));
      }
    }
  }

  private updateEmptyHint(): void {
    this.emptyHint.hidden = this.rows.childElementCount > 0;
  }
}
