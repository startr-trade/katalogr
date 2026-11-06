// Stand-ins for the two vendored libraries. Their real rendering is proved by tools/web-smoke.sh
// in a browser, and Pele's by `pele_check`. Here they only record what they were asked to draw.
var __pele = [];
var __cy = [];
window.pele = {
  render: function (source) {
    __pele.push(String(source));
    if (String(source).indexOf("FAILS") >= 0) throw new Error("pele: cannot draw FAILS");
    return { svg: "<svg></svg>" };
  }
};
var __cyHandlers = [];
var __cyAdded = [];
var __cyIds = {};
// Fires the handlers registered for `event` on a node with this data, as a tap would.
function __cyFire(event, data) {
  var node = { data: function (k) { return k === undefined ? data : data[k]; }, id: function () { return data.id; } };
  __cyHandlers.filter(function (h) { return h.event === event; }).forEach(function (h) { h.fn({ target: node }); });
}
window.cytoscape = function (opts) {
  var els = opts && opts.elements ? opts.elements : [];
  __cy.push(els);
  els.forEach(function (e) { __cyIds[e.data.id] = true; });
  var api = {
    on: function (event, a, b) { __cyHandlers.push({ event: event, fn: typeof a === "function" ? a : b }); return api; },
    one: function () { return api; },
    layout: function () { return { run: function () {} }; },
    destroy: function () {},
    resize: function () {},
    fit: function () {},
    style: function () { var s = { fromJson: function () { return s; }, update: function () {} }; return s; },
    nodes: function () { return []; },
    edges: function () { return []; },
    elements: function () { return []; },
    getElementById: function (id) { return { empty: function () { return !__cyIds[id]; } }; },
    add: function (more) { __cyAdded.push(more); more.forEach(function (e) { __cyIds[e.data.id] = true; }); return api; }
  };
  return api;
};

// The harness starts the page itself, after setting the hash.
window.KW_MANUAL_BOOT = true;
