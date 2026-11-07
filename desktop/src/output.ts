/**
 * 訳文ペイン。状態は 4 種類だけ:
 *   idle / loading / translating / error
 * 訳文テキストは状態と独立して表示できる (エラー時も直前の訳文は残す)。
 */

export type OutputActionKind = "restart" | "retry";

export interface OutputAction {
  readonly kind: OutputActionKind;
  readonly label: string;
}

export interface OutputView {
  readonly kind: "idle" | "loading" | "translating" | "error";
  readonly message: string;
  readonly action: OutputAction | null;
  readonly text: string;
}

export interface OutputElements {
  readonly notice: HTMLElement;
  readonly noticeText: HTMLElement;
  readonly placeholder: HTMLElement;
  readonly text: HTMLElement;
  readonly action: HTMLButtonElement;
  readonly copy: HTMLButtonElement;
  readonly copyLabel: HTMLElement;
  readonly copyStatus: HTMLElement;
}

const COPY_LABEL = "コピー";
const COPY_DONE_LABEL = "コピーしました";
const COPY_DONE_DURATION = 1500;
const STATUS_DURATION = 2500;

export class OutputPane {
  private readonly elements: OutputElements;
  private readonly onAction: (kind: OutputActionKind) => void;
  private view: OutputView = { kind: "idle", message: "", action: null, text: "" };
  private labelTimer: number | undefined;
  private statusTimer: number | undefined;

  constructor(elements: OutputElements, onAction: (kind: OutputActionKind) => void) {
    this.elements = elements;
    this.onAction = onAction;

    elements.action.addEventListener("click", () => {
      const action = this.view.action;
      if (action !== null) {
        this.onAction(action.kind);
      }
    });
    elements.copy.addEventListener("click", () => {
      void this.copy();
    });
  }

  render(view: OutputView): void {
    this.view = view;
    const { elements } = this;
    const hasText = view.text.trim() !== "";
    const showNotice = view.kind !== "idle" && view.message !== "";
    const action = view.action;

    elements.notice.hidden = !showNotice;
    elements.notice.dataset["kind"] = view.kind;
    elements.noticeText.textContent = view.message;

    elements.action.hidden = action === null;
    if (action !== null) {
      elements.action.textContent = action.label;
    }

    elements.placeholder.hidden = hasText || view.kind !== "idle";
    elements.text.hidden = !hasText;
    elements.text.textContent = view.text;

    elements.copy.setAttribute("aria-disabled", String(!hasText));
  }

  private async copy(): Promise<void> {
    const text = this.view.text;
    if (text.trim() === "") {
      this.showCopyStatus("訳文がありません", true);
      return;
    }
    const copied = await writeClipboard(text);
    if (!copied) {
      this.showCopyStatus("コピーできませんでした", true);
      return;
    }
    this.showCopyStatus("", false);
    this.elements.copyLabel.textContent = COPY_DONE_LABEL;
    window.clearTimeout(this.labelTimer);
    this.labelTimer = window.setTimeout(() => {
      this.elements.copyLabel.textContent = COPY_LABEL;
    }, COPY_DONE_DURATION);
  }

  private showCopyStatus(message: string, isError: boolean): void {
    const { copyStatus } = this.elements;
    copyStatus.textContent = message;
    if (isError) {
      copyStatus.dataset["state"] = "error";
    } else {
      copyStatus.removeAttribute("data-state");
    }
    window.clearTimeout(this.statusTimer);
    if (message !== "") {
      this.statusTimer = window.setTimeout(() => {
        copyStatus.textContent = "";
        copyStatus.removeAttribute("data-state");
      }, STATUS_DURATION);
    }
  }
}

/** クリップボードへ書く。権限や API が無いときは textarea + execCommand に落とす。 */
async function writeClipboard(text: string): Promise<boolean> {
  try {
    const clipboard = navigator.clipboard;
    if (clipboard !== undefined && typeof clipboard.writeText === "function") {
      await clipboard.writeText(text);
      return true;
    }
  } catch {
    // 権限が無い場合はフォールバックへ
  }
  return legacyCopy(text);
}

function legacyCopy(text: string): boolean {
  const helper = document.createElement("textarea");
  helper.value = text;
  helper.setAttribute("readonly", "");
  helper.style.position = "fixed";
  helper.style.top = "-1000px";
  helper.style.opacity = "0";
  document.body.append(helper);
  helper.select();
  helper.setSelectionRange(0, text.length);
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  helper.remove();
  return copied;
}
