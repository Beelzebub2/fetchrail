import { useEffect, useState } from "react";
import { currentMonitor } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";

// Interface sizes offered in Appearance; 0 picks one from the screen.
export const INTERFACE_SCALES = [0, 0.9, 1, 1.1, 1.25, 1.5] as const;
const KEY = "interfaceScale";

// A 1080p-sized screen keeps 100%; bigger ones grow the interface by part of the difference, so a 1440p
// display gets about 120% and nothing passes 150%. Sizes are logical, after the system's own display scaling.
export function automaticScale(width: number, height: number) {
  const ratio = Math.min(width / 1920, height / 1080);
  return Math.min(1.5, Math.max(1, Math.round((1 + (ratio - 1) * 0.6) * 20) / 20));
}

function stored() {
  try {
    const value = Number(localStorage.getItem(KEY));
    return (INTERFACE_SCALES as readonly number[]).includes(value) ? value : 0;
  } catch {
    return 0;
  }
}

// Zooms the main window's web view, the way browser zoom does, so every size and breakpoint scales together.
export function useInterfaceScale(enabled: boolean) {
  const [choice, setChoice] = useState(stored);
  const [automatic, setAutomatic] = useState(1);

  useEffect(() => {
    if (!enabled) return;
    let timer = 0;
    // Moving the window to another screen can change the automatic size.
    const measure = () =>
      void currentMonitor()
        .then((monitor) => monitor && setAutomatic(automaticScale(monitor.size.width / monitor.scaleFactor, monitor.size.height / monitor.scaleFactor)))
        .catch(() => {});
    const later = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(measure, 400);
    };
    measure();
    window.addEventListener("resize", later);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("resize", later);
    };
  }, [enabled]);

  const scale = choice || automatic;
  useEffect(() => {
    if (enabled) void getCurrentWebview().setZoom(scale).catch(() => {});
  }, [enabled, scale]);

  // Ctrl+= and Ctrl+- step through the sizes; Ctrl+0 returns to the automatic one.
  useEffect(() => {
    if (!enabled) return;
    const steps = INTERFACE_SCALES.filter(Boolean);
    const zoom = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      const next =
        event.key === "=" || event.key === "+"
          ? steps.find((size) => size > scale + 0.001)
          : event.key === "-"
            ? [...steps].reverse().find((size) => size < scale - 0.001)
            : event.key === "0"
              ? 0
              : undefined;
      if (next === undefined) return;
      event.preventDefault();
      choose(next);
    };
    window.addEventListener("keydown", zoom);
    return () => window.removeEventListener("keydown", zoom);
  }, [enabled, scale]);

  function choose(next: number) {
    setChoice(next);
    try {
      localStorage.setItem(KEY, String(next));
    } catch {
      // The choice still applies for this session.
    }
  }

  return { choice, automatic, scale, choose };
}
