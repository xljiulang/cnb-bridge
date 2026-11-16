export type AccountState = "signedOut" | "signingIn" | "connected" | "failed";
export type AuthMethod = "oauth" | "personalAccessToken";

export type BridgeState =
  | "signedOut"
  | "signingIn"
  | "preparingKey"
  | "preparingRepo"
  | "startingWorkspace"
  | "connecting"
  | "idle"
  | "ready"
  | "needsSshKey"
  | "needsRepoCreation"
  | "stopping"
  | "failed";

export interface AccountStatus {
  state: AccountState;
  username?: string | null;
  authMethod?: AuthMethod | null;
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
  repoCreationUrl?: string | null;
  error?: string | null;
}

export interface Settings {
  repoNamespace: string;
  repoName: string;
  localPort: number;
  minimizeToTray: boolean;
  launchAtLogin: boolean;
}

export interface Namespace {
  name: string;
  path: string;
}

export interface ModelSummary {
  id: string;
  displayName: string;
  description?: string | null;
  contextWindow: number;
  inputModalities: string[];
  supportsTools: boolean;
  supportsStreaming: boolean;
  supportsReasoningSummaries: boolean;
}

export interface AppSnapshot {
  account: AccountStatus;
  bridge: BridgeSnapshot;
  settings: Settings;
  version: string;
  logsPath: string;
  httpLogsPath: string;
  models: ModelSummary[];
}

export interface LogEntry {
  time: string;
  level: string;
  message: string;
}

export interface HttpTraceSummary {
  id: string;
  startedAt: string;
  method: string;
  path: string;
  status: number;
  elapsedMs: number;
  protocol: string;
  stream: boolean;
  requestBytes: number;
  responseBytes: number;
  requestTruncated: boolean;
  responseTruncated: boolean;
  result: string;
}

export interface CommandError {
  code?: string;
  message?: string;
}
