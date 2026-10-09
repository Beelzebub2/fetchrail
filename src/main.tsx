import React from "react";
import ReactDOM from "react-dom/client";
import App, { DownloadPrompt, Setup } from "./App";

// The setup program and the download prompts load this same page with a word in its address.
const query = new URLSearchParams(location.search);
const prompt = query.get("confirm");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{query.has("setup") ? <Setup /> : prompt ? <DownloadPrompt id={prompt} /> : <App />}</React.StrictMode>,
);
