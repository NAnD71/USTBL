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
      { compilerOptions: { module: ts.ModuleKind.CommonJS } }
    ).outputText
  )(exports, module, () => ({}));
  return module.exports;
}

const {
  campusInstallCardState,
  campusInstallStageKey,
  campusInstallProgressView,
  visibleCampusInstalls,
  localizeCampusInstallError,
} = load("src/utils/campus-install-task.ts");

test("campus install cards expose active, paused, complete and failure states", () => {
  assert.equal(campusInstallCardState("Started"), "active-install");
  assert.equal(campusInstallCardState("InProgress"), "active-install");
  assert.equal(campusInstallCardState("Stopped"), "paused");
  assert.equal(campusInstallCardState("Completed"), "completed");
  assert.equal(campusInstallCardState("Failed"), "failed");
  assert.equal(campusInstallCardState("Started", "NETWORK_ERROR"), "failed");
  assert.equal(campusInstallCardState("Cancelled"), "cancelled");
});

test("campus install stage labels are normalized for locale lookup", () => {
  assert.equal(campusInstallStageKey("  Fetching "), "fetching");
  assert.equal(campusInstallStageKey("finalizing"), "applying");
  assert.equal(campusInstallStageKey("WaitingClient"), "waitingclient");
});

test("cards show verified file and byte counts only in the fetching stage", () => {
  const progress = {
    stage: "fetching",
    done: 12,
    total: 20,
    bytesDone: 120,
    bytesTotal: 200,
  };
  assert.deepEqual(campusInstallProgressView(progress), {
    stage: "fetching",
    done: 12,
    total: 20,
    bytes: { done: 120, total: 200 },
  });
  for (const stage of [
    "waitingClient",
    "preparing",
    "locating",
    "applying",
    "finalizing",
  ]) {
    assert.equal(campusInstallProgressView({ ...progress, stage }).bytes, null);
  }
  assert.equal(campusInstallProgressView(null), null);
});

test("resume and discard records appear in their card or in the restart list, never both", () => {
  const jobs = [
    { instanceId: "a", group: "modpack-install?a", resumable: true },
    { instanceId: "b", group: "modpack-install?b", resumable: true },
    { instanceId: "c", group: null, resumable: true },
  ];
  assert.deepEqual(visibleCampusInstalls(jobs, [jobs[0].group]), [
    jobs[1],
    jobs[2],
  ]);
  assert.deepEqual(
    visibleCampusInstalls(jobs, [jobs[0].group], jobs[0].group),
    [jobs[0]]
  );
  assert.deepEqual(visibleCampusInstalls(jobs, []), jobs);
});

test("failure reasons use locale keys and unknown errors never leak raw codes", () => {
  const t = (key, options) =>
    key.includes("MODPACK_UPDATE_VERIFY_FAILED")
      ? "校验失败"
      : (options?.defaultValue ?? key);
  assert.equal(
    localizeCampusInstallError(t, "MODPACK_UPDATE_VERIFY_FAILED"),
    "校验失败"
  );
  assert.match(
    localizeCampusInstallError(t, "http status 503"),
    /NETWORK_ERROR$/
  );
  assert.match(
    localizeCampusInstallError(t, "unknown raw failure"),
    /UNKNOWN_ERROR$/
  );
});
