import { invoke } from "@tauri-apps/api/core";
import { InvokeResponse } from "@/models/response";
import { MemoryInfo } from "@/models/system-info";
import { responseHandler } from "@/utils/response";

/**
 * UtilsService class for general utility functions (system info, fonts, services).
 */
export class UtilsService {
  /**
   * RETRIEVE the memory info of the system.
   * @returns {Promise<InvokeResponse<MemoryInfo>>}
   */
  @responseHandler("utils")
  static async retrieveMemoryInfo(): Promise<InvokeResponse<MemoryInfo>> {
    return await invoke("retrieve_memory_info");
  }

  /**
   * DELETE a mod file belonging to the given instance.
   * @param instanceId the instance that owns the mod.
   * @param path the file to delete.
   * @returns {Promise<InvokeResponse<void>>}
   */
  @responseHandler("utils")
  static async deleteFile(
    instanceId: string,
    path: string
  ): Promise<InvokeResponse<void>> {
    return await invoke("delete_file", { instanceId, path });
  }

  /**
   * OPEN a directory known to the launcher.
   */
  @responseHandler("utils")
  static async openDirectory(path: string): Promise<InvokeResponse<void>> {
    return await invoke("open_directory", { path });
  }

  /**
   * READ an image from an instance's screenshots directory.
   */
  @responseHandler("utils")
  static async readScreenshot(path: string): Promise<InvokeResponse<number[]>> {
    return await invoke("read_screenshot", { path });
  }

  /**
   * RETRIEVE the list of installed TrueType fonts.
   * @returns {Promise<InvokeResponse<string[]>>}
   */
  @responseHandler("utils")
  static async retrieveFontList(): Promise<InvokeResponse<string[]>> {
    return await invoke("retrieve_truetype_font_list");
  }

  /**
   * CHECK the availability of a given service URL.
   * @param url The URL to test.
   * @returns {Promise<InvokeResponse<number>>} Round-trip time in milliseconds.
   */
  @responseHandler("utils")
  static async checkServiceAvailability(
    url: string
  ): Promise<InvokeResponse<number>> {
    return await invoke("check_service_availability", { url });
  }
}
