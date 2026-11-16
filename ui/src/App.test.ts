import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import App from "./App.vue";

const account = {
  state: "connected",
  username: "tester",
  authMethod: "personalAccessToken",
  error: null,
  needsOrganization: false,
};
const snapshot = {
  account,
  bridge: {
    state: "idle",
    account,
    username: "tester",
    repoSlug: "tester/cnb-ai-bridge",
    workspaceSn: null,
    localPort: 11233,
    baseUrl: "http://127.0.0.1:11233/v1",
    sshPublicKey: null,
    repoCreationUrl: null,
    error: null,
  },
  settings: {
    repoNamespace: "tester",
    repoName: "cnb-ai-bridge",
    localPort: 11233,
    httpLogFilter: "abnormal",
    launchAtLogin: false,
  },
  version: "0.1.0",
  logsPath: "logs",
  httpLogsPath: "http-logs",
  models: [
    {
      id: "deepseek-v4.1-flash",
      displayName: "DeepSeek V4.1 Flash",
      description: null,
      contextWindow: 1048576,
      inputModalities: ["text", "image"],
      supportsTools: true,
      supportsStreaming: true,
      supportsReasoningSummaries: true,
    },
  ],
};

afterEach(() => {
  clearMocks();
  vi.useRealTimers();
});

function mockSnapshotIpc() {
  mockWindows("main");
  mockIPC(
    (command) => {
      switch (command) {
        case "get_snapshot":
          return snapshot;
        case "get_logs":
          return [{ time: "2026-11-17 01:00:00", level: "info", message: "ready" }];
        case "list_http_traces":
        case "list_namespaces":
          return [];
        case "get_cnb_usage":
          return {
            namespace: "tester",
            month: "2026-11",
            creditsUsedMilli: 0,
            creditsTotalMilli: 1000,
            fetchedAt: "2026-11-17T00:00:00Z",
          };
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

describe("App shell", () => {
  it("switches between all views through the sidebar", async () => {
    mockSnapshotIpc();
    const wrapper = mount(App);
    await flushPromises();
    expect(wrapper.find(".account-main").text()).toContain("tester");

    await wrapper.findAll("nav button")[1].trigger("click");
    await flushPromises();
    expect(wrapper.find(".settings-card").exists()).toBe(true);

    await wrapper.findAll("nav button")[2].trigger("click");
    await flushPromises();
    expect(wrapper.find(".log-row").text()).toContain("ready");

    await wrapper.findAll("nav button")[3].trigger("click");
    await flushPromises();
    expect(wrapper.find(".details-card").text()).toContain("作者");
    wrapper.unmount();
  });

  it("shows in-place copy feedback for both endpoint and model id", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    vi.useFakeTimers();
    mockSnapshotIpc();

    const wrapper = mount(App);
    await flushPromises();

    const copyButtons = wrapper.findAll(".copy-button");
    expect(copyButtons).toHaveLength(2);

    await copyButtons[0].trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("deepseek-v4.1-flash");
    expect(copyButtons[0].text()).toContain("已复制");
    expect(wrapper.find(".banner.notice").exists()).toBe(false);

    await copyButtons[1].trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("http://127.0.0.1:11233/v1");
    expect(copyButtons[1].text()).toContain("已复制");
    expect(copyButtons[0].text()).toContain("复制 ID");
    expect(wrapper.find(".banner.notice").exists()).toBe(false);

    await vi.advanceTimersByTimeAsync(2500);
    await flushPromises();
    expect(copyButtons[1].text()).toContain("复制地址");

    wrapper.unmount();
  });
});
