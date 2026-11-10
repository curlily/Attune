const { invoke } = window.__TAURI__.core;

let greetInputEl;
let greetMsgEl;

async function greet() {
  // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
  greetMsgEl.textContent = await invoke("greet", { name: greetInputEl.value });
}

window.addEventListener("DOMContentLoaded", () => {
  greetInputEl = document.querySelector("#greet-input");
  greetMsgEl = document.querySelector("#greet-msg");
  document.querySelector("#greet-form").addEventListener("submit", (e) => {
    e.preventDefault();
    greet();
  });
});

window.connect_tidal = async () => {
  try {
    await invoke("connect_tidal");
  } catch (e) {
    console.error(e);
  }
}

window.connect_spotify = async () => {
  
  try {
    console.log("Mrauw");
    await invoke("connect_spotify");
    console.log("Mrauw complete");
  } catch (e) {
    console.error(e);
  }
}

window.get_playstate = async () => {
  try {
    const playstate = await invoke("get_playstate");
    console.log(playstate);
  } catch (e) {
    console.error(e);
  }
}

document.getElementById("connect_spotify").addEventListener("click", connect_spotify);