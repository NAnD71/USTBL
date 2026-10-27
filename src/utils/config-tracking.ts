import { ChangeCounts, Checkpoint, Semantic } from "@/models/config-tracking";

const CHECKPOINT_LABELS: Record<Checkpoint, string> = {
  before_launch: "checkpoint.before_launch",
  launcher_ready: "checkpoint.launcher_ready",
  after_exit: "checkpoint.after_exit",
  manual_scan: "checkpoint.manual_scan",
};

const SEMANTIC_LABELS: Record<Semantic, string> = {
  initial_observation: "semantic.initial_observation",
  during_loading: "semantic.during_loading",
  during_runtime: "semantic.during_runtime",
  between_sessions: "semantic.between_sessions",
  interrupted_or_unknown: "semantic.interrupted_or_unknown",
  ambiguous: "semantic.ambiguous",
};

export const checkpointLabelKey = (checkpoint: Checkpoint): string =>
  CHECKPOINT_LABELS[checkpoint];

export const semanticLabelKey = (semantic: Semantic): string =>
  SEMANTIC_LABELS[semantic];

export const formatChangeCounts = (
  counts: ChangeCounts
): Record<keyof ChangeCounts, string> => ({
  added: counts.added.toLocaleString(),
  modified: counts.modified.toLocaleString(),
  deleted: counts.deleted.toLocaleString(),
  uncertain: counts.uncertain.toLocaleString(),
});

export const totalHistoryBytes = (logBytes: number, objectBytes: number) =>
  Math.max(0, logBytes) + Math.max(0, objectBytes);

export const formatHistoryBytes = (bytes: number): string => {
  const value = Math.max(0, bytes);
  const units = ["B", "KB", "MB", "GB"];
  if (value < 1024) return `${value} B`;
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), 3);
  return `${(value / 1024 ** index).toFixed(2)} ${units[index]}`;
};
