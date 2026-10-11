import "./download-ui.mjs";
const root = document.documentElement;
const motion = root.classList.contains("motion");
const TICK_MS = 250;

const clamp = (value, low = 0, high = 1) => Math.min(high, Math.max(low, value));
const span = (value, from, to) => clamp((value - from) / (to - from));
const ease = (t) => t * t * (3 - 2 * t);

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

// Everything that animates here is written straight onto the element that moves, as a
// transform or an opacity, and only when the value changed. That keeps scrolling off the
// layout path: no inherited custom properties, no animated widths.
const written = new WeakMap();
function setStyle(node, property, value) {
  let seen = written.get(node);
  if (!seen) written.set(node, (seen = {}));
  if (seen[property] === value) return;
  seen[property] = value;
  node.style[property] = value;
}
function setText(node, text) {
  if (node.textContent !== text) node.textContent = text;
}
function setClass(node, name, on) {
  if (node.classList.contains(name) !== on) node.classList.toggle(name, on);
}

/* Theme and accent ------------------------------------------------------- */

const accentName = document.querySelector("[data-accent-name]");

function remember(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage can be blocked; the choice still applies to this visit.
  }
}

function syncAppearance() {
  const theme = root.dataset.theme === "light" ? "light" : "dark";
  const accent = root.dataset.accent || "ember";
  for (const button of document.querySelectorAll("[data-set-theme]")) {
    button.setAttribute("aria-pressed", String(button.dataset.setTheme === theme));
  }
  for (const button of document.querySelectorAll("[data-set-accent]")) {
    button.setAttribute("aria-pressed", String(button.dataset.setAccent === accent));
  }
  const name = accent[0].toUpperCase() + accent.slice(1);
  if (accentName && accentName.textContent !== name) {
    accentName.textContent = name;
    accentName.classList.remove("swap");
    void accentName.offsetWidth;
    accentName.classList.add("swap");
  }
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", theme === "light" ? "#f4f5f8" : "#0c0e13");
}

document.addEventListener("click", (event) => {
  const button = event.target.closest("[data-set-theme], [data-set-accent]");
  if (!button) return;
  if (button.dataset.setTheme) {
    if (button.dataset.setTheme === "light") root.dataset.theme = "light";
    else delete root.dataset.theme;
    remember("fetchrail-theme", button.dataset.setTheme);
  } else {
    if (button.dataset.setAccent === "ember") delete root.dataset.accent;
    else root.dataset.accent = button.dataset.setAccent;
    remember("fetchrail-accent", button.dataset.setAccent);
  }
  syncAppearance();
});
syncAppearance();

/* Scroll progress -------------------------------------------------------- */

// Each tracked element reports its progress through a stretch of scrolling, eased a
// little behind the real scroll position so scrubbed scenes glide instead of ticking.
const ranges = {
  // A tall wrapper around a sticky stage: 0 as it pins, 1 as it lets go.
  pin: (rect, height) => -rect.top / Math.max(1, rect.height - height),
  // 0 as the top edge comes in at the bottom, 1 once it has climbed most of the screen.
  enter: (rect, height) => (height - rect.top) / (height * 0.7),
  // 0 as the block reaches the lower part of the screen, 1 by the time it is read past.
  read: (rect, height) => (height * 0.86 - rect.top) / (height * 0.5 + rect.height),
};
const trackers = [];
const pageProgress = document.querySelector(".page-progress");
let framePending = false;

function track(node, range, onProgress) {
  trackers.push({ node, range, onProgress, shown: -1 });
}

function updateTrackers() {
  framePending = false;
  const height = innerHeight;
  // Read every position first, then write, so one frame never lays out twice.
  const targets = trackers.map((tracker) => clamp(ranges[tracker.range](tracker.node.getBoundingClientRect(), height)));
  const scrollable = root.scrollHeight - height;
  const page = scrollable > 0 ? scrollY / scrollable : 0;
  let settling = false;
  trackers.forEach((tracker, index) => {
    const target = targets[index];
    const eased = tracker.shown < 0 ? target : tracker.shown + (target - tracker.shown) * 0.18;
    const value = Math.abs(target - eased) < 0.0005 ? target : eased;
    if (value !== target) settling = true;
    if (value === tracker.shown) return;
    tracker.shown = value;
    tracker.onProgress(value);
  });
  if (pageProgress) setStyle(pageProgress, "transform", `scaleX(${page.toFixed(4)})`);
  if (settling) requestFrame();
}

