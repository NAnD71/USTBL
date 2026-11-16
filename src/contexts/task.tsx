import { useToast as useChakraToast } from "@chakra-ui/react";
import { emit } from "@tauri-apps/api/event";
import { useRouter } from "next/router";
import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { OtherResourceType } from "@/enums/resource";
import {
  CampusInstallProgress,
  CampusUnfinishedInstall,
  CampusUnfinishedPublish,
} from "@/models/campus-modpack";
import { ModpackUpdateFinished } from "@/models/modpack-update";
import {
  GTaskEventPayload,
  GTaskEventStatusEnums,
  PTaskEventPayload,
  PTaskEventStatusEnums,
  TaskGroupDesc,
  TaskParam,
  TaskPayload,
  TaskTypeEnums,
} from "@/models/task";
import { CampusModpackService } from "@/services/campus-modpack";
import { ConfigService } from "@/services/config";
import { InstanceService } from "@/services/instance";
import { ModpackUpdateService } from "@/services/modpack-update";
import { TaskService } from "@/services/task";
import {
  localizeCampusInstallError,
  removeCampusInstallGroups,
} from "@/utils/campus-install-task";
import { localizeFileError } from "@/utils/file-error";
import { takeLoaderCompletions } from "@/utils/loader-completion";
import { enqueueSuggestionModal } from "@/utils/modal-queue";
import {
  finishModpackUpdate,
  localizeModpackUpdateError,
} from "@/utils/modpack-update-task";
import { takeRenameSuggestion } from "@/utils/rename-suggestion";
import {
  applyGroupEvent,
  applyTaskEvent,
  withGroupInfo,
} from "@/utils/task-state";

interface TaskContextType {
  installProgress: Record<string, CampusInstallProgress>;
  updateResults: Record<string, ModpackUpdateFinished>;
  tasks: TaskGroupDesc[];
  generalPercent: number | undefined; // General progress percentage for all tasks
  /** Publish records waiting on the user (§3.2.3); the nav-bar task button
   * counts these too, so it stays visible after a restart. */
  unfinishedPublishes: CampusUnfinishedPublish[];
  unfinishedInstalls: CampusUnfinishedInstall[];
  refreshUnfinishedInstalls: () => void;
  refreshUnfinishedPublishes: () => void;
  handleScheduleProgressiveTaskGroup: (
    taskGroup: string,
    params: TaskParam[]
  ) => void;
  handleCancelProgressiveTaskGroup: (taskGroup: string) => void;
  handleStopProgressiveTaskGroup: (taskGroup: string) => void;
  handleResumeProgressiveTaskGroup: (taskGroup: string) => void;
  handleClearHistoryTaskGroups: () => void;
}

export const TaskContext = createContext<TaskContextType | undefined>(
  undefined
);

