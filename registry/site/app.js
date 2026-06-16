// dotenv-cloud landing page behavior: theme toggle, copy buttons, active nav.
(function () {
  "use strict";

  // ---- Theme toggle (persisted; defaults to system preference) ----
  var root = document.documentElement;
  var toggle = document.querySelector(".theme-toggle");

  function systemDark() {
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  }
  function currentTheme() {
    return root.getAttribute("data-theme") || (systemDark() ? "dark" : "light");
  }
  if (toggle) {
    toggle.addEventListener("click", function () {
      var next = currentTheme() === "dark" ? "light" : "dark";
      root.setAttribute("data-theme", next);
      try { localStorage.setItem("theme", next); } catch (e) {}
    });
  }

  // ---- Copy-to-clipboard buttons on code blocks ----
  document.querySelectorAll(".code").forEach(function (block) {
    var pre = block.querySelector("pre");
    if (!pre) return;
    var btn = document.createElement("button");
    btn.className = "copy-btn";
    btn.type = "button";
    btn.textContent = "Copy";
    btn.addEventListener("click", function () {
      var text = pre.innerText.replace(/\n+$/, "");
      navigator.clipboard.writeText(text).then(function () {
        btn.textContent = "Copied";
        btn.classList.add("copied");
        setTimeout(function () {
          btn.textContent = "Copy";
          btn.classList.remove("copied");
        }, 1500);
      });
    });
    block.appendChild(btn);
  });

  // ---- Active section highlight in the table of contents (scrollspy) ----
  // Only same-page (#anchor) links participate; cross-page links (e.g. the
  // Reference link) are left alone.
  var links = Array.prototype.slice.call(document.querySelectorAll("nav.toc a"));
  var pairs = links
    .filter(function (a) {
      return (a.getAttribute("href") || "").charAt(0) === "#";
    })
    .map(function (a) {
      return { a: a, el: document.getElementById(a.getAttribute("href").slice(1)) };
    })
    .filter(function (p) {
      return p.el;
    });

  var current = null;
  function setActive(a) {
    if (current === a) return;
    if (current) {
      current.classList.remove("active");
      if (!current.className) current.removeAttribute("class"); // avoid leaving class=""
    }
    if (a) a.classList.add("active");
    current = a;
  }

  if (pairs.length) {
    var header = document.querySelector("header.bar");

    function spy() {
      var offset = (header ? header.offsetHeight : 0) + 16;
      var y = window.scrollY + offset;
      var active = pairs[0].a;
      for (var i = 0; i < pairs.length; i++) {
        var top = pairs[i].el.getBoundingClientRect().top + window.scrollY;
        if (top <= y) active = pairs[i].a;
      }
      // At the very bottom, the last section is active even if its top is above.
      if (window.innerHeight + window.scrollY >= document.body.scrollHeight - 2) {
        active = pairs[pairs.length - 1].a;
      }
      setActive(active);
    }

    // Clicking a link reflects immediately, before the smooth scroll settles.
    pairs.forEach(function (p) {
      p.a.addEventListener("click", function () {
        setActive(p.a);
      });
    });

    window.addEventListener("scroll", spy, { passive: true });
    window.addEventListener("resize", spy);
    spy();
  }
})();
