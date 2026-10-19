const m = require("./m");
const { f, g: h } = require("./m");
const one = require("./m").f;
require("./side");

function go() {
  m.f();
  f();
  h();
  one();
  return require("./late");
}
