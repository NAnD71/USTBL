const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const source = fs.readFileSync(
  path.join(__dirname, "../../src-tauri/src/instance/commands.rs"),
  "utf8"
);
const create = source.slice(
  source.indexOf("pub async fn create_instance"),
  source.indexOf("pub async fn finish_mod_loader_install")
);
const switchLoader = source.slice(
  source.indexOf("pub(crate) async fn switch_mod_loader"),
  source.indexOf("pub async fn retrieve_modpack_meta_info")
);

test("instance creation registers before scheduling and cancels a failed queue", () => {
  const register = create.indexOf("register_created_campus_instance");
  const schedule = create.indexOf("schedule_progressive_task_group");
  assert.ok(register >= 0 && schedule >= 0 && register < schedule);
  assert.match(create, /format!\("game-client\?\{\}", instance\.id\)/);
  assert.ok(create.indexOf("cancel_progressive_task_group") > schedule);
  assert.ok(create.indexOf("dir_guard.commit()") > schedule);
});

test("loader switching plans the replacement before removal and uses the shared instance writer", () => {
  const install = switchLoader.indexOf("install_mod_loader(");
  const remove = switchLoader.indexOf("remove_fabric_api_mods(");
  const store = switchLoader.indexOf("store_instance(");
  assert.ok(install >= 0 && remove > install && store > remove);
  assert.doesNotMatch(switchLoader, /instance\s*\.save_json_cfg/);
});
