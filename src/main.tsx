import "./lib/engine-compat";
import "./index.css";
import "./i18n/multicli";
import React from "react";
import ReactDOM from "react-dom/client";
import BaselineApp from "./multicli/BaselineApp";

// Bypass upstream bootstrap: it starts plugins, analytics and notifications.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><BaselineApp /></React.StrictMode>,
);