function requestFrame() {
  if (framePending) return;
  framePending = true;
  requestAnimationFrame(updateTrackers);
}

addEventListener("scroll", requestFrame, { passive: true });
addEventListener("resize", requestFrame);

/* Header and reveals ----------------------------------------------------- */

const head = document.querySelector("[data-site-head]");
const sentinel = document.querySelector("[data-head-sentinel]");
if (head && sentinel) {
  new IntersectionObserver(([entry]) => head.classList.toggle("scrolled", !entry.isIntersecting)).observe(sentinel);
}

const revealer = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      entry.target.classList.add("in");
      revealer.unobserve(entry.target);
    }
  },
  { rootMargin: "0px 0px -12% 0px" },
);
for (const node of document.querySelectorAll("[data-reveal]")) revealer.observe(node);

/* Strands ---------------------------------------------------------------- */

// A fill is as wide as its track and slides in from the left, so growing it never
// needs a new layout.
function paintFill(fill, fraction, state = "receiving") {
  setStyle(fill, "transform", `translateX(${(clamp(fraction) * 100 - 100).toFixed(1)}%)`);
  const name = `fill ${state === "receiving" && fraction >= 1 ? "done" : state}`;
  if (fill.className !== name) fill.className = name;
}

function buildStrands(host, count) {
  const fills = Array.from({ length: count }, () => element("i", "fill"));
  host.replaceChildren(
    ...fills.map((fill) => {
      const strand = element("span");
      strand.append(fill);
      return strand;
    }),
  );
  return fills;
}

for (const host of document.querySelectorAll("[data-fills]")) {
  const fractions = host.dataset.fills.split(",").map(Number);
  const fills = buildStrands(host, fractions.length);
  fractions.forEach((fraction, index) => {
    let state = host.dataset.state || "receiving";
    if (String(index) === host.dataset.retrying) state = "connecting";
    if (String(index) === host.dataset.failed) state = "failed";
    paintFill(fills[index], fraction, state);
  });
}

// Runs `step` while its host is on screen and the tab is in front.
function whileVisible(host, step, interval = TICK_MS) {
  let visible = false;
  new IntersectionObserver(([entry]) => (visible = entry.isIntersecting)).observe(host);
  setInterval(() => {
    if (visible && !document.hidden) step(interval / 1000);
  }, interval);
}

/* Hero ------------------------------------------------------------------- */

// 32 connections fill at their own pace, then join into one bar.
const rails = document.querySelector("[data-rails]");
if (rails) {
  const count = 32;
  let fractions = Array.from({ length: count }, (_, i) => Math.min(1, 0.34 + 0.72 * Math.abs(Math.sin(i * 1.9 + 0.6))));
  const rates = fractions.map((_, i) => 0.05 + 0.07 * Math.abs(Math.sin(i * 2.7 + 1.3)));
  const fills = buildStrands(rails, count);
  const paint = () => fractions.forEach((fraction, i) => paintFill(fills[i], fraction));
  paint();

  let hold = 0;
  if (motion) {
    whileVisible(rails, (seconds) => {
      if (hold > 0) {
        hold -= 1;
        if (hold === 2) rails.classList.add("fading");
        if (hold === 0) {
          fractions = fractions.map(() => 0);
          rails.classList.add("resetting");
          rails.classList.remove("merged");
          paint();
          void rails.offsetWidth;
          rails.classList.remove("resetting", "fading");
        }
        return;
      }
      fractions = fractions.map((fraction, i) => Math.min(1, fraction + rates[i] * seconds));
      paint();
      if (fractions.every((fraction) => fraction >= 1)) {
        rails.classList.add("merged");
        hold = 8;
      }
    });
  }
}

// The app window leans back at first and stands up as it is scrolled into view.
const heroStage = document.querySelector("[data-hero-stage]");
if (heroStage && motion) {
  const windowNode = heroStage.querySelector(".shot");
  const glow = heroStage.querySelector(".hero-glow");
  track(heroStage, "enter", (progress) => {
    setStyle(windowNode, "transform", `rotateX(${((1 - progress) * 20).toFixed(2)}deg) scale(${(0.88 + 0.12 * progress).toFixed(4)})`);
    setStyle(glow, "opacity", (0.35 + 0.65 * progress).toFixed(2));
  });
}

