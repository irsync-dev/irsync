// The version switcher in the top bar: a button that opens the list of the
// books (Stable, Beta, Alpha, Dev). site/tools/switcher.py writes the
// markup, with every link already pointing at the right page, into the
// books of the built site. A plain mdbook build has no such markup, and
// this script does nothing there.
(function () {
  "use strict";
  const button = document.getElementById("irsync-book-toggle");
  const list = document.getElementById("irsync-book-list");
  if (!button || !list) {
    return;
  }

  // Opened from the keyboard, focus moves into the list; opened with the
  // mouse, it stays on the button.
  function open(fromKeyboard) {
    list.hidden = false;
    button.setAttribute("aria-expanded", "true");
    if (!fromKeyboard) {
      return;
    }
    const current = list.querySelector('[aria-current="true"]') || list.querySelector("a");
    if (current) {
      current.focus();
    }
  }

  function close(refocus) {
    if (list.hidden) {
      return;
    }
    list.hidden = true;
    button.setAttribute("aria-expanded", "false");
    if (refocus) {
      button.focus();
    }
  }

  button.addEventListener("click", function (event) {
    event.stopPropagation();
    if (list.hidden) {
      // A click made with Enter or Space has no click count.
      open(event.detail === 0);
    } else {
      close(false);
    }
  });

  // A link that stays on the same page keeps the reader's place in it.
  for (const link of list.querySelectorAll("a[data-same-page]")) {
    link.addEventListener("click", function () {
      if (location.hash) {
        link.href = link.href.split("#")[0] + location.hash;
      }
    });
  }

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape" && !list.hidden) {
      event.preventDefault();
      // mdbook's own keys (search, chapters) must not act on it.
      event.stopPropagation();
      close(true);
    }
  }, true);

  document.addEventListener("click", function (event) {
    if (!list.contains(event.target) && event.target !== button) {
      close(false);
    }
  });

  // Tabbing out of the list closes it.
  list.addEventListener("focusout", function (event) {
    if (event.relatedTarget && !list.contains(event.relatedTarget) && event.relatedTarget !== button) {
      close(false);
    }
  });
})();
