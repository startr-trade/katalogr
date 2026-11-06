/* katalogr web view: the single-file transport. It answers the page's requests from the data
   embedded in the file, so the file has the same views and the same text as the served page and
   asks nothing of any network. It loads first, before the page's own scripts. */
var KW = window.KW = window.KW || {};

KW.static = true;
KW.staticSource = false;

KW.staticData = (function () {
  "use strict";
  var data = null;
  return function () {
    if (data === null) {
      var el = document.getElementById("kw-data");
      try { data = JSON.parse(el ? el.textContent : "{}") || {}; } catch (e) { data = {}; }
    }
    return data;
  };
})();

KW.staticSource = !!KW.staticData().with_source;

window.KW_TRANSPORT = (function () {
  "use strict";
  var PREFIX = "/api/v1/";
  var VOLATILE = { consistency: true, timeout: true, at: true };

  function decode(s) {
    try { return decodeURIComponent(String(s).replace(/\+/g, " ")); } catch (e) { return String(s); }
  }

  /* "/api/v1/op?b=2&a=1&timeout=5" gives {op: "op", pairs: ["a=1", "b=2"], values: {a: "1", b: "2"}}:
     the volatile names are removed and the rest kept sorted by name, in their encoded form. */
  function parse(path) {
    var p = String(path);
    if (p.indexOf(PREFIX) === 0) { p = p.slice(PREFIX.length); }
    var cut = p.indexOf("?");
    var op = cut < 0 ? p : p.slice(0, cut);
    var qs = cut < 0 ? "" : p.slice(cut + 1);
    var list = [];
    var values = {};
    qs.split("&").forEach(function (pair) {
      if (!pair) { return; }
      var eq = pair.indexOf("=");
      var name = decode(eq < 0 ? pair : pair.slice(0, eq));
      if (VOLATILE[name] === true) { return; }
      list.push({ name: name, text: pair });
      values[name] = eq < 0 ? "" : decode(pair.slice(eq + 1));
    });
    list.sort(function (a, b) { return a.name < b.name ? -1 : a.name > b.name ? 1 : 0; });
    return { op: op, pairs: list.map(function (x) { return x.text; }), values: values };
  }

  function has(obj, key) { return Object.prototype.hasOwnProperty.call(obj, key); }

  /* The embedded symbols whose name holds every word of `text`, in the file's order. */
  function matching(text) {
    var words = String(text || "").toLowerCase().split(/\s+/).filter(Boolean);
    var symbols = KW.staticData().symbols || [];
    return symbols.filter(function (s) {
      var name = String(s && s.name !== undefined ? s.name : "").toLowerCase();
      return words.every(function (w) { return name.indexOf(w) >= 0; });
    });
  }

  return function (path, cb) {
    var d = KW.staticData();
    var q = parse(path);
    var key = q.op + (q.pairs.length ? "?" + q.pairs.join("&") : "");
    if (key === "meta") {
      cb(200, JSON.stringify({ capabilities: d.capabilities || [], katalogr: d.katalogr || "" }));
    } else if (key === "timeline") {
      cb(200, JSON.stringify({ result: { points: [] } }));
    } else if (q.op === "find" && has(q.values, "text")) {
      cb(200, JSON.stringify({ result: matching(q.values.text) }));
    } else if (q.op === "search" && has(q.values, "terms")) {
      cb(200, JSON.stringify({ result: matching(q.values.terms) }));
    } else if (d.answers && has(d.answers, key)) {
      cb(d.answers[key].status, JSON.stringify(d.answers[key].body));
    } else {
      cb(0, "not-in-export");
    }
  };
})();
