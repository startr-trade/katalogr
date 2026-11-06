/* katalogr web view: start-up, the sidebar, routing, the theme and the transport. */
var KW = window.KW = window.KW || {};

KW.state = { manifest: null, capabilities: [], catalogue: null, timeline: null, timelineWaiting: null,
  token: 0, expect: null, explorer: null };

/* One function reaches the API: get(pathAndQuery, callback(status, bodyText)). A host page,
   such as the single-file build or a test, supplies window.KW_TRANSPORT; otherwise fetch. */
KW.get = function (path, callback) {
  if (typeof window.KW_TRANSPORT === "function") { window.KW_TRANSPORT(path, callback); return; }
  var meta = document.querySelector("meta[name=katalogr-api]");
  var base = meta ? (meta.getAttribute("content") || "") : "";
  fetch(base + path).then(function (r) {
    return r.text().then(function (t) { callback(r.status, t); });
  }).catch(function () { callback(0, ""); });
};

KW.theme = (function () {
  "use strict";
  var KEY = "katalogr-theme";

  /* The saved choice, else the system's. */
  function choose() {
    try {
      var saved = window.localStorage.getItem(KEY);
      if (saved === "light" || saved === "coal") { return saved; }
    } catch (e) { /* storage is unavailable: use the system's choice */ }
    var dark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
    return dark ? "coal" : "light";
  }

  function current() {
    return document.documentElement.classList.contains("coal") ? "coal" : "light";
  }

  /* Only the class changes: nothing is drawn again. A graph in view takes the new colours. */
  function apply(name) {
    var cl = document.documentElement.classList;
    cl.remove("light");
    cl.remove("coal");
    cl.add(name);
    if (KW.state.explorer && KW.state.explorer.restyle) { KW.state.explorer.restyle(); }
  }

  function toggle() {
    var next = current() === "coal" ? "light" : "coal";
    apply(next);
    try { window.localStorage.setItem(KEY, next); } catch (e) { /* not saved */ }
    return next;
  }

  return { choose: choose, current: current, apply: apply, toggle: toggle, init: function () { apply(choose()); } };
})();

KW.sidebar = function (activeId) {
  var nav = document.getElementById("kw-sidebar");
  nav.replaceChildren();
  KW.state.manifest.views.filter(function (v) { return v.nav; }).forEach(function (v) {
    var a = KW.dom.link(v.title, KW.core.buildLink(v.id, {}), { "data-kw": "nav-" + v.id });
    if (v.id === activeId) { a.setAttribute("aria-current", "page"); }
    nav.appendChild(a);
  });
};

/* A single-file export names the positions it reflects, and says when it cut the symbols. */
KW.exportHeader = function (main) {
  var d = KW.staticData();
  main.appendChild(KW.dom.h("p", { "class": "kw-positions", "data-kw": "positions" }, [KW.core.positionsLine(d.positions)]));
  KW.core.exportCutNotes(d.partial).forEach(function (t) { main.appendChild(KW.dom.notice(t, "info")); });
};

KW.render = function () {
  var route = KW.core.parseRoute(location.hash);
  var view = route.view || "graph";
  route.view = view;
  var token = ++KW.state.token;
  var live = function () { return token === KW.state.token; };
  var main = document.getElementById("kw-main");
  main.replaceChildren();
  KW.state.explorer = null;
  if (KW.static) { KW.exportHeader(main); }
  var row = KW.state.manifest.views.filter(function (v) { return v.id === view; })[0];
  KW.sidebar(row && row.nav ? view : "");
  if (!row) {
    main.appendChild(KW.dom.notice(KW.core.messages.unknownView, "info"));
    return;
  }
  var section = KW.dom.h("section", { "data-kw": "view-" + view }, [KW.dom.h("h1", {}, [row.title])]);
  main.appendChild(section);
  if (KW.core.viewState(row, KW.state.capabilities) === "absent") {
    KW.views.absent(section, row);
    return;
  }
  if (row.kind === "graph") { KW.views.graph(route, section, live); }
  else if (row.kind === "service-map") { KW.views.serviceMap(route, section, live); }
  else if (row.kind === "panels") { KW.views.panels(route, row, section, live); }
  else if (row.kind === "symbol") { KW.views.symbol(route, section, live); }
  else if (row.kind === "search") { KW.views.search(route, section, live); }
  else if (row.kind === "lineage") { KW.views.lineage(route, section, live); }
};

KW.boot = function () {
  KW.state.manifest = JSON.parse(document.getElementById("kw-views").textContent);
  KW.theme.init();
  document.getElementById("kw-theme").addEventListener("click", function () { KW.theme.toggle(); });
  var box = document.getElementById("kw-search");
  box.addEventListener("change", function () {
    location.hash = KW.core.buildLink("search", { q: box.value });
  });
  window.addEventListener("hashchange", function () {
    // A tab switch already drew what the new hash names.
    if (KW.state.expect !== null && location.hash === KW.state.expect) { KW.state.expect = null; return; }
    KW.state.expect = null;
    KW.render();
  });
  KW.get("/api/v1/meta", function (status, body) {
    if (status === 200) {
      try { KW.state.capabilities = JSON.parse(body).capabilities || []; } catch (e) { KW.state.capabilities = []; }
      KW.render();
      return;
    }
    var main = document.getElementById("kw-main");
    var m = KW.core.statusMessage(status, body);
    KW.sidebar("");
    main.appendChild(KW.dom.notice(m.text + (m.detail ? " " + m.detail : ""), "error"));
  });
};

/* The scripts load at the end of <body>, so the page is ready. A test or a host page that wants
   to start it itself sets KW_MANUAL_BOOT first. */
if (!window.KW_MANUAL_BOOT) { KW.boot(); }
