import { mount } from "svelte";
// Bundled fonts (latin subsets): the identity does not depend on what is
// installed, and the CSP keeps `font-src 'self'`.
import "@fontsource-variable/inter/index.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-600.css";
import "@fontsource/geist-sans/latin-500.css";
import "@fontsource/geist-sans/latin-600.css";
import "./lib/styles/tokens.css";
import "./lib/styles/base.css";
import App from "./App.svelte";

const target = document.getElementById("app");
if (!target) throw new Error("#app not found");

export default mount(App, { target });
