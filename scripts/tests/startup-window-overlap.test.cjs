const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.join(__dirname, "../..");
const config = JSON.parse(
  fs.readFileSync(path.join(root, "src-tauri/tauri.conf.json"), "utf8")
);
const source = fs.readFileSync(path.join(root, "src-tauri/src/lib.rs"), "utf8");

test("early frontend windows keep their real routes", () => {
  for (const [label, url] of [["main", "/"], ["tray_popup", "/tray-popup"]]) {
    const window = config.app.windows.find((item) => item.label === label);
    assert.equal(window.create, true, label);
    assert.equal(window.url, url, label);
  }
  assert.equal(fs.existsSync(path.join(root, "public/startup.html")), false);
});

test("early application commands wait until all managed state exists", () => {
  assert.match(source, /fn startup_invoke_handler/);
  assert.match(source, /pending\.push\(item\)/);
  assert.match(source, /\.submit\(invoke\)/);
  assert.match(source, /fn release_startup_invokes/);
  const manage = source.indexOf("app.manage(Mutex::new(local_ygg_server.clone()))");
  const release = source.indexOf("release_startup_invokes();");
  const background = source.indexOf("check if full account feature");
  assert.ok(manage >= 0 && release > manage && background > release);
});
