// Trim mdbook's theme picker to Auto, Light and Dark. Auto (mdbook's
// default) follows the system's light or dark setting. The light and dark
// look come from the design tokens and are the same for every mdbook theme
// of that kind, so Rust, Coal and Ayu would only repeat them: they go, and
// Navy, the dark theme book.toml names, is shown as "Dark".
(function () {
  "use strict";
  // A reader who chose Rust, Coal or Ayu on an earlier visit: store the
  // kept theme with the same look instead. mdbook's script (which runs
  // first) still counts the removed themes as valid, and looks for their
  // buttons when the picker opens. The page keeps its class until the
  // reader picks again; both classes give the same look.
  const kept = { rust: "light", coal: "navy", ayu: "navy" };
  try {
    const stored = localStorage.getItem("mdbook-theme");
    if (stored in kept) {
      localStorage.setItem("mdbook-theme", kept[stored]);
    }
  } catch (e) {
    // No storage: nothing was stored either.
  }
  for (const id of ["mdbook-theme-rust", "mdbook-theme-coal", "mdbook-theme-ayu"]) {
    const button = document.getElementById(id);
    if (button && button.parentElement) {
      button.parentElement.remove();
    }
  }
  const navy = document.getElementById("mdbook-theme-navy");
  if (navy) {
    navy.textContent = "Dark";
  }
  // mdbook marked the stored choice when the page loaded; if that was a
  // removed theme, mark the kept one now.
  const list = document.getElementById("mdbook-theme-list");
  if (list && !list.querySelector(".theme-selected")) {
    let stored = null;
    try {
      stored = localStorage.getItem("mdbook-theme");
    } catch (e) {
      // No storage: Auto.
    }
    const button = document.getElementById("mdbook-theme-" + (stored || "default_theme"));
    if (button) {
      button.classList.add("theme-selected");
    }
  }
})();
