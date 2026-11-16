import { describe, expect, it } from "vitest";
import {
  bridgeLabel,
  formatError,
  isBridgeTransitioning,
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
    expect(isBridgeTransitioning("reconnecting")).toBe(true);
    expect(isBridgeTransitioning("failed")).toBe(false);
    expect(isBridgeTransitioning("needsRepoCreation")).toBe(false);
    expect(isBridgeTransitioning("ready")).toBe(false);
  });

  it("labels the automatic recovery state", () => {
    expect(bridgeLabel("reconnecting")).toBe("正在恢复连接");
  });

  it("formats command errors", () => {
    expect(formatError({ message: "boom" })).toBe("boom");
    expect(formatError("plain")).toBe("plain");
  });
});
