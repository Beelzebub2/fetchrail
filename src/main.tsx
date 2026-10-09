import React from "react";
import ReactDOM from "react-dom/client";
import App, { DownloadPrompt, Setup } from "./App";
import { DownloadProgress } from "./DownloadProgress";

// The setup program and the download prompts load this same page with a word in its address.
const query = new URLSearchParams(location.search);
const prompt = query.get("confirm");
const progress = query.get("progress");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{query.has("setup") ? <Setup /> : prompt ? <DownloadPrompt id={prompt} /> : progress ? <DownloadProgress id={progress} /> : <App />}</React.StrictMode>,
);
