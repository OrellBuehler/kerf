/* =============================================================
   Direct downloads.

   Release assets carry their version in the file name
   (`Kerf_0.19.1_x64-setup.exe`), so GitHub's stable
   `releases/latest/download/<name>` URL cannot address them. This
   resolves the real asset URLs from the API instead, points the
   hero button at the visitor's own platform, and fills in the
   version and size on every platform row.

   Progressive enhancement: every link ships pointing at the
   releases page, so the page stays usable with this file blocked
   or the API unreachable.
   ============================================================= */
(function () {
  "use strict";

  var root = document.querySelector("[data-dl]");
  if (!root || !window.fetch) return;

  var REPO = root.getAttribute("data-repo");
  if (!REPO) return;

  function all(sel) {
    return Array.prototype.slice.call(root.querySelectorAll(sel));
  }

  /* --- The asset table -------------------------------------------------
     One entry per downloadable file we publish. `match` recognizes the
     asset by name, so `.sig` files and latest.json never match. */
  var ASSETS = [
    { key: "windows-exe", os: "windows", sub: "Installer (.exe)", match: /_x64-setup\.exe$/i },
    { key: "windows-msi", os: "windows", sub: "MSI package", match: /_x64_en-US\.msi$/i },
    { key: "macos-arm", os: "macos", sub: "Apple silicon (.dmg)", match: /_aarch64\.dmg$/i },
    { key: "macos-intel", os: "macos", sub: "Intel (.dmg)", match: /_x64\.dmg$/i },
    { key: "linux-appimage", os: "linux", sub: "AppImage", match: /_amd64\.AppImage$/i },
    { key: "linux-deb", os: "linux", sub: "Debian / Ubuntu (.deb)", match: /_amd64\.deb$/i },
    { key: "linux-rpm", os: "linux", sub: "Fedora / RHEL (.rpm)", match: /\.x86_64\.rpm$/i }
  ];

  /* Which file a visitor on this platform actually wants. */
  var PREFERRED = {
    windows: "windows-exe",
    "macos:arm64": "macos-arm",
    "macos:x64": "macos-intel",
    linux: "linux-appimage"
  };

  var OS_NAME = { windows: "Windows", macos: "macOS", linux: "Linux" };

  /* --- Platform detection ---------------------------------------------- */

  function detectOs() {
    var ua = navigator.userAgent || "";
    // Phones and tablets get no desktop build: leave them undetected so the
    // button stays a neutral "Download" pointing at the release page.
    if (/Android|iPhone|iPad|iPod/i.test(ua)) return "";
    var hint = (navigator.userAgentData && navigator.userAgentData.platform) || "";
    var p = hint || navigator.platform || "";
    if (/Win/i.test(p) || /Windows/i.test(ua)) return "windows";
    if (/Mac/i.test(p) || /Mac OS X/i.test(ua)) return "macos";
    if (/Linux|X11|CrOS/i.test(p) || /Linux|X11|CrOS/i.test(ua)) return "linux";
    return "";
  }

  // Safari exposes no architecture hint and every Mac still reports
  // "MacIntel" in navigator.platform — so ask the GPU. Apple silicon renders
  // through "Apple GPU" / "Apple M…"; an Intel Mac names Intel, AMD or NVIDIA.
  function macIsAppleSilicon() {
    try {
      var gl =
        document.createElement("canvas").getContext("webgl") ||
        document.createElement("canvas").getContext("experimental-webgl");
      if (!gl) return null;
      var dbg = gl.getExtension("WEBGL_debug_renderer_info");
      if (!dbg) return null;
      var r = String(gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) || "");
      if (/Intel|AMD|Radeon|NVIDIA|GeForce/i.test(r)) return false;
      if (/Apple/i.test(r)) return true;
    } catch (e) {
      /* blocked by a privacy setting — fall through to "unknown" */
    }
    return null;
  }

  /* --- Release lookup ---------------------------------------------------
     Not `/releases/latest`: publishing a tag makes it "latest" some 25
     minutes before the slowest bundle finishes uploading, so the newest
     release can legitimately carry no assets yet. Walk back to the newest
     one that actually has files. */

  var CACHE_KEY = "kerf.dl.v1";
  var CACHE_MS = 30 * 60 * 1000;

  function cached() {
    try {
      var v = JSON.parse(sessionStorage.getItem(CACHE_KEY) || "null");
      return v && Date.now() - v.at < CACHE_MS ? v.data : null;
    } catch (e) {
      return null;
    }
  }

  function store(data) {
    try {
      sessionStorage.setItem(CACHE_KEY, JSON.stringify({ at: Date.now(), data: data }));
    } catch (e) {
      /* private mode — just skip the cache */
    }
  }

  function resolve(rel) {
    var files = {};
    var oses = {};
    (rel.assets || []).forEach(function (a) {
      ASSETS.forEach(function (spec) {
        if (!files[spec.key] && spec.match.test(a.name)) {
          files[spec.key] = { url: a.browser_download_url, size: a.size };
          oses[spec.os] = true;
        }
      });
    });
    var n = Object.keys(files).length;
    if (!n) return null;
    return {
      tag: rel.tag_name,
      url: rel.html_url,
      files: files,
      complete: oses.windows && oses.macos && oses.linux
    };
  }

  /* The newest release carrying all three platforms. A release whose slowest
     bundle is still uploading has some but not all, and offering it would
     leave whoever is on the missing platform with no direct link at all — so
     fall back to the newest complete one, and only settle for a partial
     release if the whole fetched window is partial. */
  function pickRelease(list) {
    var partial = null;
    for (var i = 0; i < list.length; i++) {
      if (list[i].draft || list[i].prerelease) continue;
      var got = resolve(list[i]);
      if (!got) continue;
      if (got.complete) return got;
      if (!partial) partial = got;
    }
    return partial;
  }

  function fetchRelease() {
    var hit = cached();
    if (hit) return Promise.resolve(hit);
    return fetch("https://api.github.com/repos/" + REPO + "/releases?per_page=10", {
      headers: { Accept: "application/vnd.github+json" }
    })
      .then(function (r) {
        if (!r.ok) throw new Error("github " + r.status);
        return r.json();
      })
      .then(function (list) {
        var data = Array.isArray(list) && pickRelease(list);
        if (!data) throw new Error("no release with assets");
        store(data);
        return data;
      });
  }

  /* --- Rendering -------------------------------------------------------- */

  function mb(bytes) {
    if (!bytes) return "";
    var m = bytes / 1048576;
    return (m >= 100 ? Math.round(m) : m.toFixed(1)) + " MB";
  }

  function specFor(key) {
    for (var i = 0; i < ASSETS.length; i++) if (ASSETS[i].key === key) return ASSETS[i];
    return null;
  }

  function apply(data) {
    root.classList.add("dl-live");

    // Every platform row on the page — hero menu and the Get-it grid alike.
    ASSETS.forEach(function (spec) {
      var file = data.files[spec.key];
      all('[data-dl-asset="' + spec.key + '"]').forEach(function (row) {
        if (!file) {
          row.setAttribute("hidden", "");
          return;
        }
        row.removeAttribute("hidden");
        row.href = file.url;
        row.removeAttribute("target");
        row.removeAttribute("rel");
        var size = row.querySelector("[data-dl-size]");
        if (size) size.textContent = mb(file.size);
      });
    });

    all("[data-dl-version]").forEach(function (el) {
      el.textContent = data.tag;
      el.removeAttribute("hidden");
    });

    var os = detectOs();
    if (!os) return;

    var wantKey = PREFERRED[os];
    if (os === "macos") {
      // Unknown architecture keeps Apple silicon — the overwhelming majority
      // of Macs in use — and the Intel build stays one click away in the menu.
      wantKey = PREFERRED[macIsAppleSilicon() === false ? "macos:x64" : "macos:arm64"];
    }

    var spec = specFor(wantKey);
    var file = spec && data.files[spec.key];
    if (!file) return;

    all("[data-dl-primary]").forEach(function (main) {
      main.href = file.url;
      main.removeAttribute("target");
      main.removeAttribute("rel");
      var label = main.querySelector("[data-dl-label]");
      if (label) label.textContent = "Download for " + OS_NAME[spec.os];
    });

    all("[data-dl-meta]").forEach(function (meta) {
      meta.textContent = [data.tag, spec.sub, mb(file.size)].filter(Boolean).join(" · ");
      meta.removeAttribute("hidden");
    });

    // Show the visitor where the button points.
    all('[data-dl-asset^="' + spec.os + '"]').forEach(function (row) {
      row.classList.add("is-mine");
    });
    all('[data-dl-asset="' + spec.key + '"]').forEach(function (row) {
      row.classList.add("is-pick");
    });
    all('[data-dl-card="' + spec.os + '"]').forEach(function (card) {
      card.classList.add("is-mine");
    });
  }

  /* --- The split button's menu ------------------------------------------ */

  function wireMenu(menu) {
    var wrap = menu.closest(".dl");
    var toggle = wrap && wrap.querySelector("[data-dl-toggle]");
    if (!toggle) return;

    function open(on) {
      toggle.setAttribute("aria-expanded", on ? "true" : "false");
      if (on) menu.removeAttribute("hidden");
      else menu.setAttribute("hidden", "");
    }

    toggle.addEventListener("click", function (e) {
      e.preventDefault();
      open(toggle.getAttribute("aria-expanded") !== "true");
    });
    document.addEventListener("click", function (e) {
      if (!wrap.contains(e.target)) open(false);
    });
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape") open(false);
    });
    menu.addEventListener("click", function (e) {
      if (e.target && e.target.closest && e.target.closest("a")) open(false);
    });
    open(false);
  }

  all("[data-dl-menu]").forEach(wireMenu);

  fetchRelease().then(apply, function () {
    /* Offline, rate-limited, or no published assets: the links already point
       at the releases page, which is the right destination anyway. */
    root.classList.add("dl-fallback");
  });
})();
