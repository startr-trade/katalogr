import { add } from "./math";

function reset() {}

describe("add", () => {
  beforeEach(() => reset());
  it("sums", () => { add(1, 2); });
  it.each([[1]])("table %i", (n) => add(n, n));
});

test("top", function () { add(0, 0); });
