import { t } from "i18next";
import { AccountServiceError } from "@/enums/service-error";
import { ResponseError } from "@/models/response";

// vUSTB currently reports an old launcher scope as a human-readable 403
// detail. Keep the server detail intact, but expose a stable client-side
// classification for all launcher features that consume the API.
const OUTDATED_AUTHORIZATION_MARKER = "当前启动器登录授权版本过旧";

export function isVustbAuthorizationOutdatedError(error: unknown): boolean {
  return String(error ?? "").includes(OUTDATED_AUTHORIZATION_MARKER);
}

export function classifyVustbError(error: unknown): string {
  const rawError = String(error ?? "");
  return isVustbAuthorizationOutdatedError(rawError)
    ? AccountServiceError.AuthorizationOutdated
    : rawError;
}

export function isVustbAuthorizationOutdatedResponse(
  response: ResponseError
): boolean {
  return response.raw_error === AccountServiceError.AuthorizationOutdated;
}

export function getVustbErrorNotification(response: ResponseError) {
  if (isVustbAuthorizationOutdatedResponse(response)) {
    return {
      title: t("Services.account.authorizationOutdated.title", {
        defaultValue: "需要更新授权",
      }),
      description:
        response.details ||
        t("Services.account.authorizationOutdated.description", {
          defaultValue: "请重新登录像素北科账号后重试。",
        }),
      status: "warning" as const,
    };
  }

  return {
    title: response.details || response.message,
    status: "error" as const,
  };
}

export function getVustbErrorMessage(response: ResponseError): string {
  if (isVustbAuthorizationOutdatedResponse(response)) {
    return `${t("Services.account.authorizationOutdated.title", {
      defaultValue: "需要更新授权",
    })}：${
      response.details ||
      t("Services.account.authorizationOutdated.description", {
        defaultValue: "请重新登录像素北科账号后重试。",
      })
    }`;
  }

  return response.details || response.message;
}
