// Accent colors derived from one picked hue and vibrance. The browser panel loads this file as a
// plain script (build.mjs strips the exports), so the app and the panel wear the same colors.

const RADIANS = Math.PI / 180;
// OKLCH hue 0 is pink; the field starts at red.
const HUE_START = 20;
// One lightness per theme keeps every hue equally bright. Light's 0.55 holds fills at 3:1
// against its background and white button text at 4.5:1.
const LIGHTNESS = { dark: 0.75, light: 0.55 };
// What accent-colored text must read on at 4.5:1.
const BACKDROP = { dark: "#10131a", light: "#f4f5f8" };

export const PRESETS = { ember: "#ff8a3d", azure: "#5aa9ff", jade: "#3fd6a0", iris: "#a594ff" };

const clamp01 = (x) => Math.min(1, Math.max(0, x));
const themeOf = (theme) => (theme === "light" ? "light" : "dark");

// OKLCH to linear sRGB, after Björn Ottosson's OKLab.
function fromOklch(L, C, h) {
  const a = C * Math.cos(h * RADIANS), b = C * Math.sin(h * RADIANS);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

function toOklab([r, g, b]) {
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

// The most vivid chroma sRGB can show at this lightness and hue.
function maxChroma(L, h) {
  let low = 0, high = 0.37;
  for (let i = 0; i < 16; i++) {
    const mid = (low + high) / 2;
    if (fromOklch(L, mid, h).every((x) => x >= -1e-4 && x <= 1.0001)) low = mid;
    else high = mid;
  }
  return low;
}

const shade = (L, h, v, k = 1) => fromOklch(L, v * k * maxChroma(L, h), h);
const encode = (x) => (x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055);
const decode = (x) => (x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
const channels = (rgb) => rgb.map((x) => Math.round(encode(clamp01(x)) * 255));
const toHex = (rgb) => "#" + channels(rgb).map((n) => n.toString(16).padStart(2, "0")).join("");
const luminance = (rgb) => rgb.map(clamp01).reduce((sum, x, i) => sum + x * [0.2126, 0.7152, 0.0722][i], 0);
const contrast = (first, second) => {
  const [high, low] = [luminance(first), luminance(second)].sort((p, q) => q - p);
  return (high + 0.05) / (low + 0.05);
};
const hueAt = (x) => (HUE_START + x * 360) % 360;

function parseHex(text) {
  // Six digits only: a half-typed "#ff8" is not yet a color.
  const match = /^#?([0-9a-f]{6})$/i.exec(String(text).trim());
  return match && [0, 2, 4].map((i) => decode(parseInt(match[1].slice(i, i + 2), 16) / 255));
}

/** Where a #rrggbb color sits on the field, hue across (x) and vibrance (v), or null. */
export function spotOf(text, theme, fallbackX = 0.08) {
  const rgb = parseHex(text);
  if (!rgb) return null;
  const [, a, b] = toOklab(rgb), C = Math.hypot(a, b);
  if (C < 0.002) return { x: fallbackX, v: 0 };
  const h = (Math.atan2(b, a) / RADIANS + 360) % 360;
  return { x: ((h - HUE_START + 360) % 360) / 360, v: clamp01(C / maxChroma(LIGHTNESS[themeOf(theme)], h)) };
}

/** The saved accent, a preset name from older versions or a #rrggbb color, as a spot. */
export const accentSpot = (accent) => spotOf(PRESETS[accent] ?? accent, "dark") ?? spotOf(PRESETS.ember, "dark");

/** What gets saved: the dark theme's accent, which maps back to the same spot. */
export const accentHex = ({ x, v }) => toHex(shade(LIGHTNESS.dark, hueAt(x), v));

/** The custom properties the app and the panel color themselves with. */
export function accentColors({ x, v }, theme) {
  const mode = themeOf(theme), h = hueAt(x), backdrop = parseHex(BACKDROP[mode]), away = mode === "dark" ? 0.01 : -0.01;
  const darkInk = shade(0.2, h, v, 0.3), white = [1, 1, 1];
  const inkOn = (fill) => (contrast(darkInk, fill) >= contrast(white, fill) ? darkInk : white);
  // Button text must read on the fill; the few hues that miss move their fill away from the ink.
  let fillL = LIGHTNESS[mode], fill = shade(fillL, h, v);
  while (contrast(inkOn(fill), fill) < 4.5 && fillL > 0.1 && fillL < 0.97) fill = shade((fillL += away), h, v);
  let textL = LIGHTNESS[mode] + away * 4, text = shade(textL, h, v);
  while (contrast(text, backdrop) < 4.5 && textL > 0.1 && textL < 0.97) text = shade((textL += away), h, v);
  return {
    "--accent": toHex(fill),
    "--accent-ink": toHex(inkOn(fill)),
    "--accent-text": toHex(text),
    "--accent-soft": `rgba(${channels(fill).join(", ")}, ${mode === "dark" ? 0.14 : 0.1})`,
    "--tip": toHex(mode === "dark" ? shade(0.9, h, v, 0.6) : shade(0.36, h, v, 0.8)),
  };
}

/** The picker's field: every hue across at full vibrance, fading to grey downward. */
export function accentField(theme) {
  const L = LIGHTNESS[themeOf(theme)], stops = [];
  for (let i = 0; i <= 24; i++) stops.push(`${toHex(shade(L, hueAt(i / 24), 1))} ${((i / 24) * 100).toFixed(1)}%`);
  const grey = toHex(fromOklch(L, 0, 0));
  return `linear-gradient(to bottom, ${grey}00, ${grey}), linear-gradient(to right, ${stops.join(", ")})`;
}

/** How different two #rrggbb colors look; past about 0.02 it shows. */
export function distance(first, second) {
  const [p, q] = [first, second].map((text) => toOklab(parseHex(text) ?? [0, 0, 0]));
  return Math.hypot(p[0] - q[0], p[1] - q[1], p[2] - q[2]);
}
