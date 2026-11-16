// Link the book's title in the menu bar ("irsync") to the landing page at
// "/", as the landing page's own "irsync" links home. mdbook has no setting
// for this. A click on the bar around the title still scrolls to the top,
// mdbook's own use of that click.
(function () {
  "use strict";
  const title = document.querySelector(".menu-title");
  if (!title || title.querySelector("a")) {
    return;
  }
  const link = document.createElement("a");
  link.href = "/";
  link.title = "irsync home page";
  link.append(...title.childNodes);
  title.append(link);
})();
