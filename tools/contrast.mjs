function channel(hex) {
  const n = parseInt(hex, 16) / 255;
  return n <= 0.04045 ? n / 12.92 : ((n + 0.055) / 1.055) ** 2.4;
}
function luminance(hex) {
  return 0.2126 * channel(hex.slice(1, 3)) + 0.7152 * channel(hex.slice(3, 5)) + 0.0722 * channel(hex.slice(5, 7));
}
function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return ((hi + 0.05) / (lo + 0.05)).toFixed(2);
}
const pairs = [
  ["light ink on paper", "#1c1915", "#f6f1e7"],
  ["light quiet on paper", "#6f675c", "#f6f1e7"],
  ["light seal-ink on seal", "#fff8f2", "#9c2b2b"],
  ["light accent text on paper", "#9c2b2b", "#f6f1e7"],
  ["dark ink on paper", "#f3ecdf", "#1b1916"],
  ["dark quiet on paper", "#b7ad9f", "#1b1916"],
  ["dark seal-ink on seal", "#fff8f2", "#a8332b"],
  ["dark accent text on paper", "#ffb3a7", "#1b1916"],
  ["dark input text on input", "#f3ecdf", "#241f1b"],
];
for (const [name, a, b] of pairs) console.log(`${name}: ${contrast(a, b)}:1`);
