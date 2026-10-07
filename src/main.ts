import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";

const win = getCurrentWindow();

function addMsg(text: string, cls = "") {
  const el = document.createElement("div");
  el.className = `msg ${cls}`;
  el.textContent = text;
  document.querySelector("#messages")!.appendChild(el);
  el.scrollIntoView({ block: "end" });
}

invoke<string>("get_hotkey").then((hotkey) => {
  addMsg(`First Mate is online. Summon with ${hotkey}.`, "dim");
});

document.querySelector("#input-form")!.addEventListener("submit", (e) => {
  e.preventDefault();
  const input = document.querySelector("#input") as HTMLInputElement;
  const text = input.value.trim();
  if (!text) return;
  addMsg(text, "user");
  input.value = "";
  // LLM integration lands in the next phase; echo for now.
  addMsg(`(no provider wired yet) you said: "${text}"`, "dim");
});

document.querySelector("#close")!.addEventListener("click", () => win.hide());
window.addEventListener("blur", () => win.hide());
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") win.hide();
});
