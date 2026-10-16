import assert from "node:assert/strict";
import { PRESETS, accentColors, accentHex, accentSpot, distance, spotOf } from "../src/accent.js";

// WCAG contrast, written out again here so the module's own math is not grading itself.
const linear = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
const luminance = (hex) => linear(hex).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0);
const contrast = (a, b) => { const [high, low] = [luminance(a), luminance(b)].sort((p, q) => q - p); return (high + 0.05) / (low + 0.05); };
const backdrop = { dark: "#10131a", light: "#f4f5f8" };

let checked = 0;
for (const theme of ["dark", "light"]) {
  for (let x = 0; x <= 1; x += 0.02) {
    for (let v = 0; v <= 1.0001; v += 0.1) {
      const colors = accentColors({ x, v }, theme);
      const where = `${theme} x=${x.toFixed(2)} v=${v.toFixed(1)}`;
      for (const name of ["--accent", "--accent-ink", "--accent-text", "--tip"]) assert.match(colors[name], /^#[0-9a-f]{6}$/, where);
      assert.match(colors["--accent-soft"], /^rgba\(\d+, \d+, \d+, 0\.14?\)$/, where);
      assert.ok(contrast(colors["--accent-text"], backdrop[theme]) >= 4.5, `accent text is readable: ${where}`);
      assert.ok(contrast(colors["--accent-ink"], colors["--accent"]) >= 4.5, `button text is readable: ${where}`);
      const saved = accentHex({ x, v }), back = accentSpot(saved);
      assert.ok(distance(accentHex(back), saved) < 0.005, `a saved accent shows the same color again: ${where}`);
      // Near grey, eight bits per channel cannot hold the hue, and it no longer shows.
      if (v >= 0.3) {
        const dx = Math.abs(back.x - x);
        assert.ok(Math.min(dx, 1 - dx) < 0.01 && Math.abs(back.v - v) < 0.02, `the thumb returns to its spot: ${where}`);
      }
      checked++;
    }
  }
}

for (const [name, hex] of Object.entries(PRESETS)) assert.deepEqual(accentSpot(name), accentSpot(hex), `${name} from older settings`);
assert.deepEqual(accentSpot("not a color"), accentSpot("ember"), "anything else falls back to Ember");
assert.deepEqual(spotOf("aabbcc", "dark"), spotOf("#AABBCC", "dark"), "the # and case are optional");
assert.equal(spotOf("#ff8", "dark"), null, "a half-typed color moves nothing");
assert.equal(spotOf("#7f7f7f", "dark", 0.3).x, 0.3, "a grey keeps the hue it was on");

console.log(`PASS: ${checked} accents read at 4.5:1 in both themes and survive saving; presets and bad input resolve`);
