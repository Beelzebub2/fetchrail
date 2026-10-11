import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Feature = { available: boolean; reason: string };
export type PlatformCapabilities = {
  os: "windows" | "linux";
  startup: Feature;
  tray: Feature;
  shutdown: Feature;
  forceShutdown: Feature;
  disconnect: Feature;
  connections: { id: string; name: string }[];
  updateOwner: string;
  browserIntegration: Feature;
};

export function usePlatformCapabilities() {
  const [capabilities, setCapabilities] = useState<PlatformCapabilities | null>(null);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    const refresh = () => void invoke<PlatformCapabilities>("platform_capabilities").then(value => {
      if (!disposed && value?.os) setCapabilities(value);
    }).catch(() => { /* Existing controls stay disabled until the desktop can report support. */ });
    refresh();
    window.addEventListener("focus", refresh);
    return () => { disposed = true; window.removeEventListener("focus", refresh); };
  }, []);
  return capabilities;
}
