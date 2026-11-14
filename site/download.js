// Point the download buttons straight at the latest release's installer.
// The installer's file name carries its version, so it's looked up from the
// GitHub API. Without it (offline, rate-limited, no release yet) the buttons
// keep their link to the releases page.
(async () => {
  const buttons = document.querySelectorAll("[data-download]");
  const info = document.getElementById("release-info");
  try {
    const response = await fetch("https://api.github.com/repos/curlily/Attune/releases/latest", {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!response.ok) return;
    const release = await response.json();
    const installer = release.assets?.find((asset) => asset.name.endsWith("-setup.exe"));
    if (!installer) return;
    const version = release.tag_name.replace(/^v/, "");
    const size = (installer.size / 1024 / 1024).toFixed(1);
    for (const button of buttons) {
      button.href = installer.browser_download_url;
      button.title = `Attune ${version} · ${size} MB`;
    }
    // The page may leave out the line for version details.
    if (info) info.textContent = `Version ${version} · ${size} MB · Windows 10 and 11 · Free`;
  } catch {
    // Keep the link to the releases page.
  }
})();
