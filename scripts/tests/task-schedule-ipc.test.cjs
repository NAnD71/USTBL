const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

function read(file) {
  return fs.readFileSync(path.join(__dirname, "../../", file), "utf8");
}

test("only the restricted resource-download entry is exposed over IPC", () => {
  const commands = read("src-tauri/src/tasks/commands.rs");
  const internal = commands.indexOf(
    "pub async fn schedule_progressive_task_group"
  );
  assert.ok(internal >= 0);
  assert.ok(
    !commands.slice(0, internal).trimEnd().endsWith("#[tauri::command]")
  );
  assert.match(
    commands,
    /#\[tauri::command\]\s*pub async fn schedule_resource_download/
  );

  const lib = read("src-tauri/src/lib.rs");
  assert.match(lib, /commands::schedule_resource_download/);
  assert.doesNotMatch(lib, /commands::schedule_progressive_task_group/);
  assert.match(
    read("src/services/task.ts"),
    /invoke\("schedule_resource_download"/
  );
});
