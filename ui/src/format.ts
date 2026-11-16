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

export function shouldShowConnectionDetails(state: AccountState): boolean {
  return state === "connected";
}

export function settingsEqual(draft: Settings, persisted: Settings): boolean {
  return (
    draft.repoNamespace === persisted.repoNamespace &&
    draft.repoName === persisted.repoName &&
    Number(draft.localPort) === Number(persisted.localPort) &&
    draft.minimizeToTray === persisted.minimizeToTray &&
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

export function formatCreditsUsage(usedMilli: number, totalMilli: number): string {
  if (!Number.isFinite(usedMilli) || !Number.isFinite(totalMilli) || totalMilli <= 0) {
    return "-- / --";
  }
  const used = Math.max(0, usedMilli) / 1000;
  const total = totalMilli / 1000;
  const percent = ((used / total) * 100)
    .toFixed(1)
    .replace(/\.0$/, "");
  return `${used} / ${total} Credits (${percent}%)`;
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
