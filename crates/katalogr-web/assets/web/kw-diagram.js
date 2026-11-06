/* katalogr web view: drawing a Mermaid diagram with Pele. */
var KW = window.KW = window.KW || {};

KW.diagram = (function () {
  "use strict";

  /* Draws `mermaid` into `host`. When Pele throws, the host shows the source and the error
     instead, in pre.katalogr-error. Returns true when the diagram was drawn. */
  function draw(host, mermaid) {
    var source = String(mermaid === undefined || mermaid === null ? "" : mermaid);
    host.replaceChildren();
    var drawn;
    try {
      drawn = pele.render(source);
    } catch (e) {
      var message = e && e.message ? e.message : String(e);
      var box = KW.dom.h("pre", { "class": "katalogr-error", "data-kw": "diagram-error" }, [
        KW.dom.h("span", { "class": "katalogr-error-message" }, ["Diagram could not be drawn: " + message]),
        "\n",
        KW.dom.h("code", { "class": "katalogr-error-source" }, [source])
      ]);
      host.appendChild(box);
      return false;
    }
    var div = KW.dom.h("div", { "class": "diagram", "data-kw": "diagram" }, []);
    KW.dom.setSvg(div, drawn.svg);
    host.appendChild(div);
    return true;
  }

  return { draw: draw };
})();
