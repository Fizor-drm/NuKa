/**
 * 用語集の行エディタ。
 * 行は「原文の用語 → 訳文の用語」の 1 対で、空の対は送信時に落とす。
 */

import { icon } from "./icons";
import { t } from "./i18n";
import type { GlossaryPair } from "./ipc";

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
