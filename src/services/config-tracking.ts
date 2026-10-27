import { invoke } from "@tauri-apps/api/core";
import {
  ConfigTrackingTimeline,
  EntryChanges,
  TimelineEntry,
} from "@/models/config-tracking";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export class ConfigTrackingService {
  @responseHandler("instance")
  static async getTimeline(
    instanceId: string
  ): Promise<InvokeResponse<ConfigTrackingTimeline>> {
    return await invoke("get_config_tracking_timeline", { instanceId });
  }

  @responseHandler("instance")
  static async getEntryChanges(
    instanceId: string,
    entryId: number
  ): Promise<InvokeResponse<EntryChanges>> {
    return await invoke("get_config_tracking_entry_changes", {
      instanceId,
      entryId,
    });
  }

  @responseHandler("instance")
  static async scan(
    instanceId: string
  ): Promise<InvokeResponse<TimelineEntry>> {
    return await invoke("scan_config_tracking", { instanceId });
  }
}
