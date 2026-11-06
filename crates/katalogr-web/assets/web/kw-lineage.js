/* katalogr web view: type lineage. Asks for a subject, then draws what the lineage answer holds:
   where it is declared, its fields, related types, what binds to it, code, contracts and where
   the answer stops. Every string from the graph goes through KW.dom, as text. */
var KW = window.KW = window.KW || {};

KW.lineage = (function () {
  "use strict";

  var D = function () { return KW.dom; };
  var VIEW = "type-lineage";

  /* The words for each boundary code. */
  var BOUNDARY = {
    "no-contract": "This binds no contract, so only direct references to it can show.",
    "no-fields": "No fields are recorded for this type. If it is a code type, the project may have turned fields off with extract.code_fields.",
    "owner-level": "This is a field. Bindings below are on the type that holds it, unless they name the field.",
    "no-code": "No code is linked. Code appears when it reaches this through a contract, for example by a project's profile.",
    "open-contract": "A contract here has nothing on one side in this project, so it may continue elsewhere."
  };

  var OPEN = {
    "no-consumer": "nothing else in this project uses it",
    "no-provider": "nothing in this project provides it"
  };

  function boundaryText(code) { return BOUNDARY[code] || String(code); }

  function lineageLink(name, text) {
    return D().link(text || name, KW.core.buildLink(VIEW, { symbol: name }));
  }

  function symbolLink(name) {
    return D().link(name, KW.core.buildLink("symbol", { symbol: name }));
  }

  function leaf(name) {
    var body = String(name).split("#").slice(-1)[0];
    return body.split("/").slice(-1)[0];
  }

  /* No subject: a search field and the results as links to this page. */
  function ask(section, live) {
    section.appendChild(D().notice("Choose a type, schema element or contract to see where it goes.", "info"));
    var input = D().h("input", { type: "search", "data-kw": "lineage-search", "aria-label": "Find a type, element or contract" }, []);
    var results = D().h("div", { "data-kw": "lineage-results" }, []);
    input.addEventListener("change", function () {
      KW.get(KW.core.apiPath("find", { text: input.value }), function (status, body) {
        if (!live()) { return; }
        results.replaceChildren();
        if (status !== 200) {
          var m = KW.core.statusMessage(status, body);
          results.appendChild(D().notice(m.text + (m.detail ? " " + m.detail : ""), "warn"));
          return;
        }
        var hits = [];
        try { hits = JSON.parse(body).result || []; } catch (e) { hits = []; }
        if (!hits.length) { results.appendChild(D().notice(KW.core.messages.notFound, "info")); return; }
        results.appendChild(D().h("ul", {}, hits.map(function (h) {
          return D().h("li", {}, [lineageLink(h.name), " " + h.kind + " " + h.path]);
        })));
      });
    });
    section.appendChild(input);
    section.appendChild(results);
  }

  /* A failed request. Candidates of an ambiguous name link back to this page. */
  function failure(into, status, body) {
    var m = KW.core.statusMessage(status, body);
    var items = m.items.map(function (i) {
      return m.text === KW.core.messages.ambiguous ? lineageLink(i) : i;
    });
    into.appendChild(D().notice(m.text + (m.detail ? " " + m.detail : ""), status === 409 ? "info" : "warn", items));
  }

  function params(route, extra) {
    var out = {};
    Object.keys(route.params).forEach(function (k) { out[k] = route.params[k]; });
    Object.keys(extra || {}).forEach(function (k) { out[k] = extra[k]; });
    return out;
  }

  function heading(into, kw, title) {
    var box = D().h("section", { "data-kw": kw }, [D().h("h3", {}, [title])]);
    into.appendChild(box);
    return box;
  }

  function nameList(items) {
    return D().h("ul", {}, items);
  }

  function groups(into, kw, title, list, route) {
    if (!list || !list.length) { return; }
    var box = heading(into, kw, title);
    list.forEach(function (g) {
      box.appendChild(D().h("h4", {}, [g.kind + ", " + g.node_kind + ", " + String(g.confidence).toLowerCase() + ": " + g.total]));
      box.appendChild(nameList((g.hits || []).map(function (h) {
        var bits = [symbolLink(h.node.name), " depth " + h.depth];
        if (h.via) { bits.push(" via " + h.via); }
        if (h.holder) { bits.push(" in " + h.holder); }
        if (h.role) { bits.push(" (" + h.role + ")"); }
        return D().h("li", {}, bits);
      })));
      if (g.total > (g.hits || []).length) {
        var p = D().h("p", { "data-kw": "lineage-more" }, ["Showing " + (g.hits || []).length + " of " + g.total + ". "]);
        p.appendChild(D().link("Show all", KW.core.buildLink(VIEW, params(route, { expand: "true" })), { "data-kw": "lineage-expand" }));
        box.appendChild(p);
      }
    });
  }

  function fieldRows(fields) {
    return (fields || []).map(function (f) {
      var mapped = (f.mapped_by || []).map(function (m) {
        return m.node.name + " (" + m.kind + ", " + String(m.confidence).toLowerCase() + ")";
      }).concat((f.copies || []).map(function (c) { return c.node.name + " (" + c.by + ")"; }));
      return {
        name: new Array(f.depth).join("  ") + leaf(f.node.name),
        type: f.field_type === null || f.field_type === undefined ? "" : f.field_type,
        required: f.required === null || f.required === undefined ? "" : String(f.required),
        many: f.many === null || f.many === undefined ? "" : String(f.many),
        mapped: mapped
      };
    });
  }

  function draw(route, section, live, h) {
    var symbol = route.params.symbol;
    if (!symbol) { ask(section, live); return; }
    var wait = KW.state.manifest.wait_seconds;
    var q = { consistency: "wait", text: symbol, timeout: wait };
    if (route.params.at) { q.at = route.params.at; }
    if (route.params.expand === "true") { q.expand = "true"; }
    var vq = { consistency: "wait", symbol: symbol, timeout: wait };
    if (route.params.at) { vq.at = route.params.at; }
    var host = D().h("div", { "data-kw": "lineage-diagram" }, []);
    var body = D().h("div", { "data-kw": "lineage-body" }, []);
    section.appendChild(body);
    KW.get(KW.core.apiPath("lineage", q), function (status, text) {
      if (!live()) { return; }
      if (status !== 200) { failure(body, status, text); return; }
      var answer;
      try { answer = JSON.parse(text); } catch (e) { failure(body, 500, ""); return; }
      var r = answer.result || {};
      var top = D().h("section", { "data-kw": "lineage-subject" }, [
        D().h("h2", {}, [r.subject.name]),
        D().h("p", {}, [r.subject.kind + " ", D().link("Symbol page", KW.core.buildLink("symbol", { symbol: r.subject.name }), { "data-kw": "lineage-symbol-page" })])]);
      if (r.owner) {
        top.appendChild(D().h("p", { "data-kw": "lineage-owner" }, ["Field of ", lineageLink(r.owner.name)]));
      }
      body.appendChild(top);
      h.notes(body, answer);
      body.appendChild(host);

      if ((r.declared_in || []).length) {
        var d = heading(body, "lineage-declared", "Declared in");
        d.appendChild(nameList(r.declared_in.map(function (x) {
          return D().h("li", {}, [symbolLink(x.name), " " + x.path]);
        })));
      }
      if ((r.fields || []).length) {
        var f = heading(body, "lineage-fields", "Fields");
        f.appendChild(D().table([
          { title: "Field", path: "name" }, { title: "Type", path: "type" },
          { title: "Required", path: "required" }, { title: "Many", path: "many" },
          { title: "Mapped by", path: "mapped" }
        ], fieldRows(r.fields)));
        if ((r.unpaired_fields || []).length) {
          f.appendChild(D().h("p", {}, ["Not paired with a field above:"]));
          f.appendChild(nameList(r.unpaired_fields.map(function (x) { return D().h("li", {}, [x.name]); })));
        }
      }
      if ((r.related || []).length) {
        var rel = heading(body, "lineage-related", "Related types");
        rel.appendChild(nameList(r.related.map(function (x) {
          var bits = [x.relation + " ", lineageLink(x.node.name)];
          if (x.through_field) { bits.push(" through " + x.through_field); }
          return D().h("li", {}, bits);
        })));
      }
      groups(body, "lineage-bindings", "Bindings", r.bindings, route);
      groups(body, "lineage-code", "Code", r.code, route);
      if ((r.contracts || []).length) {
        var c = heading(body, "lineage-contracts", "Contracts");
        c.appendChild(nameList(r.contracts.map(function (x) {
          var t = x.contract + ": " + x.providers + " providers, " + x.consumers + " consumers";
          if (x.open) { t += " - " + (OPEN[x.open] || x.open); }
          return D().h("li", {}, [t]);
        })));
      }
      if ((r.boundaries || []).length) {
        var s = heading(body, "lineage-stops", "Where this stops");
        r.boundaries.forEach(function (code) {
          s.appendChild(D().h("p", { "data-kw": "lineage-stop-" + code }, [boundaryText(code)]));
        });
      }
    });
    var path = route.params.id ?
      KW.core.apiPath("stored-view", { id: route.params.id }) : KW.core.apiPath("views/lineage", vq);
    KW.get(path, function (status, text) {
      if (!live()) { return; }
      if (status !== 200) { return; }
      var answer;
      try { answer = JSON.parse(text); } catch (e) { return; }
      h.diagramAndExplore(host, route, VIEW, answer, live);
    });
  }

  return { boundaryText: boundaryText, draw: draw };
})();
