import { d } from "./impl";
export { a, b as c } from "./impl";
export * from "./more";
export * as ns from "./more";
export { d };
export { local as renamed };

function local() {}
