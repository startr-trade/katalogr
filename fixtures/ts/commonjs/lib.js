function local() {}

function other() {}

exports.a = function () {};
exports.b = 42;
module.exports.c = () => other();
module.exports = { local, d: other, e() {}, f: 7 };
