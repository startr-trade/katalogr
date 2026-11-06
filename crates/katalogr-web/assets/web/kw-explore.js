/* katalogr web view: the graph explorer, drawn with Cytoscape from the same answer as the
   diagram. The layout's exact coordinates are not a contract. */
var KW = window.KW = window.KW || {};

KW.explore = (function () {
  "use strict";

  var DOUBLE_TAP_MS = 400;

  function cssVar(name, fallback) {
    try {
      if (typeof getComputedStyle === "function") {
        var v = getComputedStyle(document.documentElement).getPropertyValue(name);
        if (v && String(v).trim()) { return String(v).trim(); }
      }
    } catch (e) { /* fall through to the default */ }
    return fallback;
  }

  /* The stylesheet, with colours read from the theme's variables now. */
  function buildStyle() {
    var fg = cssVar("--fg", "#000000");
    var link = cssVar("--links", "#20609f");
    var line = cssVar("--table-border-color", "#999999");
    var fill = cssVar("--quote-bg", "#eff6f9");
    var active = cssVar("--sidebar-active", "#1f1fff");
    return [
      { selector: "node", style: {
        "label": "data(label)", "color": fg, "background-color": fill, "border-width": 1,
        "border-color": link, "font-size": 10, "text-valign": "center", "text-halign": "center",
        "shape": "round-rectangle", "width": "label", "height": "label", "padding": "6px"
      } },
      { selector: "node:parent", style: {
        "text-valign": "top", "background-opacity": 0.3, "border-color": line, "padding": "12px"
      } },
      { selector: "edge", style: {
        "width": 1, "line-color": line, "target-arrow-color": line,
        "target-arrow-shape": "triangle", "curve-style": "bezier"
      } },
      { selector: "edge.inferred", style: { "line-style": "dashed" } },
      { selector: "node.highlight", style: { "border-width": 3, "border-color": active } },
      { selector: "node:selected", style: { "border-width": 3, "border-color": active } }
    ];
  }

  /* The label of a node: its own, else the last two segments of its name. */
  function labelOf(node) {
    if (node.label) { return node.label; }
    var parts = String(node.name || node.id || "").split("/");
    return parts.slice(-2).join("/");
  }

  /* ViewGraph to cytoscape elements. A group becomes a compound parent. `highlight` marks the
     node whose name or label equals it. */
  function toElements(graph, highlight) {
    var els = [];
    var groups = {};
    var nodes = (graph && graph.nodes) || [];
    nodes.forEach(function (n) {
      if (n.group && !groups[n.group]) {
        groups[n.group] = true;
        els.push({ group: "nodes", data: { id: "group:" + n.group, label: n.group } });
      }
    });
    nodes.forEach(function (n) {
      var data = { id: n.id, label: labelOf(n), name: n.name, kind: n.kind, path: n.path || "" };
      if (n.group) { data.parent = "group:" + n.group; }
      var el = { group: "nodes", data: data };
      if (highlight && (n.name === highlight || n.label === highlight)) { el.classes = "highlight"; }
      els.push(el);
    });
    ((graph && graph.edges) || []).forEach(function (e, i) {
      var el = { group: "edges", data: { id: "edge:" + e.from + ">" + e.to + ":" + e.kind + ":" + i,
        source: e.from, target: e.to, kind: e.kind, inferred: !!e.inferred } };
      if (e.inferred) { el.classes = "inferred"; }
      els.push(el);
    });
    return els;
  }

  function layoutOptions() {
    return { name: "breadthfirst", directed: true, animate: false };
  }

  /* Opens the explorer in `host`. opts: {highlight, get}. `get(pathAndQuery, callback(status,
     bodyText))` is the page's transport; a double tap on a node uses it to merge that node's
     callers one hop out. Returns {cy, restyle} or null when the library failed to start. */
  function open(host, graph, opts) {
    opts = opts || {};
    host.replaceChildren();
    var canvas = KW.dom.h("div", { "class": "kw-explore", "data-kw": "explore" }, []);
    canvas.style.height = "480px";
    var detail = KW.dom.h("div", { "class": "kw-explore-detail", "data-kw": "explore-detail" }, []);
    host.appendChild(canvas);
    host.appendChild(detail);
    var cy;
    try {
      cy = cytoscape({
        container: canvas,
        elements: toElements(graph, opts.highlight),
        layout: layoutOptions(),
        style: buildStyle()
      });
    } catch (e) {
      host.replaceChildren();
      host.appendChild(KW.dom.notice("The explorer could not start: " + (e && e.message ? e.message : String(e)), "error"));
      return null;
    }

    function show(data) {
      var kids = [KW.dom.h("strong", {}, [data.name || data.label || data.id])];
      if (data.kind) { kids.push(" (" + data.kind + ")"); }
      if (data.path) { kids.push(" " + data.path); }
      if (data.name) {
        kids.push(" ");
        kids.push(KW.dom.link("Symbol page", KW.core.buildLink("symbol", { symbol: data.name }), { "data-kw": "explore-symbol" }));
      }
      detail.replaceChildren.apply(detail, [KW.dom.h("p", {}, kids)]);
    }

    function expand(data) {
      if (typeof opts.get !== "function" || !data.name) { return; }
      var path = KW.core.apiPath("views/callers", { symbol: data.name, depth: 1 });
      opts.get(path, function (status, body) {
        if (status !== 200) {
          var m = KW.core.statusMessage(status, body);
          detail.replaceChildren(KW.dom.notice(m.text + (m.detail ? " " + m.detail : ""), "warn", m.items));
          return;
        }
        var answer;
        try { answer = JSON.parse(body); } catch (err) { return; }
        var fresh = toElements(answer.graph, null).filter(function (el) {
          return el.group === "edges" ? true : cy.getElementById(el.data.id).empty();
        });
        cy.add(fresh);
        cy.layout(layoutOptions()).run();
      });
    }

    var lastTap = { id: null, at: 0 };
    cy.on("tap", "node", function (evt) {
      var data = evt.target.data();
      var now = Date.now();
      if (lastTap.id === data.id && now - lastTap.at < DOUBLE_TAP_MS) {
        lastTap = { id: null, at: 0 };
        expand(data);
      } else {
        lastTap = { id: data.id, at: now };
        show(data);
      }
    });

    function restyle() {
      cy.style().fromJson(buildStyle()).update();
    }
    return { cy: cy, restyle: restyle };
  }

  return { open: open, toElements: toElements };
})();
