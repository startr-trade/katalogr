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
window.cytoscape = function (opts) {
  __cy.push(opts && opts.elements ? opts.elements : []);
  var api = {
    on: function () { return api; },
    one: function () { return api; },
    layout: function () { return { run: function () {} }; },
    destroy: function () {},
    resize: function () {},
    fit: function () {},
    nodes: function () { return []; },
    edges: function () { return []; },
    elements: function () { return []; },
    getElementById: function () { return { empty: function () { return true; } }; },
    add: function () { return api; }
  };
  return api;
};