/* App window: the two running downloads keep moving like they do in the app. */

const formatSize = (megabytes, digits = 1) =>
  megabytes <= 0 ? "0" : megabytes < 1000 ? `${Math.round(megabytes)} MB` : `${(megabytes / 1000).toFixed(digits)} GB`;
const formatSpeed = (megabytesPerSecond) => `${megabytesPerSecond.toFixed(1)} MB/s`;
const formatEta = (seconds) => {
  const whole = Math.max(1, Math.ceil(seconds));
  return whole < 60 ? `${whole}s` : `${Math.floor(whole / 60)}m ${String(whole % 60).padStart(2, "0")}s`;
};

function createDownload(row, { totalMb, start, partSpeed }) {
  const find = (name) => row.querySelector(`[data-sim="${name}"]`);
  const count = start.length;
  const partMb = totalMb / count;
  const strands = buildStrands(find("strands"), count);
  const map = find("map") && buildStrands(find("map"), count);
  const lanes =
    find("lanes") &&
    start.map((_, index) => {
      const lane = element("div", "lane");
      const bar = element("span", "strands thin");
      const [fill] = buildStrands(bar, 1);
      const percent = element("span", "mono percent");
      const note = element("span", "note");
      lane.append(
        element("span", "mono", String(index + 1).padStart(2, "0")),
        element("span", "mono range", `${formatSize(index * partMb, 2)} – ${formatSize((index + 1) * partMb, 2)}`),
        bar,
        percent,
        note,
      );
      find("lanes").append(lane);
      return { fill, percent, note };
    });
  const out = Object.fromEntries(
    ["caption", "percent", "speed", "connections", "eta", "eta-label", "summary"].map((name) => [name, find(name)]),
  );

  let fractions = start.slice();
  let speeds = start.map(() => partSpeed);

  const download = {
    speed: 0,
    active: 0,
    done: false,
    reset() {
      fractions = start.slice();
      download.done = false;
      find("strands").classList.remove("merged");
      find("map")?.classList.remove("merged");
      download.step(0);
    },
    step(seconds) {
      // Each connection wanders around its own pace, the way real links do.
      speeds = speeds.map(() => partSpeed * (0.85 + 0.3 * Math.random()));
      fractions = fractions.map((fraction, i) => Math.min(1, fraction + (speeds[i] * seconds) / partMb));
      const receiving = fractions.filter((fraction) => fraction < 1).length;
      const downloaded = fractions.reduce((sum, fraction) => sum + fraction * partMb, 0);
      download.active = receiving;
      download.speed = fractions.reduce((sum, fraction, i) => sum + (fraction < 1 ? speeds[i] : 0), 0);
      download.done = receiving === 0;

      fractions.forEach((fraction, i) => {
        paintFill(strands[i], fraction);
        if (map) paintFill(map[i], fraction);
        if (lanes) {
          paintFill(lanes[i].fill, fraction);
          setText(lanes[i].percent, `${Math.floor(fraction * 100)}%`);
          setText(lanes[i].note, fraction < 1 ? formatSpeed(speeds[i]) : "Done");
          setClass(lanes[i].note, "receiving", fraction < 1);
        }
      });
      setText(out.caption, download.done ? "Joining parts" : `${formatSize(downloaded)} of ${formatSize(totalMb)}`);
      setText(out.percent, `${Math.floor((downloaded / totalMb) * 100)}%`);
      setText(out.speed, download.done ? "—" : formatSpeed(download.speed));
      setText(out.connections, download.done ? "Disk write" : `${receiving} of ${count} connections`);
      setText(out.eta, download.done ? "—" : formatEta((totalMb - downloaded) / download.speed));
      setText(out["eta-label"], download.done ? "" : "left");
      if (out.summary) setText(out.summary, `${count} connections · ${receiving} receiving · ${count - receiving} done`);
      if (download.done) {
        find("strands").classList.add("merged");
        find("map")?.classList.add("merged");
      }
    },
  };
  download.step(0);
  return download;
}

