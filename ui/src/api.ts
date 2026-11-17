import { invoke } from "@tauri-apps/api/core";
import type {
  AccountStatus,
  AppSnapshot,
  CnbUsageSummary,
  HttpTraceSummary,
  LogEntry,
  Namespace,
  Settings,
} from "./types";

export const getSnapshot = () => invoke<AppSnapshot>("get_snapshot");

export const dismissError = () => invoke<AppSnapshot>("dismiss_error");

export const saveSettings = (settings: Settings) =>
  invoke<AppSnapshot>("save_settings", { settings });

export const startLogin = () => invoke<AccountStatus>("start_login");

export const loginWithAccount = (username: string) =>
  invoke<AccountStatus>("login_with_account", { username });

export const logout = () => invoke<AccountStatus>("logout");

export const removeAccount = (username: string) =>
  invoke<AccountStatus>("remove_account", { username });

export const startBridge = () => invoke<AppSnapshot>("start_bridge");

export const createCnbNamespace = () =>
  invoke<AccountStatus>("create_cnb_namespace");

export const stopBridge = () => invoke<AppSnapshot>("stop_bridge");

export const listNamespaces = () => invoke<Namespace[]>("list_namespaces");

export const getCnbUsage = () => invoke<CnbUsageSummary>("get_cnb_usage");

export const openExternal = (url: string) =>
  invoke<void>("open_external", { url });

export const openLogsFolder = () => invoke<void>("open_logs_folder");

export const getLogs = (limit = 200) =>
  invoke<LogEntry[]>("get_logs", { limit });

export const listHttpTraces = (limit = 100) =>
  invoke<HttpTraceSummary[]>("list_http_traces", { limit });

export const openHttpTrace = (id: string) =>
  invoke<void>("open_http_trace", { id });
