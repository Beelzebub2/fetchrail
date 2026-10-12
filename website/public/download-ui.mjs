import { watchRelease } from "./release-feed.mjs";
import { selectAsset } from "./downloads.mjs";
const REPO = "Beelzebub2/fetchrail";
/* Latest release --------------------------------------------------------- */

// The choices are radio buttons in a card; a page for one system only offers that system's options.
const picker = document.querySelector("[data-download-choices]");
const chosen = (name) => picker.querySelector(`input[name="${name}"]:checked`).value;
const choose = (name, value) => {
  const option = picker.querySelector(`input[name="${name}"][value="${value}"]`);
  if (option) option.checked = true;
};
let release;
function updateDownloads(resetFormat = false) {
  const linux = chosen("os") === "linux";
  for (const option of picker.querySelectorAll('input[name="format"]')) option.parentElement.hidden = linux ? option.value === "exe" : option.value !== "exe";
  if (resetFormat) choose("format", linux ? "deb" : "exe");
  const asset = selectAsset(release, REPO, chosen("os"), chosen("arch"), chosen("format"));
  for (const link of document.querySelectorAll("a[data-download]")) link.href = asset?.url ?? `https://github.com/${REPO}/releases/latest`;
  for (const node of document.querySelectorAll("[data-download-label]")) node.textContent = asset ? `Download ${linux ? "Linux" : "Windows"} ${chosen("arch")}` : "View releases";
  for (const node of document.querySelectorAll("[data-release-file]")) node.textContent = asset ? `${asset.name}${asset.size ? ` · ${(asset.size / 1048576).toFixed(1)} MB` : ""}` : (release ? "The selected package is not available in the latest release. View all releases for alternatives." : "Checking the latest release. If unavailable, use View releases to choose your package on GitHub.");
  document.querySelector("[data-download-help]").textContent = linux ? "deb: Ubuntu, Mint, Kali, Debian · rpm: Fedora · AppImage: other glibc desktops · Arch: release PKGBUILD. Check uname -m before choosing a CPU. See the Linux guide below for package availability and tested combinations." : "Windows x64 requires Microsoft Edge WebView2. Setup installs for your account without an administrator prompt.";
}
if (!document.body.dataset.downloadPlatform && /linux/i.test(navigator.userAgent)) choose("os", "linux");
picker.addEventListener("change", (event) => updateDownloads(event.target.name === "os"));
updateDownloads(true);
watchRelease(value => {
  release = value;
  for (const node of document.querySelectorAll("[data-release-version]")) node.textContent = release.tag_name;
  updateDownloads();
});
