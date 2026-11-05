/* katalogr: draw every Mermaid code block with Pele. Loaded after pele.global.js. */
(function () {
  "use strict";

  function failed(pre, source, error) {
    var box = document.createElement("pre");
    box.className = "katalogr-error";
    var head = document.createElement("span");
    head.className = "katalogr-error-message";
    head.textContent = "Diagram could not be drawn: " + error;
    var body = document.createElement("code");
    body.className = "katalogr-error-source";
    body.textContent = source;
    box.appendChild(head);
    box.appendChild(document.createTextNode("\n"));
    box.appendChild(body);
    pre.replaceWith(box);
  }

  function draw(code) {
    var pre = code.parentNode;
    var source = code.textContent;
    var host = document.createElement("div");
    host.className = "diagram";
    try {
      var result = pele.render(source);
      host.innerHTML = result.svg;
      pre.replaceWith(host);
    } catch (e) {
      failed(pre, source, e && e.message ? e.message : String(e));
    }
  }

  function run() {
    var blocks = document.querySelectorAll("pre > code.language-mermaid");
    for (var i = 0; i < blocks.length; i++) {
      draw(blocks[i]);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", run);
  } else {
    run();
  }
})();
