import "./styles.css";
import { render } from "solid-js/web";
import { App } from "./app.tsx";
import { applyTheme } from "./lib/theme.ts";

applyTheme();
const root = document.getElementById("root");
if (root) render(() => <App />, root);
