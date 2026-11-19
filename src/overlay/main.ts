import { mount } from "svelte";
import "../app.css";
import Overlay from "./Overlay.svelte";

document.addEventListener("contextmenu", (event) => event.preventDefault());

export default mount(Overlay, { target: document.getElementById("app")! });
