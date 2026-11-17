import type {
  AccountState,
  AppSnapshot,
  BridgeState,
  CommandError,
  Settings,
} from "./types";

const BRIDGE_LABELS: Record<BridgeState, string> = {
  signedOut: "未登录",
  signingIn: "等待授权",
  preparingKey: "准备 SSH 密钥",
  preparingRepo: "准备仓库",
  startingWorkspace: "启动云主机",
  connecting: "建立 SSH 通道",
  idle: "空闲待命",
  ready: "桥接就绪",
  needsSshKey: "需要注册 SSH 公钥",
  needsRepoCreation: "需要创建仓库",
  stopping: "正在关闭",
  failed: "启动失败",
};

export function bridgeLabel(state: BridgeState): string {
  return BRIDGE_LABELS[state] ?? state;
}

export function isReady(snapshot: AppSnapshot | null): boolean {
  return snapshot?.bridge.state === "ready";
}

export function isBridgeTransitioning(state: BridgeState): boolean {
  return (
    state === "preparingKey" ||
    state === "preparingRepo" ||
    state === "startingWorkspace" ||
    state === "connecting" ||
    state === "stopping"
  );
}

export type StatusIconKind =
  | "scan"
  | "live"
  | "busy"
  | "pause"
  | "error"
  | "server";

export function statusIconKind(
  state: BridgeState,
  accountConnected: boolean,
  creatingOrganization: boolean,
): StatusIconKind {
  if (!accountConnected) return "scan";
  if (state === "ready") return "live";
  if (creatingOrganization || isBridgeTransitioning(state)) return "busy";
  if (state === "idle") return "pause";
  if (
    state === "failed" ||
    state === "needsSshKey" ||
    state === "needsRepoCreation"
  ) {
    return "error";
  }
  return "server";
}

export function shouldShowConnectionDetails(
  state: AccountState,
  needsOrganization = false,
): boolean {
  return state === "connected" && !needsOrganization;
}

export function settingsEqual(draft: Settings, persisted: Settings): boolean {
  return (
    draft.repoNamespace === persisted.repoNamespace &&
    draft.repoName === persisted.repoName &&
    Number(draft.localPort) === Number(persisted.localPort) &&
    draft.httpLogFilter === persisted.httpLogFilter &&
    draft.launchAtLogin === persisted.launchAtLogin
  );
}

export function formatContextWindow(tokens: number): string {
  if (tokens >= 1024 * 1024 && tokens % (1024 * 1024) === 0) {
    return `${tokens / (1024 * 1024)}M 上下文`;
  }
  if (tokens >= 1024 && tokens % 1024 === 0) {
    return `${tokens / 1024}K 上下文`;
  }
  return `${tokens} 上下文`;
}

export function formatInputModalities(modalities: string[]): string {
  const labels: Record<string, string> = {
    text: "文本",
    image: "图像",
    audio: "音频",
    video: "视频",
  };
  return modalities.map((modality) => labels[modality] ?? modality).join(" / ");
}

export type CreditsUsageTone = "normal" | "warning" | "danger" | "unavailable";

function creditsUsagePercentValue(
  usedMilli: number,
  totalMilli: number,
): number | null {
  if (!Number.isFinite(usedMilli) || !Number.isFinite(totalMilli) || totalMilli <= 0) {
    return null;
  }
  const used = Math.max(0, usedMilli) / 1000;
  const total = totalMilli / 1000;
  return (used / total) * 100;
}

export function formatCreditsAmount(
  usedMilli: number,
  totalMilli: number,
): string {
  if (!Number.isFinite(usedMilli) || !Number.isFinite(totalMilli) || totalMilli <= 0) {
    return "-- / --";
  }
  const used = Math.max(0, usedMilli) / 1000;
  const total = totalMilli / 1000;
  return `${used} / ${total} Credits`;
}

export function formatCreditsPercent(
  usedMilli: number,
  totalMilli: number,
): string {
  const percent = creditsUsagePercentValue(usedMilli, totalMilli);
  if (percent === null) return "--";
  return `${percent
    .toFixed(1)
    .replace(/\.0$/, "")}%`;
}

const coreHoursFormatter = new Intl.NumberFormat("zh-CN", {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

function formatCoreHours(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "--";
  return coreHoursFormatter.format(seconds / 3600);
}

export function formatDevCpuUsage(
  usedSeconds: number,
  totalSeconds: number,
): string {
  return `${formatCoreHours(usedSeconds)} / ${formatCoreHours(
    totalSeconds,
  )} 核时`;
}

export function creditsUsageProgress(
  usedMilli: number,
  totalMilli: number,
): number | null {
  const percent = creditsUsagePercentValue(usedMilli, totalMilli);
  if (percent === null) return null;
  return Math.round(Math.min(100, Math.max(0, percent)) * 10) / 10;
}

export function creditsUsageTone(
  usedMilli: number,
  totalMilli: number,
): CreditsUsageTone {
  const percent = creditsUsagePercentValue(usedMilli, totalMilli);
  if (percent === null) return "unavailable";
  if (percent >= 100) return "danger";
  if (percent >= 80) return "warning";
  return "normal";
}

export function validatePort(value: number): string | null {
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    return "端口必须是 1–65535 之间的整数";
  }
  return null;
}

export function formatError(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object") {
    const candidate = error as CommandError;
    if (candidate.message) return candidate.message;
  }
  return String(error);
}
