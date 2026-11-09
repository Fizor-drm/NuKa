/**
 * 用語集の settings.json 保存ループ。
 *
 * UI (main.ts) のデバウンスから呼ばれ、「今の用語集が保存済みになるまで」を
 * 1 本の直列ループで面倒みる。DOM にも Tauri にも依存しないので、保存の
 * 競合シナリオを単体で検証できる。
 *
 * ## 競合の排除 (監査 Medium 1)
 *
 * 時系列の例: 保存済み A → B に編集 → 保存開始 → 応答前に A へ戻す。
 *
 * 1. `flush()` は同時に 1 つだけ走る (`inFlight`)。実行中に来た要求は捨てるが、
 *    走っているループが **保存のたびに「今の値」を読み直して** 比較するので、
 *    実行中に入った編集は次の周回で必ず保存される。
 * 2. 保存が成功したら「送った値」ではなく「バックエンドが正規化して返した値」を
 *    保存済みの署名にする (Rust の trim / 空行除去 / 上限適用後の値)。
 * 3. 比較と署名更新の間に `await` は無い。JS の割り込みは `await` の境界だけ
 *    なので、「保存済みを更新した直後に編集が割り込んで、その編集の保存予約が
 *    立たない」という隙間は生じない。
 * 4. 保存に失敗したときも、実行中に次の要求が来ていれば 1 回だけ続けて試す
 *    (タイマーを消費済みの要求を取りこぼさない)。
 *
 * 上の例では、B の保存中に A へ戻しても、応答後に現在値 (A) と保存済み (B) を
 * 比べ直して A を保存するため、UI とファイルが食い違ったまま残らない。
 *
 * ## 無限ループの防止
 *
 * バックエンドが送った値と違う用語集を返した場合 (UI 側の上限と整列規則が
 * 揃っている限り起きない) は、同じ内容を送り続けないよう打ち切って返却値を
 * 正とする。
 */

import type { GlossaryPair } from "./ipc";

/** 用語集の署名。順序も含めて比較する (並べ替えは保存内容の変更として扱う)。 */
export function glossarySignature(pairs: readonly GlossaryPair[]): string {
  return JSON.stringify(pairs);
}

export interface GlossarySaveIo {
  /** 今の用語集。保存できる形 (trim 済み・空行なし・上限以内) で返すこと。 */
  read(): readonly GlossaryPair[];
  /** 保存し、バックエンドが正規化して返した用語集を返す。失敗は throw。 */
  save(pairs: readonly GlossaryPair[]): Promise<readonly GlossaryPair[]>;
  /** 直近に保存できた用語集の署名。 */
  savedSignature(): string;
  /** 保存できた署名を記録する。 */
  setSavedSignature(signature: string): void;
  /** 保存に失敗した (UI へ通知する)。 */
  onError(error: unknown): void;
}

export class GlossarySaver {
  private readonly io: GlossarySaveIo;
  /** ループが走っているか。実行中に来た `flush()` はこのループへ委ねる。 */
  private inFlight = false;
  /** 実行中に来た保存要求。失敗したときの取りこぼしを 1 回だけ拾い直す。 */
  private requested = false;

  constructor(io: GlossarySaveIo) {
    this.io = io;
  }

  /**
   * 最新値が保存済みになるまで保存する。
   *
   * 失敗したら署名を進めずに抜ける (次の編集・次の flush で再試行される)。
   * ただし保存中に次の要求が来ていた場合だけは、その要求を取りこぼさないよう
   * 一度だけ続けて試す。`flush()` 自体は reject しない。
   */
  async flush(): Promise<void> {
    if (this.inFlight) {
      // 走っているループが保存のたびに最新値を読み直すので、通常はこれで足りる。
      this.requested = true;
      return;
    }
    this.inFlight = true;
    try {
      while (true) {
        const pairs = this.io.read();
        const sentSignature = glossarySignature(pairs);
        if (sentSignature === this.io.savedSignature()) {
          return;
        }
        let savedSignature: string;
        try {
          savedSignature = glossarySignature(await this.io.save(pairs));
        } catch (error) {
          this.io.onError(error);
          if (!this.requested) {
            return;
          }
          // 失敗の直前に来ていた要求は、このループの再読みでは拾えない
          // (タイマーは消費済み)。その 1 回ぶんだけ続けて試す。
          this.requested = false;
          continue;
        }
        this.io.setSavedSignature(savedSignature);
        this.requested = false;
        if (sentSignature !== savedSignature) {
          // バックエンドがさらに整列した。同じ内容を送り続けると無限ループに
          // なるので、ここで打ち切る (返却値を正とする)。
          return;
        }
      }
    } finally {
      this.inFlight = false;
    }
  }
}