const shot = document.querySelector(".shot");
if (shot) {
  const downloads = [
    createDownload(shot.querySelector('[data-download-row="iso"]'), {
      totalMb: 4200,
      start: [1, 1, 0.7, 1, 0.64, 0.47, 0.6, 1, 0.55, 0.38, 0.5, 0.44, 0.45, 0.31, 0.49, 0.39],
      partSpeed: 6.4,
    }),
    createDownload(shot.querySelector('[data-download-row="zip"]'), {
      totalMb: 1100,
      start: [0.4, 0.31, 0.22, 0.35, 0.26, 0.19, 0.3, 0.21],
      partSpeed: 0.525,
    }),
  ];
  const find = (name) => shot.querySelector(`[data-sim="${name}"]`);
  const totalSpeed = find("total-speed");
  const totalConnections = find("total-connections");
  const sparkLine = find("spark-line");
  const sparkArea = find("spark-area");
  const samples = [58, 63, 61, 70, 68, 76, 72, 80, 77, 82, 78, 84, 81, 85, 83];

  const paintSidebar = (redrawGraph) => {
    setText(totalSpeed, downloads.reduce((sum, download) => sum + download.speed, 0).toFixed(1));
    setText(totalConnections, `${downloads.reduce((sum, download) => sum + download.active, 0)} connections`);
    if (!redrawGraph) return;
    const peak = Math.max(...samples, 1) * 1.08;
    const points = samples.map(
      (value, index) => `${((index * 200) / (samples.length - 1)).toFixed(1)},${(42 - (value / peak) * 38).toFixed(1)}`,
    );
    sparkLine.setAttribute("points", points.join(" "));
    sparkArea.setAttribute("d", `M0,44 L${points.join(" L")} L200,44 Z`);
  };
  paintSidebar(true);

  if (motion) {
    let ticks = 0;
    let hold = 0;
    whileVisible(shot, (seconds) => {
      ticks += 1;
      if (hold > 0) {
        hold -= 1;
        if (hold === 0) downloads.forEach((download) => download.reset());
      } else {
        downloads.forEach((download) => download.done || download.step(seconds));
        // Stay on the joined bar for a moment before the loop starts over.
        if (downloads[0].done) hold = 12;
      }
      const second = ticks % 4 === 0;
      if (second) {
        samples.shift();
        samples.push(downloads.reduce((sum, download) => sum + download.speed, 0));
      }
      paintSidebar(second);
    });
  }
}

/* Engine: one bar, 32 rails, one bar again. ------------------------------- */

