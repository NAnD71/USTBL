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

test("checkpoint and semantic labels map every backend value", () => {
  assert.equal(
    helpers.checkpointLabelKey("before_launch"),
    "checkpoint.before_launch"
  );
  assert.equal(
    helpers.checkpointLabelKey("manual_scan"),
    "checkpoint.manual_scan"
  );
  assert.equal(
    helpers.semanticLabelKey("during_runtime"),
    "semantic.during_runtime"
  );
  assert.equal(
    helpers.semanticLabelKey("ambiguous"),
    "semantic.ambiguous"
  );
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
  assert.equal(helpers.formatHistoryBytes(1024), "1.00 KB");
  assert.equal(helpers.formatHistoryBytes(1024 * 1024), "1.00 MB");
});
