import { describe, expect, it } from "vitest";
import {
  bridgeLabel,
  formatError,
  formatContextWindow,
  formatInputModalities,
  isBridgeTransitioning,
  shouldShowConnectionDetails,
  validatePort,
} from "./format";

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

  it("formats command errors", () => {
    expect(formatError({ message: "boom" })).toBe("boom");
    expect(formatError("plain")).toBe("plain");
  });

  it("only unlocks connection details for a connected CNB account", () => {
    expect(shouldShowConnectionDetails("connected")).toBe(true);
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
});
