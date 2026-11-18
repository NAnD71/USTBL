const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.join(__dirname, "../..");
const read = (name) => fs.readFileSync(path.join(root, name), "utf8");

test("only launcher and account saves turn protection into a logged no-op", () => {
  const storage = read("src-tauri/src/storage.rs");
  assert.match(storage, /fn write_json_or_skip_protected/);
  assert.match(storage, /Skipping write-protected JSON save/);
  for (const name of [
    "src-tauri/src/launcher_config/models.rs",
    "src-tauri/src/account/models.rs",
  ]) {
    assert.match(read(name), /write_json_or_skip_protected/, name);
  }
  assert.match(storage, /return Err\(io::Error::new\([\s\S]*write is protected for this run/);
});

test("startup read protection is delivered once with the approved Q3 copy", () => {
  const lib = read("src-tauri/src/lib.rs");
  assert.match(lib, /take_startup_write_protected_files/);
  assert.equal((lib.match(/record_write_protection\(/g) || []).length, 3);
  const frontend = read("src/components/special/global-event-handler.tsx");
  assert.match(frontend, /take_startup_write_protected_files/);
  assert.ok(
    frontend.includes(
      "无法读取启动器设置文件（可能被其他程序占用）。本次使用默认设置，运行期间的修改不会保存。请关闭占用程序后重启启动器。"
    )
  );
  assert.ok(
    frontend.includes(
      "无法读取账号文件（可能被其他程序占用）。本次登录及账号修改不会保存。请关闭占用程序后重启启动器。"
    )
  );
});
