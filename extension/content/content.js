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
    '[translate="no"]', ".notranslate", '[contenteditable="true"]',
  ].join(",");

  const BLOCK_TAGS = /^(P|H[1-6]|LI|DT|DD|BLOCKQUOTE|FIGCAPTION|TD|TH|CAPTION|SUMMARY|LABEL|BUTTON|ADDRESS)$/;

  // 段落の中の目印。モデルが落とすことがあるので、崩れたらノードごとの訳に戻す。
  const OPEN = "\u27E6";
  const CLOSE = "\u27E7";
  const MARKER = /\u27E6(\/?)(\d+)\u27E7/g;

  /** @type {WeakSet<Text>} */
  let translated = new WeakSet();
  /** @type {{ node: Text, value: string }[]} */
  let originals = [];
  let generation = 0;
  let running = false;
  let stopRequested = false;
  // 日本語・中国語・韓国語は語間の空白を書かない。段落をまとめて訳したときは、
  // 原文の空白をそのまま戻すとリンクの前後に余計な空白が残る。
  let cjkTarget = true;

  function refreshTargetLanguage() {
    try {
      chrome.storage?.local?.get({ targetLang: "ja" }, (stored) => {
        cjkTarget = ["ja", "zh-hans", "zh-hant", "ko"].includes(stored?.targetLang ?? "ja");
      });
      chrome.storage?.onChanged?.addListener((changes, area) => {
        if (area === "local" && changes.targetLang) {
          cjkTarget = ["ja", "zh-hans", "zh-hant", "ko"].includes(changes.targetLang.newValue ?? "ja");
        }
      });
    } catch {
      // storage が無い環境では既定のまま。
    }
  }

  function normalize(text) {
    return String(text ?? "").replace(/[ \t\u00a0]+/g, " ").replace(/\s+/g, " ").trim();
  }

  function isVisible(element) {
    if (!element || !element.isConnected) return false;
    let current = element;
    while (current && current !== document.documentElement) {
      const style = getComputedStyle(current);
      if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse") {
        return false;
      }
      if (current.hasAttribute("hidden") || current.getAttribute("aria-hidden") === "true") return false;
      if (current.tagName === "DIALOG" && !current.hasAttribute("open")) return false;
      if (current.inert) return false;
      if (style.whiteSpace.startsWith("pre")) return false; // 整形済みの本文は崩さない
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
    // 段落タグが無いページでは、いちばん近い要素を単位にする。body まで広げると
    // 無関係な文が同じ履歴を共有してしまう。
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
    return parts.every(Boolean) ? parts : null;
  }

  function collect() {
    if (!document.body) return [];
    const byBlock = new Map();
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      const parent = node.parentElement;
      const text = normalize(node.nodeValue);
      if (parent && text && !translated.has(node) && !parent.closest(SKIP_SELECTOR)) {
        const block = blockOf(node);
        if (isVisible(block)) {
          if (!byBlock.has(block)) byBlock.set(block, []);
          byBlock.get(block).push({
            node,
            text,
            source: node.nodeValue,
            // ノードの境目に空白があったかを覚えておく。英語の "The " + "dashboard" を
            // 繋げたままモデルへ渡すと、語がくっついて訳される。
            spaced: /^\s/.test(node.nodeValue ?? ""),
          });
        }
      }
      node = walker.nextNode();
    }

    const segments = [];
    for (const parts of byBlock.values()) {
      const blockKey = String(segments.length);
      if (parts.length === 1 && parts[0].text.length < 2) continue;
      segments.push(
        parts.length === 1
          ? { text: parts[0].text, plain: parts[0].text, parts, blockKey }
          : { text: markedText(parts), plain: parts.map((part) => part.text).join(" "), parts, blockKey },
      );
    }
    return segments;
  }

  function writeOne(part, translation, keepWhitespace = true) {
    const node = part.node;
    // ページ側が翻訳中に中身を差し替えていたら、古い原文の訳で上書きしない。
    if (!node.isConnected || node.nodeValue !== part.source) return false;
    const leading = keepWhitespace ? String(part.source).match(/^\s*/)?.[0] ?? "" : "";
    const trailing = keepWhitespace ? String(part.source).match(/\s*$/)?.[0] ?? "" : "";
    originals.push({ node, value: part.source });
    translated.add(node);
    node.nodeValue = `${leading}${translation.trim()}${trailing}`;
    return true;
  }

  /** 履歴に残す文字列。目印は訳文の質を下げるので落とし、長さも原文と揃える。 */
  function forHistory(text) {
    return String(text ?? "")
      .replace(MARKER, "")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 400);
  }

  async function applySegment(segment, translation, history, isCurrent) {
    if (segment.parts.length === 1) {
      writeOne(segment.parts[0], translation);
      return;
    }
    const split = parseMarked(translation, segment.parts.length);
    if (split) {
      segment.parts.forEach((part, index) => writeOne(part, split[index], !cjkTarget));
      return;
    }
    // 目印を落とすモデルに当たったときは、ノードごとに訳し直す。
    // 語順は崩れるが、リンクを消すよりはましで、段落が無訳のまま残らない。
    let context = history;
    for (const part of segment.parts) {
      const partTranslation = await ask(part.text, context);
      // 待っている間に「原文に戻す」や「止める」が押されていたら、そこで止める。
      if (!isCurrent()) return;
      writeOne(part, partTranslation);
      context = context
        .concat([{ source: forHistory(part.text), translation: forHistory(partTranslation) }])
        .slice(-2);
    }
  }

  function restore() {
    stopRequested = true;
    generation += 1;
    let count = 0;
    for (const entry of originals) {
      if (entry.node.isConnected) {
        entry.node.nodeValue = entry.value;
        count += 1;
      }
    }
    originals = [];
    translated = new WeakSet();
    return count;
  }

  async function ask(text, history) {
    const response = await chrome.runtime.sendMessage({ type: "plamo:translate", text, history });
    if (!response?.ok) throw new Error(response?.error || "翻訳に失敗しました。");
    return response.translation;
  }

  async function run(report) {
    if (running) {
      report({ phase: "busy" });
      return;
    }
    running = true;
    stopRequested = false;
    const runGeneration = generation;

    try {
      const segments = collect();
      if (segments.length === 0) {
        report({ phase: "empty" });
        return;
      }
      report({ phase: "start", total: segments.length });

      const historyByBlock = new Map();
      const blockBusy = new Set();
      const pending = segments.map((_, index) => index);
      const concurrency = 2;
      let done = 0;
      let failed = 0;

      function takeNext() {
        const index = pending.findIndex((item) => !blockBusy.has(segments[item].blockKey));
        if (index < 0) return null;
        const [segmentIndex] = pending.splice(index, 1);
        blockBusy.add(segments[segmentIndex].blockKey);
        return segmentIndex;
      }

      async function worker() {
        while (!stopRequested) {
          const index = takeNext();
          if (index == null) {
            if (pending.length === 0 || blockBusy.size === 0) return;
            await new Promise((resolve) => setTimeout(resolve, 15));
            continue;
          }
          const segment = segments[index];
          const history = (historyByBlock.get(segment.blockKey) || []).slice(-2);
          const isCurrent = () => runGeneration === generation && !stopRequested;
          try {
            const translation = await ask(segment.text, history);
            if (isCurrent()) {
              await applySegment(segment, translation, history, isCurrent);
              if (isCurrent()) {
                historyByBlock.set(
                  segment.blockKey,
                  history
                    .concat({ source: forHistory(segment.plain), translation: forHistory(translation) })
                    .slice(-2),
                );
              }
            }
          } catch (error) {
            failed += 1;
            report({ phase: "item-error", error: error instanceof Error ? error.message : String(error) });
          } finally {
            blockBusy.delete(segment.blockKey);
          }
          done += 1;
          report({ phase: "progress", done, total: segments.length, failed });
        }
      }

      await Promise.all(Array.from({ length: concurrency }, () => worker()));
      report({ phase: stopRequested ? "stopped" : "done", done, total: segments.length, failed });
    } catch (error) {
      report({ phase: "error", error: error instanceof Error ? error.message : String(error) });
    } finally {
      running = false;
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
