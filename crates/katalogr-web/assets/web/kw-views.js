/* katalogr web view: the views. Each reads the API through KW.get and draws into the section it
   is given. A request that comes back after the page has moved on draws nothing. */
var KW = window.KW = window.KW || {};

KW.views = (function () {
  "use strict";

  var D = function () { return KW.dom; };

  function wrapParams(route, extra) {
    var out = {};
    Object.keys(route.params).forEach(function (k) { out[k] = route.params[k]; });
    Object.keys(extra || {}).forEach(function (k) { out[k] = extra[k]; });
    return out;
  }

  function parse(body) {
    try { return JSON.parse(body); } catch (e) { return null; }
  }

  function walk(obj, path) {
    var v = obj;
    String(path || "").split(".").forEach(function (p) {
      v = v === null || v === undefined ? undefined : v[p];
    });
    return v;
  }

  /* A failed request, as a notice in `into`. */
  function failure(into, status, body) {
    var m = KW.core.statusMessage(status, body);
    var items = m.items.map(function (i) {
      return KW.dom.link(i, KW.core.buildLink("symbol", { symbol: i }));
    });
    into.appendChild(D().notice(m.text + (m.detail ? " " + m.detail : ""), status === 409 ? "info" : "warn", items));
  }

  /* GET `path`; `ok(parsed)` on 200, else a notice in `into`. `live()` says whether the page is
     still on this view. */
  function load(path, into, live, ok) {
    KW.get(path, function (status, body) {
      if (!live()) { return; }
      if (status !== 200) { failure(into, status, body); return; }
      var parsed = parse(body);
      if (parsed === null) { failure(into, 500, ""); return; }
      ok(parsed);
    });
  }

  function notes(into, answer) {
    KW.core.answerNotes(answer).forEach(function (t) { into.appendChild(D().notice(t, "info")); });
  }

  function absent(section, row) {
    section.appendChild(D().notice(row.absent || KW.core.messages.unknownView, "info"));
  }

  /* ---- the time slider ---- */

  function utc(secs) {
    if (typeof secs !== "number") { return "no date"; }
    return new Date(secs * 1000).toISOString().slice(0, 16).replace("T", " ") + " UTC";
  }

  function pointText(p) {
    var commit = "";
    (p.positions || []).forEach(function (pos) { if (!commit && pos.commit) { commit = String(pos.commit).slice(0, 8); } });
    return utc(p.committed) + (commit ? "  " + commit : "") + "  " + (p.source === "snapshot" ? "snapshot" : "stored");
  }

  /* Timeline is read once per page load. */
  function timeline(cb) {
    var s = KW.state;
    if (s.timeline) { cb(s.timeline); return; }
    if (s.timelineWaiting) { s.timelineWaiting.push(cb); return; }
    s.timelineWaiting = [cb];
    KW.get("/api/v1/timeline", function (status, body) {
      var t = status === 200 ? parse(body) : null;
      s.timeline = t || { result: { points: [] } };
      var waiting = s.timelineWaiting;
      s.timelineWaiting = null;
      waiting.forEach(function (f) { f(s.timeline); });
    });
  }

  /* After an answer: the points stored for this question, or the sentence saying there are none. */
  function slider(into, answer, route, live) {
    timeline(function (t) {
      if (!live()) { return; }
      var points = KW.core.pointsFor(t, answer.query);
      if (!points.length) {
        into.appendChild(D().notice(KW.core.messages.nothingStored, "info"));
        return;
      }
      var current = -1;
      points.forEach(function (p, i) { if (p.view_id === route.params.id) { current = i; } });
      var label = D().h("span", { "data-kw": "slider-label" }, [pointText(points[current < 0 ? points.length - 1 : current])]);
      var range = D().h("input", { type: "range", min: 0, max: points.length - 1,
        value: current < 0 ? points.length - 1 : current, "data-kw": "slider", "aria-label": "Stored points" }, []);
      range.value = String(current < 0 ? points.length - 1 : current);
      range.addEventListener("change", function () {
        var p = points[parseInt(range.value, 10)];
        if (!p) { return; }
        label.textContent = pointText(p);
        var params = wrapParams(route, { id: p.view_id });
        delete params.tab;
        location.hash = KW.core.buildLink(route.view, params);
      });
      var box = D().h("div", { "class": "kw-slider" }, [range, " ", label]);
      if (route.params.id) {
        var back = wrapParams(route, {});
        delete back.id;
        box.appendChild(document.createTextNode(" "));
        box.appendChild(D().link("Back to current", KW.core.buildLink(route.view, back), { "data-kw": "slider-back" }));
      }
      into.appendChild(box);
    });
  }

  /* ---- graph and service map ---- */

  function catalogue(cb, into, live) {
    if (KW.state.catalogue) { cb(KW.state.catalogue); return; }
    load("/api/v1/views/", into, live, function (c) { KW.state.catalogue = c; cb(c); });
  }

  function controls(route, op, info, view) {
    var box = D().h("div", { "class": "kw-controls", "data-kw": "controls" }, []);
    (info.params || []).forEach(function (p) {
      var current = route.params[p.name] !== undefined ? route.params[p.name] : (p.default || "");
      var el;
      if (p.type === "enum") {
        el = D().h("select", { "data-kw": "param-" + p.name }, (p.values || []).map(function (v) {
          return D().h("option", { value: v }, [v]);
        }));
        el.value = current;
      } else if (p.type === "integer") {
        el = D().h("input", { type: "number", min: 1, "data-kw": "param-" + p.name }, []);
        el.value = current;
      } else if (p.type === "boolean") {
        el = D().h("input", { type: "checkbox", "data-kw": "param-" + p.name }, []);
        el.value = current;
        el.checked = current === "true";
      } else {
        el = D().h("input", { type: "text", "data-kw": "param-" + p.name }, []);
        el.value = current;
      }
      el.setAttribute("aria-label", p.name);
      el.addEventListener("change", function () {
        var v = p.type === "boolean" ? (el.checked === undefined ? el.value : (el.checked ? "true" : "false")) : el.value;
        var params = wrapParams(route, {});
        params[p.name] = v;
        if (view === "graph") { params.op = op; }
        delete params.id;
        location.hash = KW.core.buildLink(route.view, params);
      });
      box.appendChild(D().h("label", {}, [p.name + " ", el]));
    });
    return box;
  }

  function opFor(route, view) {
    if (view === "service-map") { return "contracts"; }
    return route.params.op || (route.params.symbol ? "callers" : "structure");
  }

  /* The request for the answer: the op's own parameters present in the route, and `at`. */
  function viewRequest(route, op, info) {
    var q = { consistency: "wait", timeout: KW.state.manifest.wait_seconds };
    (info.params || []).forEach(function (p) {
      if (route.params[p.name] !== undefined && route.params[p.name] !== "") { q[p.name] = route.params[p.name]; }
    });
    if (route.params.at) { q.at = route.params.at; }
    if (op === "callers" && route.params.symbol) { q.symbol = route.params.symbol; }
    return KW.core.apiPath("views/" + op, q);
  }

  function diagramAndExplore(section, route, view, answer, live) {
    var holder = D().h("div", { "class": "kw-view", "data-kw": "view-body" }, []);
    var tabs = D().h("p", { "class": "kw-tabs" }, []);
    var tab = route.params.tab === "explore" ? "explore" : "diagram";
    var highlight = route.params.symbol || null;
    function show(which) {
      tab = which;
      KW.state.explorer = null;
      if (which === "explore") {
        KW.state.explorer = KW.explore.open(holder, answer.graph, { highlight: highlight, get: KW.get });
      } else {
        KW.diagram.draw(holder, answer.mermaid);
      }
    }
    function tabLink(which, text) {
      var params = wrapParams(route, { tab: which });
      var a = D().link(text, KW.core.buildLink(route.view, params), { "data-kw": "tab-" + which });
      a.addEventListener("click", function (ev) {
        if (ev && ev.preventDefault) { ev.preventDefault(); }
        if (tab === which) { return; }
        show(which);
        KW.state.expect = KW.core.buildLink(route.view, params);
        location.hash = KW.state.expect;
      });
      return a;
    }
    tabs.appendChild(tabLink("diagram", "Diagram"));
    tabs.appendChild(document.createTextNode(" "));
    tabs.appendChild(tabLink("explore", "Explore"));
    section.appendChild(tabs);
    section.appendChild(holder);
    show(tab);
    notes(section, answer);
    slider(section, answer, route, live);
  }

  function graphLike(section, route, view, live) {
    var op = opFor(route, view);
    catalogue(function (cat) {
      var info = (cat.views || []).filter(function (v) { return v.op === op; })[0];
      if (!info) { section.appendChild(D().notice("There is no view op " + op + " on this server.", "warn")); return; }
      section.appendChild(controls(route, op, info, view));
      var path = route.params.id ?
        KW.core.apiPath("stored-view", { id: route.params.id }) : viewRequest(route, op, info);
      load(path, section, live, function (answer) {
        diagramAndExplore(section, route, view, answer, live);
      });
    }, section, live);
  }

  function graph(route, section, live) { graphLike(section, route, "graph", live); }
  function serviceMap(route, section, live) { graphLike(section, route, "service-map", live); }

  /* ---- panels ---- */

  function panels(route, row, section, live) {
    (row.panels || []).forEach(function (panel) {
      var box = D().h("section", { "class": "kw-panel", "data-kw": "panel-" + panel.id }, [D().h("h3", {}, [panel.title])]);
      section.appendChild(box);
      if (KW.core.viewState(panel, KW.state.capabilities) === "absent") {
        box.appendChild(D().notice(panel.absent || KW.core.messages.unknownView, "info"));
        return;
      }
      if (!panel.op) { return; }
      load(KW.core.apiPath(panel.op, panel.params || {}), box, live, function (body) {
        var rows = walk(body, panel.rows) || [];
        if (!rows.length) {
          box.appendChild(D().notice(panel.empty || "Nothing to show.", "info"));
        } else {
          box.appendChild(D().table(panel.columns || [], rows));
        }
        notes(box, body);
      });
    });
  }

  /* ---- symbol ---- */

  function nameList(items) {
    return D().h("ul", {}, items.map(function (t) { return D().h("li", {}, [t]); }));
  }

  function symbol(route, section, live) {
    var name = route.params.symbol;
    if (!name) { section.appendChild(D().notice("Name a symbol to see its page.", "info")); return; }
    KW.get(KW.core.apiPath("explain", { symbol: name }), function (status, body) {
      if (!live()) { return; }
      if (status !== 200) { failure(section, status, body); return; }
      var explain = parse(body) || {};
      var sym = (explain.result || {}).symbol || { name: name };
      var results = {};
      var wanted = ["why", "tests", "show", "callers"];
      var left = wanted.length;
      function done(key) {
        return function (st, b) {
          results[key] = st === 200 ? parse(b) : null;
          if (--left === 0 && live()) { draw(); }
        };
      }
      var line = sym.span ? sym.span.start_line : 1;
      KW.get(KW.core.apiPath("why", { location: sym.path + ":" + line }), done("why"));
      KW.get(KW.core.apiPath("tests", { text: sym.name }), done("tests"));
      KW.get(KW.core.apiPath("show", { symbol: sym.name }), done("show"));
      KW.get(KW.core.apiPath("views/callers", { symbol: sym.name, consistency: "wait", timeout: KW.state.manifest.wait_seconds }), done("callers"));

      function draw() {
        var r = explain.result || {};
        section.appendChild(D().h("h2", { "data-kw": "symbol-name" }, [sym.name]));
        section.appendChild(D().h("p", {}, [sym.kind + " " + sym.path + ":" + line]));
        if (r.signature) { section.appendChild(D().h("pre", { "data-kw": "symbol-signature" }, [r.signature])); }
        if (r.doc) { section.appendChild(D().h("p", { "data-kw": "symbol-doc" }, [r.doc])); }

        var reqs = KW.core.requirementsOf(results.why, results.tests);
        var reqBox = D().h("section", { "data-kw": "section-requirements" }, [D().h("h3", {}, ["Requirements"])]);
        reqBox.appendChild(reqs.length ?
          nameList(reqs.map(function (q) { return q.label ? q.name + " - " + q.label : q.name; })) :
          D().notice(KW.core.messages.noRequirements, "info"));
        section.appendChild(reqBox);

        var tests = (results.tests && results.tests.result) || [];
        var testBox = D().h("section", { "data-kw": "section-tests" }, [D().h("h3", {}, ["Tests"])]);
        testBox.appendChild(tests.length ?
          nameList(tests.map(function (t) { return t.test.name + " (" + t.role + ")"; })) :
          D().notice(KW.core.messages.noTests, "info"));
        section.appendChild(testBox);

        var goals = ((results.why && results.why.result) || {}).goals || [];
        var goalBox = D().h("section", { "data-kw": "section-goals" }, [D().h("h3", {}, ["Goals served"])]);
        if (goals.length) {
          goalBox.appendChild(nameList(goals.map(function (g) { return g.label ? g.target + " - " + g.label : g.target; })));
          section.appendChild(goalBox);
        }

        var callers = D().h("section", { "data-kw": "section-callers" }, [D().h("h3", {}, ["Callers"])]);
        section.appendChild(callers);
        if (results.callers) {
          var host = D().h("div", {}, []);
          callers.appendChild(host);
          KW.diagram.draw(host, results.callers.mermaid);
          notes(callers, results.callers);
        }

        var shown = results.show && results.show.result;
        if (shown) {
          section.appendChild(D().h("section", { "data-kw": "section-source" }, [
            D().h("h3", {}, ["Source"]), D().h("pre", {}, [shown.text])]));
        }
      }
    });
  }

  /* ---- search ---- */

  function search(route, section, live) {
    var q = route.params.q || "";
    if (!q) { section.appendChild(D().notice("Type words to search for in the box at the top.", "info")); return; }
    load(KW.core.apiPath("search", { terms: q }), section, live, function (body) {
      var hits = body.result || [];
      if (!hits.length) { section.appendChild(D().notice("Nothing matches this.", "info")); return; }
      section.appendChild(D().h("ul", { "data-kw": "search-results" }, hits.map(function (h) {
        var s = h.symbol || h;
        return D().h("li", {}, [D().link(s.name, KW.core.buildLink("symbol", { symbol: s.name })), " " + s.kind + " " + s.path]);
      })));
      notes(section, body);
    });
  }

  return { graph: graph, serviceMap: serviceMap, symbol: symbol, search: search, panels: panels, absent: absent, slider: slider };
})();
