import { ModpackDiffHunk } from "@/models/modpack-update";

export type Checkpoint =
  | "before_launch"
  | "launcher_ready"
  | "after_exit"
  | "manual_scan";

export type Semantic =
  | "initial_observation"
  | "during_loading"
  | "during_runtime"
  | "between_sessions"
  | "interrupted_or_unknown"
  | "ambiguous";

export type ReadState = "stable" | "unstable" | "unreadable" | "too_large";

export interface FileEntry {
  path: string;
  sha1: string | null;
  size: number;
  state: ReadState;
}

export interface ProcessExit {
  code: number | null;
  success: boolean;
}

export interface ChangeCounts {
  added: number;
  modified: number;
  deleted: number;
  uncertain: number;
}

export interface TimelineEntry {
  id: number;
  previousId: number | null;
  at: string;
  checkpoint: Checkpoint;
  semantic: Semantic;
  sessionId: string | null;
  instanceId: string | null;
  exit: ProcessExit | null;
  configPresent: boolean;
  fileCount: number;
  changes: ChangeCounts | null;
  incomplete: boolean;
}

export interface HistoryStats {
  entries: number;
  logBytes: number;
  objectBytes: number;
}

export interface Timeline {
  schema: number;
  entries: TimelineEntry[];
  diagnostics: string[];
  stats: HistoryStats;
}

export interface ConfigTrackingTimeline {
  gameRoot: string;
  sharedRoot: boolean;
  timeline: Timeline;
}

export type ChangeKind = "added" | "modified" | "deleted" | "uncertain";

export interface FileDiffText {
  type: "text";
  hunks: ModpackDiffHunk[];
  normalizedEqual: boolean;
}

export interface FileDiffBinary {
  type: "binary";
}

export interface FileDiffUnavailable {
  type: "unavailable";
  reason: string;
}

export type FileDiffBody = FileDiffText | FileDiffBinary | FileDiffUnavailable;

export interface FileChange {
  path: string;
  kind: ChangeKind;
  old: FileEntry | null;
  new: FileEntry | null;
  body: FileDiffBody;
}

export interface EntryChanges {
  schema: number;
  id: number;
  previousId: number | null;
  semantic: Semantic;
  files: FileChange[];
}
