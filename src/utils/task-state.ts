import {
  GTaskEventPayload,
  GTaskEventStatusEnums,
  PTaskEventPayload,
  PTaskEventStatusEnums,
  TaskDesc,
  TaskDescStatusEnums,
  TaskGroupDesc,
  TaskTypeEnums,
} from "@/models/task";
import {
  compareCampusInstallStages,
  isCampusInstallGroup,
} from "@/utils/campus-install-task";
import { isModpackUpdateGroup } from "@/utils/modpack-update-task";

const statusLevel = (desc: TaskDesc) => {
  switch (desc.status) {
    case TaskDescStatusEnums.Failed:
      return 0;
    case TaskDescStatusEnums.InProgress:
      return 1;
    case TaskDescStatusEnums.Waiting:
      return 2;
    case TaskDescStatusEnums.Completed:
      return 4;
    default:
      return 3;
  }
};

/** Recompute presentation fields without mutating the event or previous state. */
export function withGroupInfo(group: TaskGroupDesc): TaskGroupDesc {
  let taskDescs = group.taskDescs.map((task) => ({ ...task }));
  if (group.status === GTaskEventStatusEnums.Completed) {
    taskDescs = taskDescs.map((task) => ({
      ...task,
      status: TaskDescStatusEnums.Completed,
      current: task.total,
    }));
  }

  const finishedCount = taskDescs.filter(
    (task) => task.status === TaskDescStatusEnums.Completed
  ).length;
  const known = taskDescs.filter((task) => task.total > 0);
  const knownTotal = known.reduce((sum, task) => sum + task.total, 0);
  const knownCurrent = known.reduce((sum, task) => sum + task.current, 0);
  const estimatedTotal = known.length
    ? knownTotal +
      (taskDescs.length - known.length) * (knownTotal / known.length)
    : 0;
  const estimatedTime = taskDescs
    .filter(
      (task) =>
        task.status === TaskDescStatusEnums.InProgress && task.estimatedTime
    )
    .map((task) => task.estimatedTime!)
    .sort((a, b) => a.secs - b.secs)
    .at(-1);

  taskDescs = taskDescs
    .map((task) => ({
      ...task,
      progress: task.total ? (task.current * 100) / task.total : 0,
    }))
    .sort((a, b) => {
      if (
        isCampusInstallGroup(group.taskGroup) ||
        isModpackUpdateGroup(group.taskGroup)
      ) {
        return compareCampusInstallStages(a, b);
      }
      return statusLevel(a) - statusLevel(b);
    });

  return {
    ...group,
    taskDescs,
    finishedCount,
    progress: estimatedTotal ? (knownCurrent * 100) / estimatedTotal : 0,
    estimatedTime,
  };
}

function applyTaskStatus(task: TaskDesc, payload: PTaskEventPayload): TaskDesc {
  if (task.taskId !== payload.id) return task;
  switch (payload.event.status) {
    case PTaskEventStatusEnums.Started:
      return {
        ...task,
        status: TaskDescStatusEnums.InProgress,
        total: payload.event.total,
      };
    case PTaskEventStatusEnums.Completed:
      return {
        ...task,
        status: TaskDescStatusEnums.Completed,
        current: task.total,
      };
    case PTaskEventStatusEnums.Stopped:
      return { ...task, status: TaskDescStatusEnums.Stopped };
    case PTaskEventStatusEnums.Cancelled:
      return { ...task, status: TaskDescStatusEnums.Cancelled };
    case PTaskEventStatusEnums.InProgress:
      return {
        ...task,
        current: payload.event.current,
        status: TaskDescStatusEnums.InProgress,
        estimatedTime: payload.event.estimatedTime,
        speed: payload.event.speed,
      };
    case PTaskEventStatusEnums.Failed:
      return {
        ...task,
        status: TaskDescStatusEnums.Failed,
        reason: payload.event.reason,
      };
    case PTaskEventStatusEnums.Created:
      return task;
  }
}

export function applyTaskEvent(
  tasks: TaskGroupDesc[],
  payload: PTaskEventPayload
): TaskGroupDesc[] {
  const existing = tasks.find((group) => group.taskGroup === payload.taskGroup);
  if (!existing) {
    if (payload.event.status !== PTaskEventStatusEnums.Created) return tasks;
    return [
      withGroupInfo({
        status: GTaskEventStatusEnums.Started,
        taskGroup: payload.taskGroup,
        taskDescs: [payload.event.desc],
      }),
      ...tasks,
    ];
  }

  if (payload.event.status === PTaskEventStatusEnums.Created) {
    const incoming = payload.event.desc;
    const incomingDest =
      incoming.payload.taskType === TaskTypeEnums.Download
        ? incoming.payload.dest
        : undefined;
    const hasId = existing.taskDescs.some((task) => task.taskId === payload.id);
    const taskDescs = hasId
      ? existing.taskDescs
      : incomingDest !== undefined &&
          existing.taskDescs.some(
            (task) =>
              task.payload.taskType === TaskTypeEnums.Download &&
              task.payload.dest === incomingDest
          )
        ? existing.taskDescs.map((task) =>
            task.payload.taskType === TaskTypeEnums.Download &&
            task.payload.dest === incomingDest
              ? incoming
              : task
          )
        : [incoming, ...existing.taskDescs];
    return tasks.map((group) =>
      group.taskGroup === payload.taskGroup
        ? withGroupInfo({ ...group, taskDescs })
        : group
    );
  }

  if (!existing.taskDescs.some((task) => task.taskId === payload.id))
    return tasks;
  return tasks.map((group) =>
    group.taskGroup === payload.taskGroup
      ? withGroupInfo({
          ...group,
          taskDescs: group.taskDescs.map((task) =>
            applyTaskStatus(task, payload)
          ),
        })
      : group
  );
}

export function applyGroupEvent(
  tasks: TaskGroupDesc[],
  payload: GTaskEventPayload
): TaskGroupDesc[] {
  let found = false;
  const next = tasks.map((group) => {
    if (group.taskGroup !== payload.taskGroup) return group;
    found = true;
    // The backend owns the group state. A stale failed child must not rewrite
    // a completed group, and the incoming payload is never modified.
    return withGroupInfo({ ...group, status: payload.event });
  });
  if (!found && payload.taskGroup.startsWith("optifine-libraries?")) {
    next.push(
      withGroupInfo({
        taskGroup: payload.taskGroup,
        taskDescs: [],
        status: payload.event,
      })
    );
  }
  return next;
}