const engine = document.querySelector("[data-engine]");
if (engine) {
  const COUNT = 32;
  const STALLED = 19;
  const LANE = 6;
  const BAR = 20;
  const host = engine.querySelector("[data-engine-rails]");
  const pulse = host.querySelector(".engine-pulse");
  const fileLabel = engine.querySelector(".engine-file");
  const doneLabel = engine.querySelector(".engine-done");
  const steps = [...engine.querySelectorAll(".engine-steps li")];
  const left = engine.querySelector('[data-engine-out="left"]');
  const right = engine.querySelector('[data-engine-out="right"]');
  const boundaries = [0, 0.18, 0.42, 0.8, 1];
  // Every rail is laid out once, as a full-width lane on the centre line. From then on
  // only its transform changes: squeezed into its slice of the bar, or slid out to its lane.
  const railNodes = Array.from({ length: COUNT }, () => {
    const rail = element("div", "erail");
    rail.append(element("i"));
    host.append(rail);
    return rail;
  });
  const fillNodes = railNodes.map((rail) => rail.firstChild);
  // Every connection has its own pace; all of them are home by the end of the fetch.
  const paces = railNodes.map((_, index) => 1 + 0.9 * Math.abs(Math.sin(index * 2.3 + 0.7)));
  const filled = (index, fetch) => {
    if (index !== STALLED) return Math.min(1, fetch * paces[index]);
    // One connection drops, waits, then carries on from the offset it had reached.
    const reached = 0.3 * paces[index];
    if (fetch < 0.3) return fetch * paces[index];
    return fetch < 0.55 ? reached : reached + (1 - reached) * span(fetch, 0.55, 0.97);
  };

  const stepShown = [];
  let width = 1;
  let pitch = 1;
  let shown = 0.62;

  const paintEngine = (progress) => {
    shown = progress;
    const probe = span(progress, 0.02, 0.16);
    const splitting = span(progress, 0.18, 0.42);
    const fetch = span(progress, 0.42, 0.8);
    const joining = span(progress, 0.8, 1);
    const cut = ease(span(progress, 0.18, 0.25)) * (1 - ease(span(progress, 0.93, 0.985)));
    const fan = ease(span(progress, 0.24, 0.42)) * (1 - ease(span(progress, 0.82, 0.94)));
    const retrying = fetch >= 0.3 && fetch < 0.55;

    // While the bar is cut, each slice gives up a few pixels so the gaps show;
    // once it is whole again the slices overlap by one so no seam is left.
    const gap = cut * (1 - fan) * 3 - (1 - cut);
    const scaleX = (1 / COUNT + fan * (1 - 1 / COUNT) - gap / width).toFixed(5);
    const scaleY = ((BAR - fan * (BAR - LANE)) / LANE).toFixed(3);
    const nudge = (cut * (1 - fan) * 1.5).toFixed(1);
    let received = 0;
    railNodes.forEach((rail, index) => {
      const x = (((1 - fan) * index * 100) / COUNT).toFixed(3);
      const y = (fan * (index - (COUNT - 1) / 2) * pitch).toFixed(1);
      setStyle(rail, "transform", `translate3d(calc(${x}% + ${nudge}px), ${y}px, 0) scale(${scaleX}, ${scaleY})`);
      const fraction = filled(index, fetch);
      received += fraction;
      setStyle(fillNodes[index], "transform", `translateX(${(fraction * 100 - 100).toFixed(2)}%)`);
      setClass(rail, "live", fraction > 0 && fraction < 1);
      setClass(rail, "retry", index === STALLED && retrying);
    });
    setClass(host, "joined", fan === 0 && cut === 0);
    setStyle(pulse, "transform", `translateX(${(probe * 733).toFixed(1)}%)`);
    setStyle(pulse, "opacity", progress < 0.18 ? Math.sin(Math.PI * probe).toFixed(2) : "0");
    setStyle(fileLabel, "opacity", clamp(1 - fan * 4).toFixed(2));
    setStyle(doneLabel, "opacity", span(progress, 0.95, 1).toFixed(2));

    const phase = progress < boundaries[1] ? 0 : progress < boundaries[2] ? 1 : progress < boundaries[3] ? 2 : 3;
    steps.forEach((step, index) => {
      setClass(step, "active", index === phase);
      const stepProgress = span(progress, boundaries[index], boundaries[index + 1]).toFixed(2);
      if (stepShown[index] !== stepProgress) {
        stepShown[index] = stepProgress;
        step.style.setProperty("--sp", stepProgress);
      }
    });
    if (phase === 0) {
      setText(left, "1 request");
      setText(right, probe < 0.6 ? "Asking the server" : "4.2 GB · byte ranges accepted");
    } else if (phase === 1) {
      const connections = Math.max(1, Math.round(1 + (COUNT - 1) * ease(splitting)));
      setText(left, `${connections} ${connections === 1 ? "connection" : "connections"}`);
      setText(right, "Segments never overlap");
    } else if (phase === 2) {
      setText(left, `${COUNT} connections`);
      setText(right, retrying ? `Connection ${STALLED + 1} is retrying` : `${Math.floor((received / COUNT) * 100)}% received`);
    } else {
      setText(left, joining < 0.6 ? `${COUNT} parts` : "1 file");
      setText(right, "Every byte present");
    }
  };

  // The lane spacing depends on the stage size, so repaint whenever that changes.
  new ResizeObserver(() => {
    width = Math.max(1, host.clientWidth);
    pitch = host.clientHeight / COUNT;
    paintEngine(shown);
  }).observe(host);
  if (motion) track(engine, "pin", paintEngine);
}

/* Resume card: the resumed bars pick up exactly where the paused ones stopped. */

const resumeCard = document.querySelector("[data-resume-card]");
if (resumeCard) {
  const strands = resumeCard.querySelector("[data-resume-strands]");
  const fills = [...strands.querySelectorAll(".fill")];
  const from = strands.dataset.fills.split(",").map(Number);
  const resume = () => fills.forEach((fill, index) => paintFill(fill, Math.min(1, from[index] + 0.22)));
  if (motion) {
    strands.classList.add("slow");
    fills.forEach((fill, index) => paintFill(fill, from[index], "stopped"));
    new IntersectionObserver(
      ([entry], observer) => {
        if (!entry.isIntersecting) return;
        observer.disconnect();
        setTimeout(resume, 900);
      },
      { threshold: 0.6 },
    ).observe(resumeCard);
  } else {
    resume();
  }
}

