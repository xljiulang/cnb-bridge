import type { AppSnapshot, BridgeState, CommandError } from "./types";

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
