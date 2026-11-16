export type AccountState = "signedOut" | "signingIn" | "connected" | "failed";

export type BridgeState =
  | "signedOut"
  | "signingIn"
  | "preparingKey"
  | "preparingRepo"
  | "startingWorkspace"
  | "connecting"
  | "ready"
  | "needsSshKey"
  | "stopping"
  | "failed";

export interface AccountStatus {
  state: AccountState;
  username?: string | null;
  verificationUrl?: string | null;
  userCode?: string | null;
  error?: string | null;
}

export interface BridgeSnapshot {
  state: BridgeState;
  account: AccountStatus;
  username?: string | null;
  repoSlug?: string | null;
  workspaceSn?: string | null;
  localPort: number;
  baseUrl?: string | null;
  sshPublicKey?: string | null;
  error?: string | null;
}

export interface Settings {
  repoName: string;
  localPort: number;
  minimizeToTray: boolean;
  launchAtLogin: boolean;
}

export interface AppSnapshot {
  account: AccountStatus;
  bridge: BridgeSnapshot;
  settings: Settings;
  version: string;
  logsPath: string;
}

export interface LogEntry {
  time: string;
  level: string;
  message: string;
}

export interface CommandError {
  code?: string;
  message?: string;
}
