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
  accounts: [],
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
const signedOutSnapshot = {
  ...snapshot,
  account: {
    ...account,
    state: "signedOut",
    username: null,
  },
  bridge: {
    ...snapshot.bridge,
    state: "signedOut",
    account: {
      ...account,
      state: "signedOut",
      username: null,
    },
    username: null,
  },
  accounts: [
    {
      username: "alice",
      lastLoginAt: 200,
      active: false,
    },
    {
      username: "bob",
      lastLoginAt: 100,
      active: false,
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
            creditsUsedMilli: 214_720,
            creditsTotalMilli: 1_166_000,
            fetchedAt: "2026-11-17T00:00:00Z",
          };
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

function mockSettingsIpc(namespaces: { name: string; path: string }[]) {
  mockWindows("main");
  mockIPC(
    (command, args) => {
      switch (command) {
        case "get_snapshot":
          return snapshot;
        case "save_settings": {
          const next = (args as { settings: typeof snapshot.settings }).settings;
          return { ...snapshot, settings: next };
        }
        case "get_logs":
          return [];
        case "list_http_traces":
          return [];
        case "list_namespaces":
          return namespaces;
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
  it("opens the identity verification action from the error banner", async () => {
    const calls: { command: string; args: unknown }[] = [];
    const failedSnapshot = {
      ...snapshot,
      account: {
        ...account,
        state: "failed",
        error: "当前 CNB 账号尚未完成实名认证",
        errorAction: {
          label: "实名认证 · 个人设置",
          url: "https://cnb.cool/profile/auth",
        },
      },
    };
    mockWindows("main");
    mockIPC(
      (command, args) => {
        calls.push({ command, args });
        switch (command) {
          case "get_snapshot":
            return failedSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    const action = wrapper.find(".identity-verification-action");
    expect(action.text()).toContain("实名认证 · 个人设置");
    await action.trigger("click");
    await flushPromises();
    expect(
      calls.find((call) => call.command === "open_external")?.args,
    ).toEqual({ url: "https://cnb.cool/profile/auth" });
    wrapper.unmount();
  });

  it("shows AI Credits amount and progress percentage separately", async () => {
    mockSnapshotIpc();
    const wrapper = mount(App);
    await flushPromises();

    expect(wrapper.find(".usage-amount").text()).toBe(
      "214.72 / 1166 Credits",
    );
    expect(wrapper.find(".usage-amount").text()).not.toContain("18.4%");
    expect(wrapper.find(".usage-percent").text()).toBe("18.4%");
    const progress = wrapper.get('.usage-progress-track[role="progressbar"]');
    expect(progress.attributes("aria-valuenow")).toBe("18.4");
    expect(
      (progress.get(".usage-progress-fill").element as HTMLElement).style.width,
    ).toBe("18.4%");
    wrapper.unmount();
  });

  it("shows a direct scan action when no account is saved", async () => {
    const emptySnapshot = { ...signedOutSnapshot, accounts: [] };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return emptySnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    expect(wrapper.find(".empty-account-title").text()).toContain("登录 CNB");
    expect(wrapper.find(".account-picker-trigger").exists()).toBe(false);
    expect(wrapper.find(".account-actions .primary").text()).toContain(
      "扫码绑定 CNB",
    );
    wrapper.unmount();
  });

  it("logs in with a selected saved account and confirms removal", async () => {
    const calls: { command: string; args: unknown }[] = [];
    vi.spyOn(window, "confirm").mockReturnValue(true);
    mockWindows("main");
    mockIPC(
      (command, args) => {
        calls.push({ command, args });
        switch (command) {
          case "get_snapshot":
            return signedOutSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    expect(wrapper.find(".account-main > .account-avatar").exists()).toBe(false);
    await wrapper.find(".account-picker-trigger").trigger("click");
    const accountRows = [
      ...document.body.querySelectorAll<HTMLElement>(".account-menu-row"),
    ];
    expect(accountRows).toHaveLength(2);
    expect(accountRows[0].textContent).toContain("alice");

    (accountRows[0].querySelector(".account-choice") as HTMLElement).click();
    await flushPromises();
    expect(
      calls.find((call) => call.command === "login_with_account")?.args,
    ).toEqual({ username: "alice" });

    await wrapper.findAll("nav button")[1].trigger("click");
    await flushPromises();
    expect(
      wrapper.find('input[spellcheck="false"]').attributes("disabled"),
    ).toBeDefined();
    expect(wrapper.find(".select-trigger").attributes("disabled")).toBeDefined();

    await wrapper.findAll("nav button")[0].trigger("click");
    await flushPromises();
    await wrapper.find(".account-picker-trigger").trigger("click");
    const removeButtons = [
      ...document.body.querySelectorAll<HTMLElement>(".remove-saved-account"),
    ];
    removeButtons[1].click();
    await flushPromises();
    expect(window.confirm).toHaveBeenCalledOnce();
    expect(
      calls.find((call) => call.command === "remove_account")?.args,
    ).toEqual({ username: "bob" });

    wrapper.unmount();
  });

  it("switches between all views through the sidebar", async () => {
    mockSnapshotIpc();
    const wrapper = mount(App);
    await flushPromises();
    expect(wrapper.find(".account-main").text()).toContain("tester");

    await wrapper.findAll("nav button")[1].trigger("click");
    await flushPromises();
    expect(wrapper.find(".settings-card").exists()).toBe(true);
    expect(wrapper.findAll(".settings-card")).toHaveLength(4);

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

  it("marks unsaved settings and clears the badge after saving", async () => {
    mockSettingsIpc([]);
    const wrapper = mount(App);
    await flushPromises();

    await wrapper.findAll("nav button")[1].trigger("click");
    await flushPromises();
    expect(wrapper.find(".badge.warn").exists()).toBe(false);

    await wrapper.find('.switch-row input[type="checkbox"]').setValue(true);
    await flushPromises();
    expect(wrapper.find(".badge.warn").text()).toBe("未保存");

    await wrapper.find(".page-head button").trigger("click");
    await flushPromises();
    expect(wrapper.find(".badge.warn").exists()).toBe(false);
    expect(wrapper.find(".page-head button").text()).toContain("已保存");

    wrapper.unmount();
  });

  it("shows a namespace once when its name and path match", async () => {
    mockSettingsIpc([{ name: "xliulang", path: "xliulang" }]);
    const wrapper = mount(App);
    await flushPromises();

    await wrapper.findAll("nav button")[1].trigger("click");
    await flushPromises();

    await wrapper.find(".select-trigger").trigger("click");
    await flushPromises();

    const labels = [...document.body.querySelectorAll(".select-option")].map(
      (option) => option.textContent?.trim() ?? "",
    );
    expect(labels).toContain("xliulang");
    expect(labels.some((label) => label.includes("（xliulang）"))).toBe(false);

    wrapper.unmount();
  });
});