/* Controls gallery: vertical scrolling carries the cards sideways. -------- */

const gallery = document.querySelector("[data-gallery]");
if (gallery) {
  // The little demos only run while the section is on screen.
  new IntersectionObserver(([entry]) => gallery.classList.toggle("in", entry.isIntersecting)).observe(gallery);
  if (motion) {
    const row = gallery.querySelector("[data-gallery-track]");
    const pin = gallery.querySelector(".gallery-pin");
    const bar = gallery.querySelector(".gallery-progress i");
    const wide = matchMedia("(min-width: 760px)");
    let travel = 0;
    let shown = 0;
    const paint = () => {
      setStyle(row, "transform", wide.matches ? `translate3d(${(-shown * travel).toFixed(1)}px, 0, 0)` : "");
      setStyle(bar, "transform", `scaleX(${shown.toFixed(4)})`);
    };
    const measure = () => {
      travel = wide.matches ? Math.max(0, row.offsetWidth - pin.clientWidth) : 0;
      gallery.style.setProperty("--travel", `${travel}px`);
      paint();
      requestFrame();
    };
    measure();
    addEventListener("resize", measure);
    document.fonts?.ready.then(measure);
    track(gallery, "pin", (progress) => {
      shown = progress;
      paint();
    });
  }
}

/* Companion handoff: the picture changes as each step is read. ------------ */

const handoff = document.querySelector("[data-handoff]");
if (handoff) {
  const visual = handoff.querySelector("[data-handoff-visual]");
  const steps = [...handoff.querySelectorAll(".handoff-steps li")];
  const pinned = matchMedia("(min-width: 900px)");
  const show = (index) => {
    visual.dataset.state = index;
    steps.forEach((step, at) => step.classList.toggle("active", at === index));
  };
  show(motion ? 0 : 2);
  // Its looping animations only run while it is on screen.
  new IntersectionObserver(([entry]) => visual.classList.toggle("in", entry.isIntersecting)).observe(visual);

  if (motion) {
    // Side by side, the step crossing the middle of the screen decides the picture.
    const watcher = new IntersectionObserver(
      (entries) => {
        if (!pinned.matches) return;
        for (const entry of entries) if (entry.isIntersecting) show(steps.indexOf(entry.target));
      },
      { rootMargin: "-50% 0px -50% 0px" },
    );
    steps.forEach((step) => watcher.observe(step));
    // Stacked on a narrow screen, it plays through the three states by itself.
    let state = 0;
    whileVisible(
      visual,
      () => {
        if (pinned.matches) return;
        state = (state + 1) % steps.length;
        show(state);
      },
      2600,
    );
  }
}

/* Final section: 32 lines meet above the last download button. ------------ */

const finalSection = document.querySelector("[data-final]");
const converge = document.querySelector("[data-converge]");
if (finalSection && converge) {
  const draw = () => {
    const { width, height } = converge.getBoundingClientRect();
    if (!width || !height) return;
    converge.setAttribute("viewBox", `0 0 ${width} ${height}`);
    const middle = width / 2;
    converge.replaceChildren(
      ...Array.from({ length: 32 }, (_, index) => {
        const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
        const from = ((index + 0.5) / 32) * width;
        const to = middle + (index - 15.5) * 1.7;
        path.setAttribute("d", `M${from.toFixed(1)} 0 C${from.toFixed(1)} ${(height * 0.55).toFixed(1)}, ${to.toFixed(1)} ${(height * 0.45).toFixed(1)}, ${to.toFixed(1)} ${height}`);
        path.setAttribute("pathLength", "1");
        path.setAttribute("opacity", (0.35 + 0.65 * Math.abs(Math.sin(index * 1.9 + 0.6))).toFixed(2));
        return path;
      }),
    );
  };
  draw();
  addEventListener("resize", draw);
  // The lines draw themselves through --p, which only the 32 paths inherit.
  if (motion) track(finalSection, "enter", (progress) => converge.style.setProperty("--p", progress.toFixed(3)));
}

requestFrame();
