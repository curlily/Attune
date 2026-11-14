// A link to this page with ?join=<code> comes from a Listen along button on
// someone's Discord status. It shows the session's code and opens
// attune://join/<code>, which starts Attune (or reaches the copy already
// running) and joins. A browser can't tell whether Attune is installed, so
// the page stays up with the download next to the button that opens it.
(() => {
  // Same rules as normalize_join_code in protocol/src/messages.rs.
  const raw = new URLSearchParams(location.search).get("join") ?? "";
  const code = raw.trim().toUpperCase();
  if (!/^[A-HJ-NP-Z]{4}$/.test(code)) return;

  const link = `attune://join/${code}`;
  document.getElementById("join").hidden = false;
  document.getElementById("join-code").textContent = code;
  document.getElementById("join-open").href = link;
  document.title = `Join ${code} · Attune`;
  const status = document.getElementById("join-status");

  // The page loses focus when the browser asks to open Attune, or Attune
  // comes to the front.
  let left = false;
  const leaving = () => {
    left = true;
    status.textContent = "Opening Attune… You can close this page once it has joined.";
  };
  window.addEventListener("blur", leaving, { once: true });
  document.addEventListener("visibilitychange", () => document.hidden && leaving(), { once: true });

  // Chromium browsers (Chrome, Edge, Opera) quietly ignore a link
  // nothing handles. Others may show an error page instead of this one, so
  // there it waits for the button.
  if (navigator.userAgentData) {
    location.href = link;
    setTimeout(() => {
      if (!left) status.textContent = "Didn't open? You may not have Attune yet.";
    }, 2500);
  } else {
    status.textContent = "Choose Open in Attune to join.";
  }
})();
