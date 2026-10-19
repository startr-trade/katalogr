import { remote, def as renamed } from "./b";
import * as b from "./b";
import other from "./b";

function local() {}

export class Svc {
  run() {
    local();
    remote();
    this.helper();
    b.ns();
  }

  helper() {}
}

export function entry(svc: Svc) {
  local();
  renamed();
  other();
  svc.run();
  console.log(1);
}

local();
