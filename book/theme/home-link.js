// Link the book's title in the menu bar ("irsync") to the
// landing page at "/", as the landing page's own "irsync" links home. mdbook
// has no setting for this. A click on the bar around the title still scrolls
// to the top, mdbook's own use of that click.
//
// mdbook makes the title an h1, on every page, next to the page's own h1.
// It is the name of the site, not the heading of the page, so it stops
// being a heading here. Everything after its first word is in a span that
// irsync.css hides on a narrow screen, where the whole title does not fit.
//
// On the site, the title also says where the book was built from, after an
// aqua dot: "main@b5618f4" for the Dev book, "release-0.1@v0.1.0-beta.7" for
// a release. Each part links to its branch, commit or tag; the dot and the
// "@" do not. site/tools/label_book.py writes the parts into
// theme/source.js as window.irsyncSource; a bare mdbook build has none.
(function () {
  "use strict";
  const title = document.querySelector(".menu-title");
  if (!title || title.querySelector("a")) {
    return;
  }
  title.setAttribute("role", "none");
  const text = title.textContent.trim();
  const cut = text.indexOf(" ");
  const link = document.createElement("a");
  link.href = "/";
  link.title = "irsync home page";
  link.className = "menu-home";
  link.append(cut < 0 ? text : text.slice(0, cut));
  if (cut >= 0) {
    const rest = document.createElement("span");
    rest.className = "menu-title-rest";
    rest.textContent = text.slice(cut);
    link.append(rest);
  }
  title.replaceChildren(link);
  const parts = window.irsyncSource;
  if (Array.isArray(parts) && parts.length) {
    const rest = document.createElement("span");
    rest.className = "menu-title-rest menu-source-parts";
    const dot = document.createElement("span");
    dot.className = "menu-source-dot";
    dot.setAttribute("aria-hidden", "true");
    dot.textContent = "•";
    rest.append(dot);
    parts.forEach(function (part, i) {
      if (i) {
        rest.append("@");
      }
      const to = document.createElement("a");
      to.className = "menu-source";
      to.href = part.url;
      to.textContent = part.text;
      rest.append(to);
    });
    title.append(rest);
  }
})();
