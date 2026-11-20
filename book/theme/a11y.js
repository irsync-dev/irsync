// Two things for keyboard users that mdbook has no setting for.
//
// A "Skip to content" link, the first stop of the Tab key: without it the
// chapters in the sidebar, some fifty links, come before the page.
//
// A code block or a table that is wider than the page scrolls inside
// itself (irsync.css). Not every browser lets the keyboard reach such a
// box, so each one that scrolls takes a place in the Tab order.
(function () {
  "use strict";
  const main = document.querySelector(".content main");
  if (!main) {
    return;
  }

  main.tabIndex = -1;
  const skip = document.createElement("a");
  skip.className = "irsync-skip";
  skip.href = "#";
  skip.textContent = "Skip to content";
  skip.addEventListener("click", function (event) {
    event.preventDefault();
    main.focus();
  });
  document.body.prepend(skip);

  function mark() {
    for (const box of main.querySelectorAll("pre, pre > code, table")) {
      const scrolls = box.scrollWidth > box.clientWidth + 1 &&
        /auto|scroll/.test(getComputedStyle(box).overflowX);
      if (scrolls) {
        box.tabIndex = 0;
      } else {
        box.removeAttribute("tabindex");
      }
    }
  }

  // Again when the width changes, and once the web fonts have set the
  // final widths.
  let pending = 0;
  window.addEventListener("resize", function () {
    cancelAnimationFrame(pending);
    pending = requestAnimationFrame(mark);
  });
  if (document.fonts && document.fonts.ready) {
    document.fonts.ready.then(mark);
  }
  mark();
})();
