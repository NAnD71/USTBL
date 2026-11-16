const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

test("startup recovers config and account files without an unchecked prompt", () => {
  const lib = read("src-tauri/src/lib.rs");
  assert.match(
    lib,
    /recover_json\(&<LauncherConfig as Storage>::file_path\(\)\)/
  );
  assert.match(lib, /recover_json\(&<AccountInfo as Storage>::file_path\(\)\)/);
  assert.match(lib, /fn take_startup_corrupt_files/);
  assert.match(lib, /take_startup_corrupt_files,/);

  const handler = read("src/components/special/global-event-handler.tsx");
  assert.match(handler, /invoke<string\[\]>\("take_startup_corrupt_files"\)/);
  assert.match(handler, /已恢复默认设置。原文件备份为/);
  assert.match(handler, /需要重新登录。原文件备份为/);
});
