/**
 * inline SVG アイコン。絵文字は使わず、線幅は currentColor の 1.6px に揃える。
 * 静的なものは index.html に直接書き、ここは JS から差し込む分だけを持つ。
 */

/** JS から差し込むアイコンだけを持つ (静的なものは index.html に直書き)。 */
export type IconName = "check" | "plus" | "trash" | "arrowRight" | "sun" | "moon" | "monitor";

const SVG_NAMESPACE = "http://www.w3.org/2000/svg";

const PATHS: Record<IconName, string> = {
  check: '<path d="M20 6 9 17l-5-5" />',
  plus: '<path d="M12 5v14" /><path d="M5 12h14" />',
  trash:
    '<path d="M4 7h16" /><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" /><path d="M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12" /><path d="M10 11v6" /><path d="M14 11v6" />',
  arrowRight: '<path d="M4 12h16" /><path d="m14 6 6 6-6 6" />',
  sun:
    '<circle cx="12" cy="12" r="4" /><path d="M12 2v2" /><path d="M12 20v2" /><path d="m4.9 4.9 1.4 1.4" /><path d="m17.7 17.7 1.4 1.4" /><path d="M2 12h2" /><path d="M20 12h2" /><path d="m4.9 19.1 1.4-1.4" /><path d="m17.7 6.3 1.4-1.4" />',
  moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5Z" />',
  monitor: '<rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8" /><path d="M12 16v4" />',
};

export function icon(name: IconName, size = 16): SVGSVGElement {
  const svg = document.createElementNS(SVG_NAMESPACE, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.6");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  svg.classList.add("icon");
  svg.innerHTML = PATHS[name];
  return svg;
}

/** 既存の要素の中身をアイコン 1 個に置き換える。 */
export function setIcon(container: Element, name: IconName, size = 16): void {
  container.replaceChildren(icon(name, size));
}
