// Link the book's title in the menu bar ("irsync Documentation") to the
// landing page at "/", as the landing page's own "irsync" links home. mdbook
// has no setting for this. A click on the bar around the title still scrolls
// to the top, mdbook's own use of that click.
//
// mdbook makes the title an h1, on every page, next to the page's own h1.
// It is the name of the site, not the heading of the page, so it stops
// being a heading here. Everything after its first word is in a span that
// irsync.css hides on a narrow screen, where the whole title does not fit.
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
  link.append(cut < 0 ? text : text.slice(0, cut));
  if (cut >= 0) {
    const rest = document.createElement("span");
    rest.className = "menu-title-rest";
    rest.textContent = text.slice(cut);
    link.append(rest);
  }
  title.replaceChildren(link);
})();
