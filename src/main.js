const { invoke } = window.__TAURI__.core;

async function connectTidal() {
  try {
    await invoke("connect_tidal");
  } catch (e) {
    console.error(e);
  }
}

async function connectSpotify() {
  try {
    await invoke("connect_spotify");
  } catch (e) {
    console.error(e);
  }
}

async function getPlaystate() {
  try {
    const playstate = await invoke("get_playstate");
    console.log(playstate);
  } catch (e) {
    console.error(e);
  }
}

window.addEventListener("DOMContentLoaded", () => {
  document.getElementById("connect_tidal").addEventListener("click", connectTidal);
  document.getElementById("connect_spotify").addEventListener("click", connectSpotify);
  document.getElementById("get_playstate").addEventListener("click", getPlaystate);
});
