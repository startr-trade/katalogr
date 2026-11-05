// Renders each Mermaid case with the vendored Pele build. Usage:
//   node pele_check.mjs <pele.global.js> <cases.json>
// cases.json is an object of case name to Mermaid text. Prints one line per failure and exits 1
// if any case throws or draws nothing.
import { readFileSync } from "node:fs";
import vm from "node:vm";

const [, , peleFile, casesFile] = process.argv;
const context = vm.createContext({});
vm.runInContext(readFileSync(peleFile, "utf8") + "\n;globalThis.pele = pele;", context);
const pele = context.pele;
const cases = JSON.parse(readFileSync(casesFile, "utf8"));
let failed = 0;
for (const [name, source] of Object.entries(cases)) {
  try {
    const result = pele.render(source);
    if (!result || typeof result.svg !== "string" || !result.svg.includes("<svg")) {
      throw new Error("no svg in the result");
    }
  } catch (e) {
    failed++;
    console.log(`FAIL ${name}: ${e && e.message ? e.message : e}`);
  }
}
console.log(`${Object.keys(cases).length - failed} of ${Object.keys(cases).length} rendered`);
process.exit(failed ? 1 : 0);
