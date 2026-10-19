import { run_engine } from "@acme/engine-core";
import * as gen from "./gen/engine.js";

export function start(): number {
  gen.stop_engine();
  return run_engine();
}
