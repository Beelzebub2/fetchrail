import { watchRelease } from "./release-feed.mjs";
import { selectAsset } from "./downloads.mjs";
const REPO = "Beelzebub2/fetchrail";
/* Latest release --------------------------------------------------------- */

const osChoice = document.querySelector("[data-download-os]");
const archChoice = document.querySelector("[data-download-arch]");
const formatChoice = document.querySelector("[data-download-format]");
let release;
function updateDownloads(resetFormat = false) {
  const linux = osChoice.value === "linux";
  for (const option of formatChoice.options) option.hidden = linux ? option.value === "exe" : option.value !== "exe";
  if (resetFormat) formatChoice.value = linux ? "deb" : "exe";
  const asset = selectAsset(release, REPO, osChoice.value, archChoice.value, formatChoice.value);
  for (const link of document.querySelectorAll("a[data-download]")) link.href = asset?.url ?? `https://github.com/${REPO}/releases/latest`;
  for (const node of document.querySelectorAll("[data-download-label]")) node.textContent = asset ? `Download ${linux ? "Linux" : "Windows"} ${archChoice.value}` : "View releases";
  for (const node of document.querySelectorAll("[data-release-file]")) node.textContent = asset ? `${asset.name}${asset.size ? ` · ${(asset.size / 1048576).toFixed(1)} MB` : ""}` : (release ? "The selected package is not available in the latest release. View all releases for alternatives." : "Checking the latest release. If unavailable, use View releases to choose your package on GitHub.");
  document.querySelector("[data-download-help]").textContent = linux ? "deb: Ubuntu, Mint, Kali, Debian · rpm: Fedora · AppImage: other glibc desktops · Arch: release PKGBUILD. Check uname -m before choosing a CPU. See the Linux guide below for package availability and tested combinations." : "Windows x64 requires Microsoft Edge WebView2. Setup installs for your account without an administrator prompt.";
}
if (!document.body.dataset.downloadPlatform && /linux/i.test(navigator.userAgent)) osChoice.value = "linux";
osChoice.addEventListener("change", () => updateDownloads(true));
archChoice.addEventListener("change", () => updateDownloads());
formatChoice.addEventListener("change", () => updateDownloads());
updateDownloads(true);
watchRelease(value => {
  release = value;
  for (const node of document.querySelectorAll("[data-release-version]")) node.textContent = release.tag_name;
  updateDownloads();
});
