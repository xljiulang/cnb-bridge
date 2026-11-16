import { invoke } from "@tauri-apps/api/core";
import type {
  AccountStatus,
  AppSnapshot,
  LogEntry,
  Namespace,
  Settings,
} from "./types";

export const getSnapshot = () => invoke<AppSnapshot>("get_snapshot");

export const saveSettings = (settings: Settings) =>
  invoke<AppSnapshot>("save_settings", { settings });

export const startLogin = () => invoke<AccountStatus>("start_login");

export const cancelLogin = () => invoke<AccountStatus>("cancel_login");

export const logout = () => invoke<AccountStatus>("logout");

export const startBridge = () => invoke<AppSnapshot>("start_bridge");

export const stopBridge = () => invoke<AppSnapshot>("stop_bridge");

export const ensureSshKey = () => invoke<string>("ensure_ssh_key");

export const regenerateSshKey = () => invoke<string>("regenerate_ssh_key");

export const listNamespaces = () => invoke<Namespace[]>("list_namespaces");

export const openSshKeysPage = () => invoke<void>("open_ssh_keys_page");

export const openExternal = (url: string) =>
  invoke<void>("open_external", { url });

export const openLogsFolder = () => invoke<void>("open_logs_folder");

export const getLogs = (limit = 200) =>
  invoke<LogEntry[]>("get_logs", { limit });
