// A fake DOM, just enough for the web view's own scripts. Nothing is parsed or laid out.
// Requests and timers are queued here; Rust drains them, so no closure crosses into the engine.
var window = globalThis;
window.window = window;
var __markup = [];
var __requests = [];
var __queue = [];
var __timers = [];
var __seq = 0;
var __callbacks = {};
var __byId = {};

function __TextNode(t) { this.nodeType = 3; this.data = String(t); this.parentNode = null; }

function __ClassList(el) { this.el = el; }
__ClassList.prototype.list = function () { return this.el.className ? String(this.el.className).split(/\s+/).filter(Boolean) : []; };
__ClassList.prototype.contains = function (c) { return this.list().indexOf(c) >= 0; };
__ClassList.prototype.add = function () { var l = this.list(); for (var i = 0; i < arguments.length; i++) { if (l.indexOf(arguments[i]) < 0) l.push(arguments[i]); } this.el.className = l.join(" "); };
__ClassList.prototype.remove = function () { var l = this.list(); for (var i = 0; i < arguments.length; i++) { var k = l.indexOf(arguments[i]); if (k >= 0) l.splice(k, 1); } this.el.className = l.join(" "); };
__ClassList.prototype.toggle = function (c, force) { var on = force === undefined ? !this.contains(c) : !!force; if (on) this.add(c); else this.remove(c); return on; };

function __Element(tag, ns) {
  this.nodeType = 1;
  this.tagName = String(tag).toUpperCase();
  this.namespaceURI = ns || null;
  this.children = [];
  this.childNodes = this.children;
  this.parentNode = null;
  this.attributes = {};
  this.listeners = {};
  this.style = {};
  this.className = "";
  this.value = "";
  this.classList = new __ClassList(this);
}
__Element.prototype.appendChild = function (c) {
  if (c.parentNode) c.parentNode.removeChild(c);
  c.parentNode = this; this.children.push(c); return c;
};
__Element.prototype.append = function () {
  for (var i = 0; i < arguments.length; i++) {
    var a = arguments[i];
    this.appendChild(typeof a === "object" && a !== null ? a : new __TextNode(a));
  }
};
__Element.prototype.removeChild = function (c) {
  var k = this.children.indexOf(c); if (k >= 0) { this.children.splice(k, 1); c.parentNode = null; } return c;
};
__Element.prototype.replaceChildren = function () {
  while (this.children.length) this.removeChild(this.children[0]);
  this.append.apply(this, arguments);
};
__Element.prototype.replaceWith = function (n) {
  var p = this.parentNode; if (!p) return;
  var k = p.children.indexOf(this); p.children[k] = n; n.parentNode = p; this.parentNode = null;
};
__Element.prototype.setAttribute = function (n, v) {
  v = String(v); this.attributes[n] = v;
  if (n === "class") this.className = v;
  if (n === "id") __byId[v] = this;
  if (n === "value") this.value = v;
};
__Element.prototype.getAttribute = function (n) {
  if (n === "class") return this.className || null;
  return Object.prototype.hasOwnProperty.call(this.attributes, n) ? this.attributes[n] : null;
};
__Element.prototype.removeAttribute = function (n) { delete this.attributes[n]; };
__Element.prototype.hasAttribute = function (n) { return this.getAttribute(n) !== null; };
__Element.prototype.addEventListener = function (t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); };
__Element.prototype.removeEventListener = function (t, f) {
  var l = this.listeners[t] || []; var k = l.indexOf(f); if (k >= 0) l.splice(k, 1);
};
__Element.prototype.dispatchEvent = function (ev) {
  ev.target = ev.target || this;
  var l = (this.listeners[ev.type] || []).slice();
  for (var i = 0; i < l.length; i++) l[i].call(this, ev);
  return true;
};
__Element.prototype.focus = function () {};
__Element.prototype.collectText = function (out) {
  for (var i = 0; i < this.children.length; i++) {
    var c = this.children[i];
    if (c.nodeType === 3) out.push(c.data); else c.collectText(out);
  }
  return out;
};
Object.defineProperty(__Element.prototype, "textContent", {
  get: function () { return this.collectText([]).join(""); },
  set: function (t) { this.replaceChildren(); if (String(t) !== "") this.appendChild(new __TextNode(t)); }
});
Object.defineProperty(__Element.prototype, "innerHTML", {
  get: function () { return this._html || ""; },
  set: function (h) { this._html = String(h); __markup.push(String(h)); this.replaceChildren(); }
});
Object.defineProperty(__Element.prototype, "id", {
  get: function () { return this.attributes.id || ""; },
  set: function (v) { this.setAttribute("id", v); }
});
Object.defineProperty(__Element.prototype, "firstChild", { get: function () { return this.children[0] || null; } });

