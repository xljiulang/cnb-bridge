import { describe, expect, it } from "vitest";
import {
  bridgeLabel,
  formatError,
  formatContextWindow,
  formatCreditsUsage,
  formatInputModalities,
  isBridgeTransitioning,
  statusIconKind,
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
    expect(bridgeLabel("idle")).toBe("空闲待命");
  });

  it("selects a distinct status icon for each sidebar state", () => {
    expect(statusIconKind("signedOut", false, false)).toBe("scan");
    expect(statusIconKind("idle", true, false)).toBe("pause");
    expect(statusIconKind("ready", true, false)).toBe("live");
    expect(statusIconKind("connecting", true, false)).toBe("busy");
    expect(statusIconKind("failed", true, false)).toBe("error");
    expect(statusIconKind("needsRepoCreation", true, false)).toBe("error");
    expect(statusIconKind("preparingKey", true, true)).toBe("busy");
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

  it("formats total AI Credit usage and ignores surplus percentage", () => {
    expect(formatCreditsUsage(22_540, 1_166_000)).toBe("22.54 / 1166 Credits (1.9%)");
    expect(formatCreditsUsage(1_500, 1_000)).toBe("1.5 / 1 Credits (150%)");
    expect(formatCreditsUsage(0, 0)).toBe("-- / --");
  });
});
