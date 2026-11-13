// Point the download button straight at the latest release's installer.
// The installer's file name carries its version, so it's looked up from the
// GitHub API. Without it (offline, rate-limited, no release yet) the button
// keeps its link to the releases page.
(async () => {
  const button = document.getElementById("download");
  const info = document.getElementById("release-info");
  try {
    const response = await fetch("https://api.github.com/repos/curlily/Attune/releases/latest", {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!response.ok) return;
    const release = await response.json();
    const installer = release.assets?.find((asset) => asset.name.endsWith("-setup.exe"));
    if (!installer) return;
    button.href = installer.browser_download_url;
    const version = release.tag_name.replace(/^v/, "");
    const size = (installer.size / 1024 / 1024).toFixed(1);
    button.title = `Attune ${version} · ${size} MB`;
    // The page may leave out the line for version details.
    if (info) info.textContent = `Version ${version} · ${size} MB · Windows 10 and 11 · Free`;
  } catch {
    // Keep the link to the releases page.
  }
})();