function __walk(root, f) {
  for (var i = 0; i < root.children.length; i++) {
    var c = root.children[i];
    if (c.nodeType === 1) { if (f(c)) return c; var r = __walk(c, f); if (r) return r; }
  }
  return null;
}
function __all(root, f) {
  var out = []; __walk(root, function (e) { if (f(e)) out.push(e); return false; }); return out;
}
// Simple selectors only: #id, .class, tag, [attr], [attr="v"], and tag with one of those.
function __matcher(sel) {
  var m = /^([a-zA-Z0-9]*)(?:#([\w-]+)|\.([\w-]+)|\[([\w-]+)(?:=["']?([^"'\]]*)["']?)?\])?$/.exec(sel);
  if (!m) throw new Error("fake DOM: unsupported selector " + sel);
  return function (e) {
    if (m[1] && e.tagName !== m[1].toUpperCase()) return false;
    if (m[2] && e.id !== m[2]) return false;
    if (m[3] && !e.classList.contains(m[3])) return false;
    if (m[4]) { var v = e.getAttribute(m[4]); if (v === null) return false; if (m[5] !== undefined && v !== m[5]) return false; }
    return true;
  };
}
__Element.prototype.querySelector = function (s) { return __walk(this, __matcher(s)); };
__Element.prototype.querySelectorAll = function (s) { return __all(this, __matcher(s)); };

var document = window.document = new __Element("#document");
document.createElement = function (t) { return new __Element(t); };
document.createElementNS = function (ns, t) { return new __Element(t, ns); };
document.createTextNode = function (t) { return new __TextNode(t); };
document.getElementById = function (id) { return __byId[id] || null; };
document.documentElement = new __Element("html");
document.body = new __Element("body");
document.documentElement.appendChild(document.body);
document.appendChild(document.documentElement);
document.documentElement.setAttribute("class", "light");

function __mk(tag, id, parent) { var e = document.createElement(tag); e.setAttribute("id", id); (parent || document.body).appendChild(e); return e; }
var __app = __mk("div", "kw-app");
__mk("h1", "kw-title", __app);
__mk("input", "kw-search", __app).setAttribute("data-kw", "search-input");
__mk("button", "kw-theme", __app).setAttribute("data-kw", "theme-button");
__mk("nav", "kw-sidebar", __app);
__mk("main", "kw-main", __app);
__mk("script", "kw-views");
__mk("script", "kw-data");
var __meta = document.createElement("meta");
__meta.setAttribute("name", "katalogr-api"); __meta.setAttribute("content", "");
document.body.appendChild(__meta);

var location = window.location = { hash: "", href: "http://localhost/", origin: "http://localhost" };
var __winListeners = {};
window.addEventListener = function (t, f) { (__winListeners[t] = __winListeners[t] || []).push(f); };
window.removeEventListener = function (t, f) { var l = __winListeners[t] || []; var k = l.indexOf(f); if (k >= 0) l.splice(k, 1); };
window.dispatchEvent = function (ev) { var l = (__winListeners[ev.type] || []).slice(); for (var i = 0; i < l.length; i++) l[i].call(window, ev); };
var __store = {};
window.localStorage = {
  getItem: function (k) { return Object.prototype.hasOwnProperty.call(__store, k) ? __store[k] : null; },
  setItem: function (k, v) { __store[k] = String(v); },
  removeItem: function (k) { delete __store[k]; }
};
window.matchMedia = function () { return { matches: false, addEventListener: function () {}, addListener: function () {} }; };
window.setTimeout = function (f, ms) { __timers.push({ f: f, ms: ms || 0 }); return __timers.length; };
window.clearTimeout = function () {};
window.requestAnimationFrame = function (f) { return window.setTimeout(f, 0); };
window.console = window.console || { log: function () {}, warn: function () {}, error: function () {} };

// Rust-facing helpers.
function __setHash(h) { location.hash = h; }
function __fireHashChange() { window.dispatchEvent({ type: "hashchange" }); }
function __takeQueue() {
  var q = __queue; __queue = [];
  return JSON.stringify(q.map(function (r) { return { id: r.id, path: r.path }; }));
}
function __deliver(id, status, body) { var cb = __callbacks[id]; delete __callbacks[id]; cb(status, body); }
function __runTimers() {
  var t = __timers; __timers = [];
  for (var i = 0; i < t.length; i++) t[i].f();
  return t.length;
}
function __mainText() { return __byId["kw-main"].collectText([]).join("\n"); }
function __byKw(name) { return __walk(document, function (e) { return e.getAttribute("data-kw") === name; }); }
function __fire(name, event, value) {
  var el = __byKw(name);
  if (!el) throw new Error("no element with data-kw=" + name);
  if (value !== null && value !== undefined) el.value = value;
  el.dispatchEvent({ type: event, target: el, preventDefault: function () {}, stopPropagation: function () {} });
}
window.KW_TRANSPORT = function (path, cb) {
  var id = ++__seq; __callbacks[id] = cb; __requests.push(path); __queue.push({ id: id, path: path });
};
