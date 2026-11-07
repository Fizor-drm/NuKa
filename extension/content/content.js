/**
 * 開いているページの本文を切り出し、訳を元のテキストノードへ戻す。
 *
 * 方針:
 * - DOM の構造は変えない。テキストノードの値を書き換えるだけ。要素の置換はしない。
 *   置換すると flex/grid の割り付けが変わり、SPA が持つノード参照も壊れる。
 * - 原文はメモリに取っておく。ページの属性を汚さないので、復元はこのタブ内で完結する。
 * - 段落の中にリンクや強調があるときは、目印を付けて段落全体を 1 回で訳し、
 *   戻ってきた目印の位置で各ノードへ配る。語順が崩れず、リンクも残る。
 * - コード、数式、SVG、入力欄、非表示の要素、translate="no" は訳さない。
 * - 二重注入されても 1 回だけ初期化する。
 */
(() => {
  if (globalThis.__plamoContentReady) return;
  globalThis.__plamoContentReady = true;

  const SKIP_SELECTOR = [
    "script", "style", "noscript", "template", "svg", "math", "canvas",
    "iframe", "object", "embed", "video", "audio", "map", "picture",
    "textarea", "select", "option", "code", "pre", "kbd", "samp", "var",
    '[translate="no"]', ".notranslate",
    // 値の省略 (<div contenteditable>) と plaintext-only も拾う。
    '[contenteditable]:not([contenteditable="false"])',
  ].join(",");

  const BLOCK_TAGS = /^(P|H[1-6]|LI|DT|DD|BLOCKQUOTE|FIGCAPTION|TD|TH|CAPTION|SUMMARY|LABEL|BUTTON|ADDRESS|DIV)$/;

  // 空白を語間に置かない言語。韓国語は分かち書きするので入れない。
  const NO_SPACE_LANGUAGE = ["ja", "zh-hans", "zh-hant"];

  // 段落の中の目印。モデルが落とすことがあるので、崩れたらノードごとの訳に戻す。
  const OPEN = "\u27E6";
  const CLOSE = "\u27E7";
  const MARKER = /\u27E6(\/?)(\d+)\u27E7/g;

  // 目印付きの段落を分割すると目印の対が壊れるので、これ以上はまとめずにノードごとに訳す。
  const GROUP_LIMIT = 380;

  /** @type {WeakSet<Text>} */
  let translated = new WeakSet();
  /** @type {{ node: Text, value: string, written: string }[]} */
  let originals = [];
  let generation = 0;
  let running = false;
  let stopRequested = false;
  let cjkTarget = true;

  function refreshTargetLanguage() {
    try {
      chrome.storage?.local?.get({ targetLang: "ja" }, (stored) => {
        cjkTarget = NO_SPACE_LANGUAGE.includes(stored?.targetLang ?? "ja");
      });
      chrome.storage?.onChanged?.addListener((changes, area) => {
        if (area === "local" && changes.targetLang) {
          cjkTarget = NO_SPACE_LANGUAGE.includes(changes.targetLang.newValue ?? "ja");
        }
      });
    } catch {
      // storage が無い環境では既定のまま。
    }
  }

  function normalize(text) {
    return String(text ?? "").replace(/[ \t\u00a0]+/g, " ").replace(/\s+/g, " ").trim();
  }

  // 同じ要素を何度も getComputedStyle しない。collect() のたびに作り直すので、
  // 前回の走査のあとでページが表示を切り替えても古い判定を引きずらない。
  let stateCache = new WeakMap();
  function elementState(element) {
    if (stateCache.has(element)) return stateCache.get(element);
    const style = getComputedStyle(element);
    const state = {
      // 子孫ごと消えるもの。子が上書きできないので祖先まで見る。
      hidesSubtree:
        style.display === "none" ||
        element.hasAttribute("hidden") ||
        element.getAttribute("aria-hidden") === "true" ||
        (element.tagName === "DIALOG" && !element.hasAttribute("open")) ||
        element.inert === true,
      // 継承するもの。子が visibility: visible などで上書きできるので、
      // 計算済みの値をテキストの親でだけ見る。
      inheritedHidden:
        style.visibility === "hidden" ||
        style.visibility === "collapse" ||
        style.whiteSpace.startsWith("pre"),
    };
    stateCache.set(element, state);
    return state;
  }

  /** 閉じた <details> の中で、最初の <summary> の外にあるか。child は details の直下の要素 (テキスト直下なら null)。 */
  function hiddenByClosedDetails(details, child) {
    if (details.tagName !== "DETAILS" || details.hasAttribute("open")) return false;
    const summary = details.querySelector(":scope > summary");
    return !summary || child !== summary;
  }

  function isVisible(element) {
    if (!element || !element.isConnected) return false;
    if (elementState(element).inheritedHidden) return false;
    let current = element;
    let child = null; // current の直下で、テキストへ向かう側の要素
    while (current && current !== document.documentElement) {
      if (elementState(current).hidesSubtree) return false;
      // 入れ子の details も、通り道の details ごとに判定するので外側が閉じていれば隠れる。
      if (hiddenByClosedDetails(current, child)) return false;
      child = current;
      current = current.parentElement;
    }
    return true;
  }

  function blockOf(node) {
    let current = node.parentElement;
    while (current && current !== document.body) {
      if (BLOCK_TAGS.test(current.tagName)) return current;
      current = current.parentElement;
    }
    // 段落タグが無いページでは、いちばん近い要素を単位にする。
    return node.parentElement || document.body;
  }

  function markedText(parts) {
    // ノードの境目にあった空白だけを残す。前後の字下げや改行は落とす。
    return parts
      .map((part, index) => {
        const gap = index > 0 && (part.spaced || /\s$/.test(parts[index - 1].source ?? "")) ? " " : "";
        return `${gap}${OPEN}${index}${CLOSE}${part.text}${OPEN}/${index}${CLOSE}`;
      })
      .join("");
  }

  function parseMarked(text, count) {
    const parts = new Array(count).fill(null);
    let expected = 0;
    let open = -1;
    let cursor = 0;
    let match;
    MARKER.lastIndex = 0;
    while ((match = MARKER.exec(text))) {
      const chunk = text.slice(cursor, match.index);
      const closing = match[1] === "/";
      const number = Number(match[2]);

      if (open >= 0) {
        if (parts[open] !== null || !closing || number !== open) return null;
        parts[open] = chunk.trim();
        open = -1;
      } else if (chunk.trim()) {
        // 目印の外に訳文が書かれたら、位置を決められない。
        return null;
      }

      if (!closing) {
        if (number !== expected) return null;
        expected += 1;
        open = number;
      }
      cursor = MARKER.lastIndex;
    }
    if (open >= 0 || text.slice(cursor).trim()) return null;
    // 冠詞が消えるなどで空になる断片は許す。目印が揃っていれば位置は決まる。
    // ただし全部が空なら段落が消えるので、ノードごとの訳に戻す。
    return parts.every((part) => part !== null) && parts.some(Boolean) ? parts : null;
  }

  function collect() {
    if (!document.body) return [];
    stateCache = new WeakMap();
    const byBlock = new Map();
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      const parent = node.parentElement;
      const text = normalize(node.nodeValue);
      // 非表示判定は親から上へ見る。ブロックだけを見ると、段落の中の
      // <span hidden> やアイコン文字まで拾ってしまう。
      if (parent && text && !translated.has(node) && !parent.closest(SKIP_SELECTOR) && isVisible(parent)) {
        const block = blockOf(node);
        if (!byBlock.has(block)) byBlock.set(block, []);
        byBlock.get(block).push({
          node,
          block,
          text,
          source: node.nodeValue,
          spaced: /^\s/.test(node.nodeValue ?? ""),
        });
      }
      node = walker.nextNode();
    }

    const segments = [];
    for (const parts of byBlock.values()) {
      if (parts.length === 1 && parts[0].text.length < 2) continue;
      if (parts.length === 1) {
        segments.push({ text: parts[0].text, plain: parts[0].text, parts });
        continue;
      }
      const marked = markedText(parts);
      const plain = parts.map((part) => part.text).join(" ");
      // 長すぎる段落は、目印を分断しないよう最初からノードごとに訳す。
      segments.push(
        Array.from(marked).length > GROUP_LIMIT
          ? { text: plain, plain, parts, forceParts: true }
          : { text: marked, plain, parts },
      );
    }
    return segments;
  }

  /**
   * 訳文の断片を繋ぐとき、語がくっつかないよう補う空白を断片ごとに返す。
   * 戻り値は断片ごとの { before, after }。原文のその位置に空白が無いときだけ補うので、
   * 空白が二重にならない。空の断片は飛ばし、その前の空でない断片と比べる。
   * 訳先が空白なしの言語なら何も補わない。
   */
  function joinGaps(parts, fragments) {
    const NO_SPACE_BEFORE = /^[.,!?;:、。！？：；)\]」』】]/;
    const gaps = fragments.map(() => ({ before: "", after: "" }));
    if (cjkTarget) return gaps;
    // ブロック直下のテキストか。リンクなどインラインの中に空白を入れると、
    // 下線が空白まで伸びるので、ブロック直下の側に寄せる。
    const atBlock = (part) => part.node.parentElement === part.block;
    let previous = -1; // 直前の空でない断片。間に空白があれば -1
    fragments.forEach((fragment, index) => {
      const text = String(fragment ?? "").trim();
      const source = String(parts[index].source ?? "");
      const leadingSpace = /^\s/.test(source);
      const trailingSpace = /\s$/.test(source);
      if (!text) {
        if (leadingSpace || trailingSpace) previous = -1;
        return;
      }
      if (previous >= 0 && !leadingSpace && !NO_SPACE_BEFORE.test(text)) {
        // 前がブロック直下ならその末尾に、そうでなく今回がブロック直下なら頭に付ける。
        // どちらもインラインなら前の末尾に付ける。
        if (!atBlock(parts[previous]) && atBlock(parts[index])) gaps[index].before = " ";
        else gaps[previous].after = " ";
      }
      previous = trailingSpace ? -1 : index;
    });
    return gaps;
  }

  /** ノードに書く値。原文の前後の空白を残し、その位置に空白が無いときだけ gap を足す。 */
  function composeValue(part, translation, keepWhitespace = true, gap = { before: "", after: "" }) {
    const leading = keepWhitespace ? String(part.source).match(/^\s*/)?.[0] ?? "" : "";
    const trailing = keepWhitespace ? String(part.source).match(/\s*$/)?.[0] ?? "" : "";
    return `${leading || gap.before}${String(translation ?? "").trim()}${trailing || gap.after}`;
  }

  /** 組み立て済みの値を書く。原文は originals に、書いた値は written に残す。 */
  function writeValue(part, written) {
    const node = part.node;
    // ページ側が翻訳中に中身を差し替えていたら、古い原文の訳で上書きしない。
    if (!node.isConnected || node.nodeValue !== part.source) return false;
    originals.push({ node, value: part.source, written });
    translated.add(node);
    node.nodeValue = written;
    return true;
  }

  function writeOne(part, translation, keepWhitespace = true, gap = undefined) {
    return writeValue(part, composeValue(part, translation, keepWhitespace, gap));
  }

  /**
   * 断片をまとめて書き、段落として書いた文字列 (空白込み) を返す。履歴にはこれを使う。
   * 訳先が空白なしの言語なら、原文の語間の空白は持ち込まない。
   */
  function writeAll(parts, fragments) {
    const gaps = joinGaps(parts, fragments);
    const keep = !cjkTarget;
    const values = parts.map((part, index) => composeValue(part, fragments[index], keep, gaps[index]));
    // 訳が空になった断片の前後の空白が残ると、境界で空白が二重になる。
    // 前の値が空白で終わり、次の値が空白で始まるときだけ、次の頭の空白を落とす。
    // 空白が片側にしか無い境界や、どちらにも無い境界には触らない (語がくっつかないように)。
    let endsWithSpace = false;
    values.forEach((value, index) => {
      if (endsWithSpace && /^\s/.test(value)) values[index] = value.replace(/^\s+/, "");
      if (values[index]) endsWithSpace = /\s$/.test(values[index]);
    });
    parts.forEach((part, index) => writeValue(part, values[index]));
    return values.join("");
  }

  /** 履歴に残す文字列。目印は訳文の質を下げるので落とし、長さも原文と揃える。 */
  function forHistory(text) {
    return String(text ?? "").replace(MARKER, "").replace(/\s+/g, " ").trim().slice(0, 400);
  }

  async function ask(text, history) {
    const response = await chrome.runtime.sendMessage({ type: "plamo:translate", text, history });
    if (!response?.ok) throw new Error(response?.error || "翻訳に失敗しました。");
    return response.translation;
  }

  /**
   * ノードごとに訳し、全部そろってからまとめて書く。空白の補い方を断片の並びで決めるため。
   * 段落として書いた文字列 (履歴用) を返す。止められたら何も書かずに null。
   */
  async function translateParts(segment, history, isCurrent) {
    let context = history;
    const translations = [];
    for (const part of segment.parts) {
      if (!isCurrent()) return null;
      const partTranslation = await ask(part.text, context);
      if (!isCurrent()) return null;
      translations.push(partTranslation);
      context = context
        .concat([{ source: forHistory(part.text), translation: forHistory(partTranslation) }])
        .slice(-2);
    }
    if (!isCurrent()) return null;
    return writeAll(segment.parts, translations);
  }

  /**
   * 1 段落を訳して書く。段落としての訳文 (履歴用) を返す。止められたら null。
   * 長い段落 (forceParts) は段落全体を送らず、最初からノードごとに訳す。
   */
  async function translateSegment(segment, history, isCurrent) {
    if (segment.forceParts) return translateParts(segment, history, isCurrent);

    const translation = await ask(segment.text, history);
    if (!isCurrent()) return null;
    if (segment.parts.length === 1) {
      writeOne(segment.parts[0], translation);
      return translation;
    }
    const split = parseMarked(translation, segment.parts.length);
    if (!split) {
      // 目印を落とすモデルに当たったときは、ノードごとに訳し直す。
      return translateParts(segment, history, isCurrent);
    }
    // 履歴には目印付きの生の訳文ではなく、実際に書いた段落を渡す。
    return writeAll(segment.parts, split);
  }

  function restore() {
    stopRequested = true;
    generation += 1;
    let count = 0;
    for (const entry of originals) {
      // 翻訳後にページが中身を書き換えていたら、そちらを優先して触らない。
      if (entry.node.isConnected && entry.node.nodeValue === entry.written) {
        entry.node.nodeValue = entry.value;
        count += 1;
      }
    }
    originals = [];
    translated = new WeakSet();
    // 進行中の run は generation の不一致で書き込みも報告も止まるので、すぐ次を始めてよい。
    running = false;
    return count;
  }

  async function run(reportStatus) {
    if (running) {
      reportStatus({ phase: "busy" });
      return;
    }
    running = true;
    stopRequested = false;
    const runGeneration = generation;
    // 復元されたあと (generation が進んだあと) の古い run は、ポップアップの表示を上書きしない。
    const report = (status) => {
      if (runGeneration === generation) reportStatus(status);
    };

    try {
      const segments = collect();
      if (segments.length === 0) {
        report({ phase: "empty" });
        return;
      }
      report({ phase: "start", total: segments.length });

      // 履歴は文書順で直前 2 文。段落をまたいでも、同じページの言い回しを引き継ぐ。
      // 順番を守るため、訳は 1 文ずつ流す。llama.cpp 側で 1 文は 0.1〜0.5 秒なので、
      // 並列にしても体感は変わらないうえ、履歴と語順が崩れない。
      const historyByIndex = new Map();
      let done = 0;
      let failed = 0;

      function historyFor(index) {
        const pairs = [];
        for (const previous of [index - 2, index - 1]) {
          if (previous >= 0 && historyByIndex.has(previous)) pairs.push(historyByIndex.get(previous));
        }
        return pairs.slice(-2);
      }

      // 停止・復元のどちらでも止まる。復元のあとに新しい run が stopRequested を
      // 下ろしても、generation が違うので古い run は書き込まない。
      const isCurrent = () => runGeneration === generation && !stopRequested;
      for (let index = 0; index < segments.length; index += 1) {
        if (!isCurrent()) break;
        const segment = segments[index];
        const history = historyFor(index);
        try {
          const translation = await translateSegment(segment, history, isCurrent);
          if (translation && isCurrent()) {
            historyByIndex.set(index, {
              source: forHistory(segment.plain),
              translation: forHistory(translation),
            });
          }
        } catch (error) {
          failed += 1;
          report({ phase: "item-error", error: error instanceof Error ? error.message : String(error) });
        }
        done += 1;
        report({ phase: "progress", done, total: segments.length, failed });
      }

      report({ phase: stopRequested ? "stopped" : "done", done, total: segments.length, failed });
    } catch (error) {
      report({ phase: "error", error: error instanceof Error ? error.message : String(error) });
    } finally {
      // 復元で running は解放済み。そのあと始まった新しい run の running は消さない。
      if (runGeneration === generation) running = false;
    }
  }

  refreshTargetLanguage();

  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || !message.type) return false;

    if (message.type === "plamo:page-translate") {
      run((status) => {
        chrome.runtime.sendMessage({ type: "plamo:status", status }).catch(() => {});
      })
        .then(() => sendResponse({ ok: true }))
        .catch((error) => sendResponse({ ok: false, error: String(error) }));
      return true;
    }

    if (message.type === "plamo:page-stop") {
      stopRequested = true;
      sendResponse({ ok: true });
      return false;
    }

    if (message.type === "plamo:page-restore") {
      sendResponse({ ok: true, count: restore() });
      return false;
    }

    if (message.type === "plamo:page-count") {
      sendResponse({ ok: true, count: collect().length, running });
      return false;
    }

    return false;
  });
})();
