/* katalogr web view: the pure helpers. Nothing here touches the DOM, so the same code runs in a
   browser, under the test harness and inlined in a single file. */
var KW = window.KW = window.KW || {};

KW.core = (function () {
  "use strict";

  /* The exact sentences of the page. */
  var messages = {
    transport: "katalogr isn't answering. Is katalogr serve still running?",
    positionUnavailable: "This position isn't held here. Held positions:",
    ambiguous: "Several symbols match. Choose one:",
    notFound: "Nothing matches this.",
    problem: "katalogr reported a problem:",
    catchingUp: "The index is still catching up, so this may not match the current code.",
    noRequirements: "No requirements are linked to this symbol. They appear when a project's board is indexed.",
    noTests: "No tests are linked to this symbol.",
    nothingStored: "Nothing is stored for this view yet, so there is no history to show. Stored points appear after a view is answered exactly, or when a snapshot folder is given with --snapshots.",
    unknownView: "There is no such view.",
    /* Only a single-file export (kw-static.js) produces these two. */
    notInExport: "This isn't in this export. Run katalogr serve on the project to see it, or export again with a larger --max-symbols.",
    fixedPosition: "This file is fixed at one position, so there is no history to show."
  };

  function decode(s) {
    try {
      return decodeURIComponent(String(s).replace(/\+/g, " "));
    } catch (e) {
      return String(s);
    }
  }

  function sortedNames(params) {
    return Object.keys(params || {}).filter(function (k) {
      var v = params[k];
      return v !== undefined && v !== null && v !== "";
    }).sort();
  }

  function query(params) {
    return sortedNames(params).map(function (k) {
      return encodeURIComponent(k) + "=" + encodeURIComponent(String(params[k]));
    }).join("&");
  }

  /* "#/graph?at=A&symbol=B" gives {view: "graph", params: {at: "A", symbol: "B"}}. An empty
     hash gives an empty view, and the caller defaults it. Unknown parameters are kept. */
  function parseRoute(hash) {
    var text = String(hash || "").replace(/^#/, "").replace(/^\//, "");
    var cut = text.indexOf("?");
    var path = cut < 0 ? text : text.slice(0, cut);
    var qs = cut < 0 ? "" : text.slice(cut + 1);
    var params = {};
    if (qs) {
      qs.split("&").forEach(function (pair) {
        if (!pair) { return; }
        var eq = pair.indexOf("=");
        var name = decode(eq < 0 ? pair : pair.slice(0, eq));
        if (name) { params[name] = eq < 0 ? "" : decode(pair.slice(eq + 1)); }
      });
    }
    return { view: decode(path.split("/")[0]), params: params };
  }

  /* The inverse of parseRoute: names sorted, empty values left out, values percent-encoded. */
  function buildLink(view, params) {
    var q = query(params);
    return "#/" + view + (q ? "?" + q : "");
  }

  /* "/api/v1/<op>?<names sorted>" for an op such as "views/callers". */
  function apiPath(op, params) {
    var q = query(params);
    return "/api/v1/" + op + (q ? "?" + q : "");
  }

  /* "absent" when the row needs a capability this server lacks, else "ready". Used for views
     and for panels. */
  function viewState(row, capabilities) {
    if (row && row.needs && (capabilities || []).indexOf(row.needs) < 0) { return "absent"; }
    return "ready";
  }

  function positionText(p) {
    var commit = p && p.commit ? String(p.commit).slice(0, 12) : "none";
    return (p && p.repo ? p.repo + "@" : "") + commit + (p && p.dirty ? "+" + String(p.dirty).slice(0, 8) : "");
  }

  function parseBody(body) {
    if (body && typeof body === "object") { return body; }
    try { return JSON.parse(body); } catch (e) { return null; }
  }

  /* What to say about a failed request: {text, detail, items}. A status of 0 (or none) means
     the transport failed. `items` lists held positions or candidate symbols, as plain text. */
  function statusMessage(status, body) {
    if (!status && body === "not-in-export") { return { text: messages.notInExport, detail: "", items: [] }; }
    if (!status) { return { text: messages.transport, detail: "", items: [] }; }
    var b = parseBody(body) || {};
    var detail = typeof b.message === "string" ? b.message : "";
    if (status === 409 && b.error === "position-unavailable") {
      return { text: messages.positionUnavailable, detail: "", items: (b.available || []).map(positionText) };
    }
    if (status === 409) {
      var hits = Array.isArray(b.result) ? b.result : [];
      return {
        text: messages.ambiguous,
        detail: "",
        items: hits.map(function (h) { return h.name || ""; }).filter(Boolean)
      };
    }
    if (status === 404) { return { text: messages.notFound, detail: detail, items: [] }; }
    return { text: messages.problem, detail: detail, items: [] };
  }

  /* Plain-language notes under an answer: not exact or stale, and cut. */
  function answerNotes(answer) {
    var notes = [];
    if (!answer) { return notes; }
    if (answer.exact === false || answer.stale === true) { notes.push(messages.catchingUp); }
    var cut = Array.isArray(answer.partial) ? answer.partial[0] : answer.partial;
    var shown = cut && cut.shown !== undefined ? cut.shown : answer.shown;
    var total = cut && cut.total !== undefined ? cut.total : answer.total;
    if (cut && total === undefined) {
      notes.push("Showing " + shown + " of more than " + shown + ".");
    } else if (total !== undefined && shown !== undefined && total > shown) {
      notes.push("Showing " + shown + " of " + total + ".");
    }
    return notes;
  }

  /* The text of one table cell: walks a dotted path; an array is joined with ", ", taking
     `column.join` from each element when that is set. Missing is the empty string. */
  function cell(row, column) {
    var v = row;
    var parts = String(column.path || "").split(".");
    for (var i = 0; i < parts.length; i++) {
      if (v === null || v === undefined) { return ""; }
      v = v[parts[i]];
    }
    if (v === null || v === undefined) { return ""; }
    if (Array.isArray(v)) {
      return v.map(function (e) {
        var x = column.join && e !== null && typeof e === "object" ? e[column.join] : e;
        return x === null || x === undefined ? "" : typeof x === "object" ? JSON.stringify(x) : String(x);
      }).join(", ");
    }
    return typeof v === "object" ? JSON.stringify(v) : String(v);
  }

  /* Two view questions are the same when the op and every argument agree. */
  function sameQuery(a, b) {
    if (!a || !b || a.op !== b.op) { return false; }
    var x = a.args || {}, y = b.args || {};
    var kx = Object.keys(x).sort(), ky = Object.keys(y).sort();
    if (kx.length !== ky.length) { return false; }
    for (var i = 0; i < kx.length; i++) {
      if (kx[i] !== ky[i] || String(x[kx[i]]) !== String(y[ky[i]])) { return false; }
    }
    return true;
  }

  /* The points of a timeline answer (the envelope, or just {points}) that ask this question,
     in the order the server gave them. */
  function pointsFor(timeline, query) {
    var t = timeline && timeline.result ? timeline.result : timeline;
    var points = (t && t.points) || [];
    return points.filter(function (p) { return sameQuery(p.query, query); });
  }

  /* The header line of a single-file export: "Positions:" and each repo with its short commit. */
  function positionsLine(positions) {
    return "Positions: " + (positions || []).map(positionText).join(", ");
  }

  /* The note for the symbols an export cut: its `Partial` entries with at "export.symbols". */
  function exportCutNotes(partial) {
    return (partial || []).filter(function (p) { return p && p.at === "export.symbols"; }).map(function (p) {
      return "This export holds the " + p.shown + " most connected of " + p.total + " symbols.";
    });
  }

  /* The requirements of a symbol: the sorted, de-duplicated union of the tests' requirements,
     the refs-item targets of the rationale and the items of the commits. Each entry is
     {name, label}; the label is the rationale's, or empty. */
  function requirementsOf(whyBody, testsBody) {
    var found = {};
    function add(name, label) {
      if (!name) { return; }
      if (!found[name] || (!found[name].label && label)) { found[name] = { name: name, label: label || "" }; }
    }
    var tests = (testsBody && testsBody.result) || [];
    tests.forEach(function (t) { (t.requirements || []).forEach(function (r) { add(r, ""); }); });
    var why = (whyBody && whyBody.result) || {};
    (why.rationale || []).forEach(function (r) { if (r.kind === "refs-item") { add(r.target, r.label); } });
    (why.commits || []).forEach(function (c) { (c.items || []).forEach(function (i) { add(i, ""); }); });
    return Object.keys(found).sort().map(function (k) { return found[k]; });
  }

  return {
    messages: messages,
    parseRoute: parseRoute,
    buildLink: buildLink,
    apiPath: apiPath,
    viewState: viewState,
    statusMessage: statusMessage,
    answerNotes: answerNotes,
    cell: cell,
    sameQuery: sameQuery,
    pointsFor: pointsFor,
    requirementsOf: requirementsOf,
    positionsLine: positionsLine,
    exportCutNotes: exportCutNotes
  };
})();
