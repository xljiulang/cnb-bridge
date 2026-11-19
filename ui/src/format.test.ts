import { describe, expect, it } from "vitest";
import {
  bridgeLabel,
  creditsUsageProgress,
  creditsUsageTone,
  formatCreditsAmount,
  formatCreditsPercent,
  formatDevCpuUsage,
  formatError,
  formatContextWindow,
  formatInputModalities,
  isBridgeTransitioning,
  settingsEqual,
  shouldShowConnectionDetails,
  validatePort,
} from "./format";
import type { Settings } from "./types";

const savedSettings: Settings = {
  repoNamespace: "owner",
  repoName: "cnb-ai-bridge",
  localPort: 11233,
  httpLogFilter: "abnormal",
  launchAtLogin: false,
};

describe("format helpers", () => {
  it("labels bridge states", () => {
    expect(bridgeLabel("ready")).toBe("桥接就绪");
    expect(bridgeLabel("needsSshKey")).toBe("需要注册 SSH 公钥");
    expect(bridgeLabel("needsRepoCreation")).toBe("需要创建仓库");
    expect(bridgeLabel("stopping")).toBe("正在断开");
    expect(bridgeLabel("failed")).toBe("桥接失败");
  });

  it("validates ports", () => {
    expect(validatePort(11233)).toBeNull();
    expect(validatePort(0)).not.toBeNull();
    expect(validatePort(70000)).not.toBeNull();
  });

  it("identifies bridge transitions that must block duplicate starts", () => {
    expect(isBridgeTransitioning("preparingKey")).toBe(true);
    expect(isBridgeTransitioning("preparingRepo")).toBe(true);
    expect(isBridgeTransitioning("startingWorkspace")).toBe(true);
    expect(isBridgeTransitioning("connecting")).toBe(true);
    expect(isBridgeTransitioning("stopping")).toBe(true);
    expect(isBridgeTransitioning("idle")).toBe(false);
    expect(isBridgeTransitioning("failed")).toBe(false);
    expect(isBridgeTransitioning("needsRepoCreation")).toBe(false);
    expect(isBridgeTransitioning("ready")).toBe(false);
  });

  it("labels the request-driven idle state", () => {
    expect(bridgeLabel("idle")).toBe("按需连接");
  });

  it("formats command errors", () => {
    expect(formatError({ message: "boom" })).toBe("boom");
    expect(formatError("plain")).toBe("plain");
  });

  it("only unlocks connection details for a connected CNB account", () => {
    expect(shouldShowConnectionDetails("connected", false)).toBe(true);
    expect(shouldShowConnectionDetails("connected", true)).toBe(false);
    expect(shouldShowConnectionDetails("signedOut")).toBe(false);
    expect(shouldShowConnectionDetails("signingIn")).toBe(false);
    expect(shouldShowConnectionDetails("failed")).toBe(false);
  });

  it("formats model context and input modalities for the model card", () => {
    expect(formatContextWindow(1_048_576)).toBe("1M 上下文");
    expect(formatContextWindow(131_072)).toBe("128K 上下文");
    expect(formatInputModalities(["text", "image"])).toBe("文本 / 图像");
    expect(formatInputModalities(["embedding"])).toBe("embedding");
  });

  it("detects settings changes and clears after values are restored", () => {
    const changedDraft = { ...savedSettings, repoName: "other-repo" };

    expect(settingsEqual(savedSettings, savedSettings)).toBe(true);
    expect(settingsEqual(changedDraft, savedSettings)).toBe(false);
    expect(settingsEqual({ ...changedDraft, repoName: savedSettings.repoName }, savedSettings)).toBe(
      true,
    );
  });

  it("compares numeric ports and desktop switches", () => {
    expect(settingsEqual({ ...savedSettings, localPort: 12000 }, savedSettings)).toBe(false);
    expect(settingsEqual({ ...savedSettings, httpLogFilter: "all" }, savedSettings)).toBe(false);
    expect(settingsEqual({ ...savedSettings, launchAtLogin: true }, savedSettings)).toBe(false);
  });

  it("formats AI Credit amount and percentage separately", () => {
    expect(formatCreditsAmount(22_540, 1_166_000)).toBe(
      "22.54 / 1166 Credits",
    );
    expect(formatCreditsPercent(22_540, 1_166_000)).toBe("1.9%");
    expect(formatCreditsAmount(1_500, 1_000)).toBe("1.5 / 1 Credits");
    expect(formatCreditsPercent(1_500, 1_000)).toBe("150%");
    expect(formatCreditsAmount(0, 0)).toBe("-- / --");
    expect(formatCreditsPercent(0, 0)).toBe("--");
  });

  it("formats monthly development CPU usage as core hours", () => {
    expect(formatDevCpuUsage(1_370_366, 5_760_000)).toBe(
      "380.66 / 1,600.00 核时",
    );
    expect(formatDevCpuUsage(0, 0)).toBe("0.00 / 0.00 核时");
    expect(formatDevCpuUsage(Number.NaN, 5_760_000)).toBe(
      "-- / 1,600.00 核时",
    );
  });

  it("clamps progress while preserving the displayed surplus percentage", () => {
    expect(creditsUsageProgress(0, 1_000)).toBe(0);
    expect(creditsUsageProgress(225_400, 1_166_000)).toBe(19.3);
    expect(creditsUsageProgress(800, 1_000)).toBe(80);
    expect(creditsUsageProgress(1_000, 1_000)).toBe(100);
    expect(creditsUsageProgress(1_500, 1_000)).toBe(100);
    expect(creditsUsageProgress(0, 0)).toBeNull();
    expect(creditsUsageProgress(Number.NaN, 1_000)).toBeNull();
  });

  it("assigns normal, warning, danger and unavailable usage tones", () => {
    expect(creditsUsageTone(18.4, 100)).toBe("normal");
    expect(creditsUsageTone(80, 100)).toBe("warning");
    expect(creditsUsageTone(100, 100)).toBe("danger");
    expect(creditsUsageTone(150, 100)).toBe("danger");
    expect(creditsUsageTone(0, 0)).toBe("unavailable");
  });
});
