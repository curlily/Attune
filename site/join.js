// A link to this page with ?join=<code> comes from a Listen along button on
// someone's Discord status. It shows the session's code and opens
// attune://join/<code>, which starts Attune (or reaches the copy already
// running) and joins. A browser can't tell whether Attune is installed, so
// the page stays up, with the download further down.
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

  // Chromium browsers (Chrome, Edge, Opera) quietly ignore a link nothing
  // handles. Firefox would replace this page with an error, so there the
  // link opens in a hidden frame: it still asks to open Attune, and an
  // error stays inside the frame. (Chromium won't open apps from frames.)
  if (navigator.userAgentData) {
    location.href = link;
  } else {
    const frame = document.createElement("iframe");
    frame.hidden = true;
    frame.src = link;
    document.body.append(frame);
  }
  setTimeout(() => {
    if (!left) status.textContent = "Didn't open? You may not have Attune yet.";
  }, 2500);
})();
