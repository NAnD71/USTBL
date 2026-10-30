const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function load(file) {
  const exports = {};
  const module = { exports };
  new Function(
    "exports",
    "module",
    "require",
    ts.transpileModule(
      fs.readFileSync(path.join(__dirname, "../../", file), "utf8"),
      {
        compilerOptions: { module: ts.ModuleKind.CommonJS },
      }
    ).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const { mergeExpandedIds } = load("src/utils/tree-expansion.ts");

const tree = (ids) => ids.map((id) => ({ id, data: null, children: [] }));

test("existing expanded nodes remain expanded when nodes change", () => {
  const result = mergeExpandedIds(new Set(["a"]), tree(["a", "b"]), new Set());
  assert.deepEqual([...result], ["a"]);
});

test("new nodes receive the default expansion", () => {
  const result = mergeExpandedIds(
    new Set(["a"]),
    tree(["a", "b"]),
    new Set(["b"])
  );
  assert.deepEqual([...result].sort(), ["a", "b"]);
});

test("additional expansion only adds ids", () => {
  const result = mergeExpandedIds(
    new Set(["a"]),
    tree(["a", "b"]),
    new Set(),
    new Set(["b"])
  );
  assert.deepEqual([...result].sort(), ["a", "b"]);
});

test("an id that disappeared is treated as new when it returns", () => {
  const result = mergeExpandedIds(new Set(["a"]), tree(["a"]), new Set(["a"]));
  assert.deepEqual([...result], ["a"]);
});
