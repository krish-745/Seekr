import { invoke } from "@tauri-apps/api/core";
import { onMount } from "solid-js";
import "./App.css";

function App() {
  let input!: HTMLInputElement;

  onMount(() => {
    // The window is reused, so refocus and select the last query every time it is shown.
    window.addEventListener("focus", () => {
      input.focus();
      input.select();
    });
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape") invoke("hide");
    });
  });

  return (
    <main class="panel">
      <input
        ref={input}
        class="query"
        placeholder="Search apps, files and more"
        spellcheck={false}
        autofocus
      />
    </main>
  );
}

export default App;
