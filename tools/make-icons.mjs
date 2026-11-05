/**
 * 拡張機能アイコン。朱の地に「訳」の一字。画像生成は使わない。
 * 字形は 16x16 の点で持ち、大きいサイズへ最近傍で拡げる。
 */
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "extension", "icons");
mkdirSync(root, { recursive: true });

const GLYPH = [
  "..111111111111..",
  ".1............1.",
  "1.111111111111.1",
  "1.1..........1.1",
  "1.1.11111111.1.1",
  "1.1.1......1.1.1",
  "1.1.1.1111.1.1.1",
  "1.1.1.1..1.1.1.1",
  "1.1.1.1111.1.1.1",
  "1.1.1......1.1.1",
  "1.1.11111111.1.1",
  "1.1..........1.1",
  "1.111111111111.1",
  ".1............1.",
  "..111111111111..",
  "................",
];

function png(size, rgba) {
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y += 1) {
    raw[y * (size * 4 + 1)] = 0;
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([sig, chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

function crc(buf) {
  let c = 0xffffffff;
  for (const byte of buf) {
    c ^= byte;
    for (let i = 0; i < 8; i += 1) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return (c ^ 0xffffffff) >>> 0;
}

function render(size) {
  const paper = [246, 241, 231, 255];
  const seal = [156, 43, 43, 255];
  const rgba = Buffer.alloc(size * size * 4);
  const cell = size / 16;
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      const on = GLYPH[Math.min(15, Math.floor(y / cell))][Math.min(15, Math.floor(x / cell))] === "1";
      const color = on ? seal : paper;
      const offset = (y * size + x) * 4;
      color.forEach((channel, index) => {
        rgba[offset + index] = channel;
      });
    }
  }
  return png(size, rgba);
}

for (const size of [16, 32, 48, 128]) {
  writeFileSync(join(root, `icon-${size}.png`), render(size));
}
console.log(`wrote icons to ${root}`);
