// Browser-safe release selection. Keep OS, architecture and format explicit.
export function selectAsset(release, repo, os, architecture, format) {
  if (!/^v\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?$/.test(release?.tag_name ?? "")) return null;
  if (!["x64", "arm64"].includes(architecture)) return null;
  let name;
  if (os === "windows" && architecture === "x64" && format === "exe") name = `Fetchrail-Setup-${release.tag_name}-windows-x64.exe`;
  else if (os === "linux" && ["deb", "rpm", "AppImage", "tar.gz"].includes(format)) name = `Fetchrail-${release.tag_name}-linux-${architecture}.${format}`;
  else return null;
  const asset = release.assets?.find(asset => asset.name === name);
  if (!asset) return null;
  try {
    const url = new URL(asset.browser_download_url);
    if (url.href !== `https://github.com/${repo}/releases/download/${release.tag_name}/${name}`) return null;
    return { name, url: url.href, size: asset.size };
  } catch { return null; }
}
