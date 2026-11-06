/* katalogr web view: building elements. Text goes in only through textContent. The one place
   that sets markup is setSvg, which takes the SVG the diagram library drew. */
var KW = window.KW = window.KW || {};

KW.dom = (function () {
  "use strict";

  function unsafeUrl(v) {
    return /^\s*(javascript|data|vbscript):/i.test(String(v));
  }

  /* h("a", {class: "x", href: "#/graph", onclick: fn}, ["text", node]). Strings among the
     children become text nodes. An attribute named on* takes a function or is dropped, and a
     link to a script URL is dropped. */
  function h(tag, attrs, children) {
    var el = document.createElement(tag);
    var names = Object.keys(attrs || {});
    for (var i = 0; i < names.length; i++) {
      var name = names[i];
      var value = attrs[name];
      if (value === undefined || value === null || value === false) { continue; }
      if (/^on/i.test(name)) {
        if (typeof value === "function") { el.addEventListener(name.slice(2).toLowerCase(), value); }
        continue;
      }
      if ((name === "href" || name === "src" || name === "action") && unsafeUrl(value)) { continue; }
      if (name === "class") { el.className = String(value); continue; }
      if (name === "text") { el.textContent = String(value); continue; }
      el.setAttribute(name, value === true ? "" : String(value));
    }
    var list = children === undefined || children === null ? [] : [].concat(children);
    for (var j = 0; j < list.length; j++) {
      var c = list[j];
      if (c === undefined || c === null || c === false) { continue; }
      el.appendChild(typeof c === "object" ? c : document.createTextNode(String(c)));
    }
    return el;
  }

  /* Replaces the content of `el` with an SVG document drawn by the diagram library. */
  function setSvg(el, svg) {
    el.innerHTML = String(svg);
  }

  function link(text, href, attrs) {
    var a = {};
    var names = Object.keys(attrs || {});
    for (var i = 0; i < names.length; i++) { a[names[i]] = attrs[names[i]]; }
    a.href = href;
    return h("a", a, [String(text)]);
  }

  /* A paragraph of plain-language status. kind is "info" (default), "warn" or "error". */
  function notice(text, kind, items) {
    var kids = [String(text)];
    if (items && items.length) {
      kids.push(h("ul", {}, items.map(function (i) {
        return h("li", {}, [typeof i === "object" ? i : String(i)]);
      })));
    }
    return h("div", { "class": "kw-notice kw-" + (kind || "info"), "data-kw": "note" }, kids);
  }

  /* A table from manifest columns and rows. A column with symbol_link links its text to the
     symbol page. */
  function table(columns, rows) {
    var head = h("tr", {}, columns.map(function (c) { return h("th", {}, [c.title]); }));
    var body = rows.map(function (row) {
      return h("tr", {}, columns.map(function (c) {
        var text = KW.core.cell(row, c);
        var inner = c.symbol_link && text ?
          link(text, KW.core.buildLink("symbol", { symbol: text })) : text;
        return h("td", {}, [inner]);
      }));
    });
    return h("table", {}, [h("thead", {}, [head]), h("tbody", {}, body)]);
  }

  return { h: h, setSvg: setSvg, link: link, notice: notice, table: table };
})();