export const TaskContextProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const toast = useToast();
  const { close: closeToast } = useChakraToast();
  const { getInstanceList, checkModpackUpdate } = useGlobalData();
  const router = useRouter();
  const { getJavaInfos } = useLauncherConfig();
  const { openSharedModal } = useSharedModals();
  const [tasks, setTasks] = useState<TaskGroupDesc[]>([]);
  const tasksRef = React.useRef<TaskGroupDesc[]>([]);
  tasksRef.current = tasks;
  const taskListRequest = React.useRef(0);
  const [installProgress, setInstallProgress] = useState<
    Record<string, CampusInstallProgress>
  >({});
  const [unfinishedInstalls, setUnfinishedInstalls] = useState<
    CampusUnfinishedInstall[]
  >([]);
  const [generalPercent, setGeneralPercent] = useState<number>();
  const [updateResults, setUpdateResults] = useState<
    Record<string, ModpackUpdateFinished>
  >({});
  const { t } = useTranslation();
  const loaderCompletionClaims = React.useRef(new Map<string, Set<string>>());
  const loaderInstallsInFlight = React.useRef(new Set<string>());
  const loaderPreparationsClaimed = React.useRef(new Set<string>());
  const [loaderCompletionTick, setLoaderCompletionTick] = useState(0);

  useEffect(() => {
    for (const instanceId of takeLoaderCompletions(
      tasks,
      loaderCompletionClaims.current,
      loaderInstallsInFlight.current
    )) {
      loaderInstallsInFlight.current.add(instanceId);
      const instanceName = getInstanceList()?.find(
        (i) => i.id === instanceId
      )?.name;
      const loadingToast = toast({
        title: t("Services.instance.finishModLoaderInstall.loading", {
          instanceName,
        }),
        status: "loading",
      });
      InstanceService.finishModLoaderInstall(instanceId)
        .then((response) => {
          if (response.status === "success" && !response.data) return;
          toast({
            title: response.message,
            description:
              response.status === "success" ? undefined : response.details,
            status: response.status === "success" ? "success" : "error",
          });
          if (response.status === "success") getInstanceList(true);
        })
        .catch((error) =>
          logger.error("Failed to finish loader install:", error)
        )
        .finally(() => {
          closeToast(loadingToast);
          loaderInstallsInFlight.current.delete(instanceId);
          // A second runtime group may have finished while this invocation
          // waited for the backend's scan to publish the instance.
          setLoaderCompletionTick((tick) => tick + 1);
        });
    }
  }, [tasks, loaderCompletionTick, getInstanceList, toast, t, closeToast]);

  const handleRetrieveProgressTasks = useCallback(() => {
    const request = taskListRequest.current + 1;
    taskListRequest.current = request;
    TaskService.retrieveProgressiveTaskList().then((response) => {
      if (request !== taskListRequest.current) return;
      if (response.status === "success") {
        logger.info("Retrieved progressive tasks:", response.data);
        setTasks((prevTasks) => {
          let tasks = response.data
            .map((group) => {
              const prevGroup = prevTasks.find(
                (task) => task.taskGroup === group.taskGroup
              );
              return withGroupInfo(prevGroup || group);
            })
            .filter(
              (group) => group.status !== GTaskEventStatusEnums.Cancelled
            );
          tasks.sort((a, b) => {
            let { timestamp: aTime } = parseTaskGroup(a.taskGroup);
            let { timestamp: bTime } = parseTaskGroup(b.taskGroup);
            return bTime - aTime;
          });
          return tasks;
        });
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    });
  }, [toast]);

  useEffect(() => {
    handleRetrieveProgressTasks();
  }, [handleRetrieveProgressTasks]);

  const handleScheduleProgressiveTaskGroup = useCallback(
    (taskGroup: string, params: TaskParam[]) => {
      TaskService.scheduleProgressiveTaskGroup(taskGroup, params).then(
        (response) => {
          // success toast will now be called by task context group listener
          if (response.status !== "success") {
            toast({
              title: response.message,
              description: response.details,
              status: "error",
            });
          }
        }
      );
    },
    [toast]
  );

  const handleCancelProgressiveTaskGroup = useCallback(
    (taskGroup: string) => {
      TaskService.cancelProgressiveTaskGroup(taskGroup).then((response) => {
        if (response.status !== "success") {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      });
    },
    [toast]
  );

  const handleStopProgressiveTaskGroup = useCallback(
    (taskGroup: string) => {
      TaskService.stopProgressiveTaskGroup(taskGroup).then((response) => {
        if (response.status !== "success") {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      });
    },
    [toast]
  );

  const handleResumeProgressiveTaskGroup = useCallback(
    (taskGroup: string) => {
      TaskService.resumeProgressiveTaskGroup(taskGroup).then((response) => {
        if (response.status !== "success") {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      });
    },
    [toast]
  );

  const isActiveGroup = (t: TaskGroupDesc) =>
    t.status === GTaskEventStatusEnums.Started ||
    t.status === GTaskEventStatusEnums.Stopped;

  const handleClearHistoryTaskGroups = useCallback(() => {
    tasks
      .filter((task) => !isActiveGroup(task))
      .forEach((group) => {
        void TaskService.deleteProgressiveTaskGroup(group.taskGroup);
      });
    setTasks((previous) => previous.filter(isActiveGroup));
  }, [tasks]);

  useEffect(() => {
    const unlisten = TaskService.onProgressiveTaskUpdate(
      (payload: PTaskEventPayload) => {
        if (payload.event.status === PTaskEventStatusEnums.Failed) {
          logger.error(
            `Task ${payload.id} failed in group ${payload.taskGroup}: ${payload.event.reason}`
          );
        }
        setTasks((previous) => applyTaskEvent(previous, payload));
      }
    );

    return () => {
      unlisten();
    };
  }, []);

  useEffect(() => {
    const unlisten = TaskService.onTaskGroupUpdate(
      (payload: GTaskEventPayload) => {
        logger.info(`Received task group update: ${payload.event}`);
        setTasks((previous) => applyGroupEvent(previous, payload));

        const { name, version } = parseTaskGroup(payload.taskGroup);

        if (
          (name === "loader-installer" || name === "optifine-installer") &&
          payload.event === GTaskEventStatusEnums.Completed &&
          version &&
          !loaderPreparationsClaimed.current.has(payload.taskGroup)
        ) {
          const instanceId = version;
          const component =
            name === "optifine-installer" ? "optifine" : "loader";
          loaderPreparationsClaimed.current.add(payload.taskGroup);
          InstanceService.prepareModLoaderInstall(instanceId, component)
            .then((response) => {
              if (response.status !== "success") {
                toast({
                  status: "error",
                  title: response.message,
                  description: response.details,
                });
                getInstanceList(true);
              }
            })
            .catch((error) => {
              logger.error(
                "Failed to prepare mod loader after installer download:",
                error
              );
              getInstanceList(true);
            });
        }

        // Modpack jobs report their own final result, independently of dialogs.
        // Installer-only groups continue into loader preparation silently.
        if (
          name !== "modpack-publish" &&
          name !== "modpack-install" &&
          name !== "modpack-update" &&
          name !== "loader-installer" &&
          name !== "optifine-installer"
        )
          toast({
            status:
              payload.event === GTaskEventStatusEnums.Failed
                ? "error"
                : "success",
            title: t(
              `Services.task.onTaskGroupUpdate.status.${payload.event}`,
              {
                param: t(`DownloadTasksPage.task.${name}`, {
                  param: version || "",
                }),
              }
            ),
          });

        if (payload.event === GTaskEventStatusEnums.Completed) {
          switch (name) {
            case "game-client":
            case "change-mod-loader": {
              getInstanceList(true);
              break;
            }
            case "mod":
            case "mod-update":
              emit("instance:refresh-resource-list", OtherResourceType.Mod);
              break;
            case "resourcepack":
              emit(
                "instance:refresh-resource-list",
                OtherResourceType.ResourcePack
              );
              break;
            case "shader":
              emit(
                "instance:refresh-resource-list",
                OtherResourceType.ShaderPack
              );
              break;
            case "launcher-update": {
              let group = tasksRef.current.find(
                (t) => t.taskGroup === payload.taskGroup
              );
              if (group && group.taskDescs.length > 0) {
                ConfigService.installLauncherUpdate(
                  group.taskDescs[0].payload.filename || "",
                  true
                ).then((response) => {
                  if (response.status !== "success") {
                    toast({
                      title: response.message,
                      description: response.details,
                      status: "error",
                    });
                  }
                });
              }
              break;
            }
            case "mojang-java":
              getJavaInfos(true);
              break;
            default:
              break;
          }
        }
      }
    );
    return () => {
      unlisten();
    };
  }, [t, toast, closeToast, getInstanceList, getJavaInfos, openSharedModal]);

  const [unfinishedPublishes, setUnfinishedPublishes] = useState<
    CampusUnfinishedPublish[]
  >([]);

  // Keep the native listener mounted across list refreshes and navigation.
  const updateFinishedHandler = React.useRef<
    (payload: ModpackUpdateFinished) => void
  >(() => {});
  updateFinishedHandler.current = (payload) => {
    setUpdateResults((previous) => ({
      ...previous,
      [payload.group]: payload,
    }));
    finishModpackUpdate(payload, {
      refresh: () => {
        getInstanceList(true);
      },
      check: checkModpackUpdate,
      notify: (failed, error) =>
        toast({
          title: t(
            failed
              ? "Services.instance.applyModpackUpdate.error.title"
              : "Services.instance.applyModpackUpdate.success"
          ),
          description: error
            ? localizeModpackUpdateError(t, error)
            : payload.instanceName,
          status: failed ? "error" : "success",
        }),
      loaderPending: () => {
        toast({
          title: t("UpdateModpackModal.toast.loaderPending"),
          status: "info",
        });
        void router.push("/downloads");
      },
      suggestRename: (instanceId) => {
        const name = getInstanceList()?.find(
          (instance) => instance.id === instanceId
        )?.name;
        const suggestion = takeRenameSuggestion(instanceId, name);
        if (!suggestion) return;
        enqueueSuggestionModal((release) =>
          openSharedModal("rename-suggestion", {
            instanceId,
            version: suggestion.version,
            suggestedName: suggestion.suggested,
            onDone: release,
          })
        );
      },
    });
  };
  useEffect(
    () =>
      ModpackUpdateService.onUpdateFinished((payload) =>
        updateFinishedHandler.current(payload)
      ),
    []
  );
  const refreshUnfinishedPublishes = useCallback(() => {
    CampusModpackService.listCampusModpackPublishJobs().then((response) => {
      if (response.status === "success") setUnfinishedPublishes(response.data);
    });
  }, []);

  const refreshUnfinishedInstalls = useCallback(() => {
    CampusModpackService.listCampusModpackInstallJobs().then((response) => {
      if (response.status === "success") setUnfinishedInstalls(response.data);
    });
  }, []);

  useEffect(
    () =>
      CampusModpackService.onInstallGroupsRemoved((groups) => {
        setTasks((previous) => removeCampusInstallGroups(previous, groups));
        setInstallProgress((previous) =>
          Object.fromEntries(
            Object.entries(previous).filter(
              ([group]) => !groups.includes(group)
            )
          )
        );
      }),
    []
  );

  useEffect(
    () =>
      CampusModpackService.onInstallProgress((progress) => {
        if (progress.group) {
          setInstallProgress((previous) => ({
            ...previous,
            [progress.group!]: progress,
          }));
        }
      }),
    []
  );

  useEffect(
    () =>
      CampusModpackService.onInstallFinished((payload) => {
        refreshUnfinishedInstalls();
        if (!payload.error) getInstanceList(true);
        toast({
          title: t(
            payload.error
              ? "Services.instance.installCampusModpack.error.title"
              : "Services.instance.installCampusModpack.success"
          ),
          description: payload.error
            ? localizeCampusInstallError(t, payload.error)
            : undefined,
          status: payload.error ? "error" : "success",
        });
      }),
    [getInstanceList, refreshUnfinishedInstalls, t, toast]
  );

  // The records drive the nav-bar task button too, so they are fetched here
  // once, on every publish ending, and on a slow timer — the components
  // showing them never poll on their own (§3.2.2, 10-23 addition).
  useEffect(() => {
    refreshUnfinishedPublishes();
    refreshUnfinishedInstalls();
    const timer = setInterval(() => {
      refreshUnfinishedPublishes();
      refreshUnfinishedInstalls();
    }, 3000);
    return () => clearInterval(timer);
  }, [refreshUnfinishedInstalls, refreshUnfinishedPublishes]);

  // A publish runs in the background; this is where its result is told.
  useEffect(() => {
    return CampusModpackService.onPublishFinished((payload) => {
      refreshUnfinishedPublishes();
      if (payload.error !== undefined) {
        toast({
          title: t("Services.instance.publishCampusModpack.error.title"),
          description:
            localizeFileError(t, payload.error) ??
            t(
              `Services.instance.publishCampusModpack.error.description.${payload.error}`,
              { defaultValue: payload.error }
            ),
          status: "error",
        });
        return;
      }
      const adoption = payload.outcome?.adoption;
      if (adoption === "followed") getInstanceList(true);
      const skipped = payload.outcome?.skipped.length ?? 0;
      const locallyDeleted = payload.outcome?.locallyDeleted ?? 0;
      const notes = [
        skipped > 0 ? t("PublishModpackModal.skipped", { count: skipped }) : "",
        adoption && adoption !== "followed"
          ? t(`PublishModpackModal.adoption.${adoption}`)
          : locallyDeleted > 0
            ? t("PublishModpackModal.adoption.localDeleted", {
                count: locallyDeleted,
              })
            : "",
        adoption && adoption !== "followed" && locallyDeleted > 0
          ? t("PublishModpackModal.adoption.localDeleted", {
              count: locallyDeleted,
            })
          : "",
      ].filter(Boolean);
      toast({
        title: t("Services.instance.publishCampusModpack.success"),
        description: notes.length > 0 ? notes.join(" ") : undefined,
        status: "success",
      });
    });
  }, [getInstanceList, refreshUnfinishedPublishes, t, toast]);

  useEffect(() => {
    if (!tasks || !tasks.length) setGeneralPercent(undefined);
    else {
      let filteredTasks = tasks.filter(
        (t) => t.status === GTaskEventStatusEnums.Started
      );

      if (filteredTasks.length === 0) setGeneralPercent(undefined);
      else {
        setGeneralPercent(
          filteredTasks.reduce(
            (acc, group) => acc + (group.progress ?? 0) / filteredTasks.length,
            0
          )
        );
      }
    }
  }, [tasks]);

  return (
    <TaskContext.Provider
      value={{
        tasks,
        installProgress,
        updateResults,
        generalPercent,
        unfinishedPublishes,
        refreshUnfinishedPublishes,
        unfinishedInstalls,
        refreshUnfinishedInstalls,
        handleScheduleProgressiveTaskGroup,
        handleCancelProgressiveTaskGroup,
        handleStopProgressiveTaskGroup,
        handleResumeProgressiveTaskGroup,
        handleClearHistoryTaskGroups,
      }}
    >
      {children}
    </TaskContext.Provider>
  );
};

export const useTaskContext = (): TaskContextType => {
  const context = useContext(TaskContext);
  if (!context) {
    throw new Error("useTaskContext must be used within a TaskContextProvider");
  }
  return context;
};

/** Where a download writes to; other kinds of task have no destination. */
export const destOf = (payload: TaskPayload): string | undefined =>
  payload.taskType === TaskTypeEnums.Download ? payload.dest : undefined;

export const parseTaskGroup = (
  taskGroup: string
): {
  name: string;
  version?: string;
  timestamp: number;
  isRetry: boolean;
  rawName: string;
} => {
  const lastAtIndex = taskGroup.lastIndexOf("@");
  let rawName: string;
  let timestamp: number;

  if (lastAtIndex === -1) {
    rawName = taskGroup;
    timestamp = Date.now();
  } else {
    rawName = taskGroup.substring(0, lastAtIndex);
    timestamp = parseInt(taskGroup.substring(lastAtIndex + 1));
  }

  const [name, version] = rawName.split("?");

  return {
    name: name.replace(/^retry-/, ""),
    version,
    isRetry: name.startsWith("retry-"),
    timestamp: timestamp,
    rawName,
  };
};
