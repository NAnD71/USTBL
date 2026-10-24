import type { TFunction } from "i18next";
import type {
  CampusInstallProgress,
  CampusUnfinishedInstall,
} from "@/models/campus-modpack";
import type { GTaskEventStatusEnums } from "@/models/task";

export type CampusInstallCardState =
  | "active-install"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled";

/** Keep the task-card state mapping independent from React rendering. */
export const campusInstallCardState = (
  status: GTaskEventStatusEnums | string,
  reason?: string
): CampusInstallCardState => {
  if (status === "Completed") return "completed";
  if (status === "Failed" || reason) return "failed";
  if (status === "Cancelled") return "cancelled";
  if (status === "Stopped") return "paused";
  return "active-install";
};

export const campusInstallStageKey = (filename: string): string =>
  filename.trim().toLowerCase() === "finalizing"
    ? "applying"
    : filename.trim().toLowerCase();

export const campusInstallProgressView = (
  progress?: Omit<CampusInstallProgress, "instanceId"> | null
) =>
  progress
    ? {
        stage: campusInstallStageKey(progress.stage),
        done: progress.done,
        total: progress.total,
        bytes:
          progress.stage === "fetching"
            ? { done: progress.bytesDone, total: progress.bytesTotal }
            : null,
      }
    : null;

export const visibleCampusInstalls = (
  jobs: CampusUnfinishedInstall[],
  groups: string[],
  group?: string
) =>
  jobs.filter((job) =>
    group ? job.group === group : !job.group || !groups.includes(job.group)
  );

export const isCampusInstallGroup = (group: string): boolean =>
  group.startsWith("modpack-install?");

export const localizeCampusInstallError = (
  t: TFunction,
  reason?: string
): string => {
  if (!reason) return t("DownloadTasksPage.label.error");
  if (
    /network|connection|timeout|timed out|request|http|status|range ended/i.test(
      reason
    )
  ) {
    return t(
      "Services.instance.installCampusModpack.error.description.NETWORK_ERROR"
    );
  }
  if (
    /os error|permission|access.*denied|disk|space|file creation|read.only/i.test(
      reason
    )
  ) {
    return t(
      "Services.instance.installCampusModpack.error.description.FILE_OPERATION_ERROR"
    );
  }
  const key = `Services.instance.installCampusModpack.error.description.${reason}`;
  const fallback = t(`UpdateModpackModal.reason.${reason}`, {
    defaultValue: t(
      "Services.instance.installCampusModpack.error.description.UNKNOWN_ERROR"
    ),
  });
  return t(key, { defaultValue: fallback });
};
