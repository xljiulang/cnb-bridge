import { describe, expect, it } from "vitest";
import { bridgeLabel, formatError, shortenKey, validatePort } from "./format";

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

  it("formats command errors", () => {
    expect(formatError({ message: "boom" })).toBe("boom");
    expect(formatError("plain")).toBe("plain");
  });

  it("shortens long keys", () => {
    const key = `ssh-ed25519 ${"A".repeat(80)} cnb`;
    expect(shortenKey(key)).toContain("…");
  });
});
