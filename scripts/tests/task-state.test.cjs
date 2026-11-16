const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const ts = require("typescript");

const exportsFor = {};
const enums = {
  GTaskEventStatusEnums: {
    Started: "Started",
    Failed: "Failed",
    Completed: "Completed",
    Stopped: "Stopped",
    Cancelled: "Cancelled",
  },
  PTaskEventStatusEnums: {
    Created: "Created",
    Started: "Started",
    InProgress: "InProgress",
    Completed: "Completed",
    Failed: "Failed",
    Stopped: "Stopped",
    Cancelled: "Cancelled",
  },
  TaskDescStatusEnums: {
    Stopped: "Stopped",
    Cancelled: "Cancelled",
    Completed: "Completed",
    InProgress: "InProgress",
    Failed: "Failed",
    Waiting: "Waiting",
  },
  TaskTypeEnums: { Download: "download", Upload: "upload" },
};
new Function(
  "exports",
  "require",
  ts.transpileModule(
    fs.readFileSync(
      path.join(__dirname, "../../src/utils/task-state.ts"),
      "utf8"
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }
  ).outputText
)(exportsFor, (id) => {
  if (id === "@/models/task") return enums;
  if (id === "@/utils/campus-install-task") {
    return {
      compareCampusInstallStages: () => 0,
      isCampusInstallGroup: () => false,
    };
  }
  if (id === "@/utils/modpack-update-task") {
    return { isModpackUpdateGroup: () => false };
  }
  throw new Error(id);
});
const { applyGroupEvent, applyTaskEvent, withGroupInfo } = exportsFor;

function task(id, status = "Waiting", current = 0, total = 10) {
  return {
    taskId: id,
    taskGroup: "mod@1",
    payload: { taskType: "download", dest: "mod.jar" },
    status,
    current,
    total,
  };
}

function group(status = "Started", taskDescs = [task(1)]) {
  return { taskGroup: "mod@1", status, taskDescs };
}

test("group completion does not mutate state or let a stale child rewrite the backend status", () => {
  const previous = [group("Started", [task(1, "Failed")])];
  const before = structuredClone(previous);
  const payload = { taskGroup: "mod@1", event: "Completed" };

  const next = applyGroupEvent(previous, payload);

  assert.equal(next[0].status, "Completed");
  assert.deepEqual(previous, before);
  assert.deepEqual(payload, { taskGroup: "mod@1", event: "Completed" });
});

test("task events and retry creation replace state immutably without duplicates", () => {
  const previous = [group()];
  const progressed = applyTaskEvent(previous, {
    id: 1,
    taskGroup: "mod@1",
    event: { status: "InProgress", current: 4 },
  });
  assert.equal(progressed[0].taskDescs[0].current, 4);
  assert.equal(progressed[0].taskDescs[0].status, "InProgress");
  assert.equal(previous[0].taskDescs[0].current, 0);

  const incoming = task(2, "Waiting");
  const retried = applyTaskEvent(progressed, {
    id: 2,
    taskGroup: "mod@1",
    event: { status: "Created", desc: incoming },
  });
  assert.equal(retried[0].taskDescs.length, 1);
  assert.equal(retried[0].taskDescs[0].taskId, 2);
  assert.equal(progressed[0].taskDescs[0].taskId, 1);
});

test("group presentation is computed on copies", () => {
  const original = group("Started", [task(1, "InProgress", 5, 10)]);
  const result = withGroupInfo(original);

  assert.equal(result.finishedCount, 0);
  assert.equal(result.progress, 50);
  assert.equal(result.taskDescs[0].progress, 50);
  assert.equal(original.progress, undefined);
  assert.equal(original.taskDescs[0].progress, undefined);
});
