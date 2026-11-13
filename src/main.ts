import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";

// Attune is an app, not a page: right click doesn't open the web view's
// menu (Back, Reload, Inspect…).
document.addEventListener("contextmenu", (event) => event.preventDefault());

export default mount(App, { target: document.getElementById("app")! });
