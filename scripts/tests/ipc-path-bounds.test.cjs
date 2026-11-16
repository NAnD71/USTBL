const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.join(__dirname, "../..");

function sourceFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(file);
    return /\.(ts|tsx)$/.test(entry.name) ? [file] : [];
  });
}

test("the frontend has no unrestricted plugin-fs or openPath entry point", () => {
  for (const file of sourceFiles(path.join(root, "src"))) {
    const source = fs.readFileSync(file, "utf8");
    assert.doesNotMatch(source, /@tauri-apps\/plugin-fs/, file);
    assert.doesNotMatch(source, /\bopenPath\b/, file);
  }
});

test("desktop capabilities keep URL opening and reveal, but no global path access", () => {
  const capability = JSON.parse(
    fs.readFileSync(
      path.join(root, "src-tauri/capabilities/desktop.json"),
      "utf8"
    )
  );
  assert.ok(capability.permissions.includes("opener:default"));
  assert.ok(!capability.permissions.includes("fs:default"));
  for (const permission of capability.permissions) {
    if (typeof permission === "string") continue;
    assert.notEqual(permission.identifier, "fs:scope");
    assert.notEqual(permission.identifier, "opener:allow-open-path");
  }
});

test("the unused arbitrary recursive-directory deletion command stays removed", () => {
  const lib = fs.readFileSync(path.join(root, "src-tauri/src/lib.rs"), "utf8");
  const utils = fs.readFileSync(
    path.join(root, "src-tauri/src/utils/commands.rs"),
    "utf8"
  );
  const service = fs.readFileSync(
    path.join(root, "src/services/utils.ts"),
    "utf8"
  );
  assert.doesNotMatch(lib, /commands::delete_directory/);
  assert.doesNotMatch(utils, /fn delete_directory/);
  assert.doesNotMatch(service, /deleteDirectory/);
});
