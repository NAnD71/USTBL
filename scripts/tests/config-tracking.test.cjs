const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file) {
  const module = { exports: {} };
  new Function(
    "exports",
    "module",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText
  )(module.exports, module);
  return module.exports;
}

const helpers = load("src/utils/config-tracking.ts");

test("all checkpoint labels map to locale keys", () => {
  for (const checkpoint of [
    "before_launch",
    "launcher_ready",
    "after_exit",
    "manual_scan",
  ]) {
    assert.equal(
      helpers.checkpointLabelKey(checkpoint),
      `checkpoint.${checkpoint}`
    );
  }
});

test("all semantic labels map to locale keys", () => {
  for (const semantic of [
    "initial_observation",
    "during_loading",
    "during_runtime",
    "between_sessions",
    "interrupted_or_unknown",
    "ambiguous",
  ]) {
    assert.equal(helpers.semanticLabelKey(semantic), `semantic.${semantic}`);
  }
});

test("change counts are formatted without changing their values", () => {
  assert.deepEqual(
    helpers.formatChangeCounts({
      added: 1,
      modified: 1200,
      deleted: 0,
      uncertain: 3,
    }),
    { added: "1", modified: "1,200", deleted: "0", uncertain: "3" }
  );
});

test("history storage sums log and object bytes and formats binary units", () => {
  assert.equal(helpers.totalHistoryBytes(12, 34), 46);
  assert.equal(helpers.formatHistoryBytes(0), "0 B");
  assert.equal(helpers.formatHistoryBytes(-1), "0 B");
  assert.equal(helpers.formatHistoryBytes(1024), "1.00 KB");
  assert.equal(helpers.formatHistoryBytes(1024 * 1024), "1.00 MB");
  assert.equal(helpers.formatHistoryBytes(1024 ** 3), "1.00 GB");
  assert.equal(helpers.formatHistoryBytes(1024 ** 3 + 1), "1.00 GB");
});

test("latest request gate ignores reverse-order responses", () => {
  const gate = helpers.createLatestRequestGate();
  const first = gate.begin();
  const second = gate.begin();
  assert.equal(gate.isCurrent(first), false);
  assert.equal(gate.isCurrent(second), true);
  gate.dispose();
  assert.equal(gate.isCurrent(second), false);
});
