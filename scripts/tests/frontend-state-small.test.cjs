const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

const partialExports = {};
new Function(
  "exports",
  ts.transpileModule(read("src/utils/partial.ts"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText
)(partialExports);

test("a key-path update deep-copies nested state", () => {
  const current = { spec: { memory: 1024, nested: { keep: true } } };
  const next = partialExports.updatedByKeyPath(current, "spec.memory", 2048);

  assert.deepEqual(next, { spec: { memory: 2048, nested: { keep: true } } });
  assert.equal(current.spec.memory, 1024);
  assert.notEqual(next.spec, current.spec);
  assert.notEqual(next.spec.nested, current.spec.nested);
});

test("the second shaderpack accordion reads and writes index one", () => {
  const source = read("src/pages/instances/details/[id]/shaderpacks.tsx");
  const section = source.slice(
    source.indexOf('title={t("InstanceShaderPacksPage.shaderPackList.title")}')
  );
  assert.match(section, /initialIsOpen=\{accordionStates\[1\]\}/);
  assert.match(section, /toSpliced\(1, 1, isOpen\)/);
});

test("world-list failure clears worlds, not resource packs", () => {
  const source = read("src/contexts/instance.tsx");
  const worldReader = source.slice(
    source.indexOf("const handleRetrieveWorldList"),
    source.indexOf("const handleRetrieveLocalModList")
  );
  assert.match(worldReader, /setWorlds\(\[\]\)/);
  assert.doesNotMatch(worldReader, /setResourcePacks/);
});

test("every resource reader uses an instance-and-generation check", () => {
  const source = read("src/contexts/instance.tsx");
  assert.equal((source.match(/beginResourceRequest\("/g) || []).length, 7);
  assert.equal((source.match(/isCurrentResourceRequest\("/g) || []).length, 7);
  assert.match(source, /generation !== configRequest\.current/);
});
