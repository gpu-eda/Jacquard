// Documentation version picker.
//
// The site root tracks main; each release also publishes a frozen copy under
// /vX.Y.Z/ (see the `docs-version` job in .github/workflows/release.yml). This
// adds a control to the menu bar for moving between them. main's docs are the
// default and stay at the root, so arriving with no version in the URL puts you
// on main HEAD, which is what we want people to read.
//
// The list comes from versions.json at the site root, regenerated at deploy
// time from the directories that actually exist on gh-pages. Release candidates
// publish docs too (their release notes link into them) but are deliberately
// left out of the list, which would otherwise fill up with -rc entries.

(function () {
  "use strict";

  // mdBook declares `const path_to_root` in an inline script ahead of this file.
  // A top-level const is a lexical global: reachable by name, absent from
  // `window`, and a ReferenceError if we somehow run first. Hence the guard,
  // and a fallback derived from a stylesheet mdBook always emits.
  function bookRoot() {
    try {
      if (typeof path_to_root === "string") {
        return new URL(path_to_root || "./", window.location.href);
      }
    } catch (e) {
      /* not declared yet: fall through */
    }
    var link = document.querySelector('link[href$="css/general.css"]');
    if (link) {
      return new URL(
        link.getAttribute("href").replace(/css\/general\.css$/, ""),
        window.location.href
      );
    }
    return new URL("./", window.location.href);
  }

  var root = bookRoot();
  var segments = root.pathname.replace(/\/+$/, "").split("/");
  var tail = segments[segments.length - 1];

  // A pinned build lives one directory below the site root, named for its tag.
  var pinned = /^v\d/.test(tail);
  var current = pinned ? tail : "";
  var siteRoot = pinned ? new URL("../", root) : root;

  // Where we are inside this build, so a switch lands on the same page.
  var pagePath = window.location.href.slice(root.href.length);

  function switchTo(version) {
    var base = version ? new URL(version + "/", siteRoot).href : siteRoot.href;
    var target = base + pagePath;
    // Pages come and go between versions; land on that version's index rather
    // than a 404 when the current page has no counterpart there.
    fetch(target, { method: "HEAD" })
      .then(function (r) {
        window.location.href = r.ok ? target : base;
      })
      .catch(function () {
        window.location.href = base;
      });
  }

  function render(versions) {
    var host = document.querySelector("#menu-bar .right-buttons");
    if (!host) return;

    var select = document.createElement("select");
    select.className = "version-picker";
    select.setAttribute("aria-label", "Documentation version");
    if (pinned) select.classList.add("is-pinned");

    var options = [{ value: "", label: "main (latest)" }].concat(
      versions.map(function (v) {
        return { value: v, label: v };
      })
    );

    // A pinned build whose tag predates the list (or was pruned) still needs to
    // show where it is, rather than silently reading as main.
    if (pinned && versions.indexOf(current) === -1) {
      options.push({ value: current, label: current });
    }

    options.forEach(function (o) {
      var el = document.createElement("option");
      el.value = o.value;
      el.textContent = o.label;
      if (o.value === current) el.selected = true;
      select.appendChild(el);
    });

    select.addEventListener("change", function () {
      switchTo(select.value);
    });

    host.insertBefore(select, host.firstChild);
  }

  fetch(new URL("versions.json", siteRoot).href, { cache: "no-cache" })
    .then(function (r) {
      return r.ok ? r.json() : [];
    })
    .then(function (list) {
      var versions = Array.isArray(list) ? list : [];
      // Nothing to switch to and we're on main: no control worth showing.
      // But a pinned reader always gets one, so there's a way back to main.
      if (versions.length || pinned) render(versions);
    })
    .catch(function () {
      if (pinned) render([]);
    });
})();
