// Runs before first paint so a saved theme or accent never flashes the default,
// and so the scroll scenes are laid out from the first frame.
(function () {
  var root = document.documentElement;
  root.classList.add("js");
  if (!matchMedia("(prefers-reduced-motion: reduce)").matches) root.classList.add("motion");
  try {
    var theme = localStorage.getItem("fetchrail-theme");
    var accent = localStorage.getItem("fetchrail-accent");
    if (theme === "light") root.dataset.theme = theme;
    if (accent === "azure" || accent === "jade" || accent === "iris") root.dataset.accent = accent;
  } catch (error) {
    // Storage can be blocked; the default look still applies.
  }
})();
