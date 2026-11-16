// Trim mdbook's theme picker to Auto, Light and Dark. Auto (mdbook's
// default) follows the system's light or dark setting. The light and dark
// look come from the design tokens and are the same for every mdbook theme
// of that kind, so Rust, Coal and Ayu would only repeat them: they go, and
// Navy, the dark theme book.toml names, is shown as "Dark". A reader who
// chose Rust, Coal or Ayu before still gets the light or dark look.
(function () {
  "use strict";
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
})();
