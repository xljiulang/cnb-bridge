import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import App from "./App.vue";

const account = {
  state: "connected",
  username: "tester",
  displayName: "Tester 昵称",
  avatarUrl: "https://cnb.cool/users/tester/avatar/l",
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
  version: "1.0.0",
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
    displayName: null,
    avatarUrl: null,
  },
  bridge: {
    ...snapshot.bridge,
    state: "signedOut",
    account: {
      ...account,
      state: "signedOut",
      username: null,
      displayName: null,
      avatarUrl: null,
    },
    username: null,
  },
  accounts: [
    {
      username: "alice",
      displayName: "爱丽丝",
      avatarUrl: "https://cnb.cool/users/alice/avatar/l",
      lastLoginAt: 200,
      active: false,
    },
    {
      username: "bob",
      displayName: "鲍勃",
      avatarUrl: null,
      lastLoginAt: 100,
      active: false,
    },
  ],
};

afterEach(() => {
  clearMocks();
  vi.useRealTimers();
});

function mockSnapshotIpc(current: unknown = snapshot) {
  mockWindows("main");
  mockIPC(
    (command) => {
      switch (command) {
        case "get_snapshot":
          return current;
        case "get_logs":
          return [{ time: "2026-11-17 01:00:00", level: "info", message: "ready" }];
        case "list_http_traces":
        case "list_namespaces":
          return [];
        case "get_cnb_usage":
          return {
            namespace: "tester",
            month: "2026-11",
            devCpuUsedSeconds: 1_370_366,
            devCpuTotalSeconds: 5_760_000,
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
            devCpuUsedSeconds: 0,
            devCpuTotalSeconds: 0,
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

  it("keeps a dismissed error hidden across snapshot refreshes", async () => {
    const calls: string[] = [];
    const failedSnapshot = {
      ...snapshot,
      account: {
        ...account,
        state: "failed",
        error: "扫码窗口已关闭",
      },
    };
    const dismissedSnapshot = {
      ...failedSnapshot,
      account: {
        ...failedSnapshot.account,
        error: null,
      },
    };
    let dismissed = false;
    let nextError: string | null = null;
    vi.useFakeTimers();
    mockWindows("main");
    mockIPC(
      (command) => {
        calls.push(command);
        switch (command) {
          case "get_snapshot":
            if (nextError) {
              return {
                ...dismissedSnapshot,
                account: { ...dismissedSnapshot.account, error: nextError },
              };
            }
            return dismissed ? dismissedSnapshot : failedSnapshot;
          case "dismiss_error":
            dismissed = true;
            return dismissedSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    expect(wrapper.text()).toContain("扫码窗口已关闭");
    await wrapper.get('[aria-label="关闭错误"]').trigger("click");
    await flushPromises();
    expect(calls).toContain("dismiss_error");
    expect(wrapper.text()).not.toContain("扫码窗口已关闭");

    await vi.advanceTimersByTimeAsync(1600);
    await flushPromises();
    expect(wrapper.text()).not.toContain("扫码窗口已关闭");

    nextError = "新的扫码错误";
    await emit("cnb-bridge://snapshot");
    await flushPromises();
    expect(wrapper.text()).toContain("新的扫码错误");
    wrapper.unmount();
  });

  it("refreshes logs without polling snapshots and refreshes snapshots when visible", async () => {
    let snapshotCalls = 0;
    let logCalls = 0;
    vi.useFakeTimers();
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            snapshotCalls += 1;
            return snapshot;
          case "get_logs":
            logCalls += 1;
            return [];
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();
    const initialCalls = snapshotCalls;

    await vi.advanceTimersByTimeAsync(1_600);
    await flushPromises();
    expect(snapshotCalls).toBe(initialCalls);

    const logsButton = wrapper
      .findAll("button")
      .find((button) => button.text().trim() === "日志");
    expect(logsButton).toBeDefined();
    await logsButton!.trigger("click");
    await flushPromises();
    const snapshotCallsBeforeTimer = snapshotCalls;
    const logCallsBeforeTimer = logCalls;

    await vi.advanceTimersByTimeAsync(30_000);
    await flushPromises();
    expect(snapshotCalls).toBe(snapshotCallsBeforeTimer);
    expect(logCalls).toBeGreaterThan(logCallsBeforeTimer);

    const hiddenSnapshotCalls = snapshotCalls;
    const hiddenLogCalls = logCalls;
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "hidden",
    });
    await vi.advanceTimersByTimeAsync(30_000);
    await flushPromises();
    expect(snapshotCalls).toBe(hiddenSnapshotCalls);
    expect(logCalls).toBe(hiddenLogCalls);

    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    await flushPromises();
    expect(snapshotCalls).toBeGreaterThan(hiddenSnapshotCalls);
    expect(logCalls).toBe(hiddenLogCalls);
    wrapper.unmount();
  });

  it("shows startup progress immediately and prevents duplicate starts", async () => {
    let startCalls = 0;
    let started = false;
    let resolveStart!: (value: typeof snapshot) => void;
    const pendingStart = new Promise<typeof snapshot>((resolve) => {
      resolveStart = resolve;
    });
    const readySnapshot = {
      ...snapshot,
      bridge: { ...snapshot.bridge, state: "ready" },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return started ? readySnapshot : snapshot;
          case "start_bridge":
            startCalls += 1;
            return pendingStart.then((value) => {
              started = true;
              return value;
            });
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    const startButton = wrapper.get(".head-actions .primary");
    await startButton.trigger("click");
    expect(startButton.text()).toContain("正在桥接");
    expect(startButton.text()).not.toContain("正在启动");
    expect(wrapper.text()).not.toContain("正在启动");
    expect(wrapper.get(".title-row .badge").text()).toBe("正在桥接");
    expect(startButton.find(".spin").exists()).toBe(true);
    await startButton.trigger("click");
    expect(startCalls).toBe(1);

    resolveStart(readySnapshot);
    await flushPromises();
    expect(wrapper.find(".head-actions .danger").text()).toContain("断开桥接");
    wrapper.unmount();
  });

  it("updates startup stages from bridge events while the start command is pending", async () => {
    type SnapshotWithWorkspace = Omit<typeof snapshot, "bridge"> & {
      bridge: Omit<typeof snapshot.bridge, "workspaceSn"> & {
        workspaceSn: string | null;
      };
    };
    let currentSnapshot: SnapshotWithWorkspace = snapshot;
    let resolveStart!: (value: SnapshotWithWorkspace) => void;
    const pendingStart = new Promise<SnapshotWithWorkspace>((resolve) => {
      resolveStart = resolve;
    });
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return currentSnapshot;
          case "start_bridge":
            return pendingStart;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    await wrapper.get(".head-actions .primary").trigger("click");
    await flushPromises();

    const publish = async (
      state: string,
      workspaceSn: string | null = currentSnapshot.bridge.workspaceSn,
    ) => {
      currentSnapshot = {
        ...currentSnapshot,
        bridge: { ...currentSnapshot.bridge, state, workspaceSn },
      };
      await emit("cnb-bridge://snapshot");
      await flushPromises();
    };

    await publish("preparingRepo");
    expect(wrapper.findAll(".pipeline .step")[1].classes()).toContain("done");

    await publish("startingWorkspace");
    expect(wrapper.findAll(".pipeline .step")[2].classes()).toContain("current");

    await publish("connecting", "ws-connecting");
    expect(wrapper.findAll(".pipeline .step")[2].classes()).toContain("current");
    expect(wrapper.findAll(".pipeline .step")[2].text()).toContain("正在连接");
    expect(wrapper.findAll(".pipeline .step")[2].text()).not.toContain(
      "工作空间与 SSH 通道",
    );

    await publish("ready");
    expect(wrapper.text()).toContain("在线");
    expect(wrapper.findAll(".pipeline .step")[2].classes()).toContain("done");
    expect(wrapper.findAll(".pipeline .step")[2].text()).toContain("SSH 已连接");

    resolveStart(currentSnapshot);
    await flushPromises();
    wrapper.unmount();
  });

  it("does not present a cached workspace as connected or waiting", async () => {
    const detachedSnapshot = {
      ...snapshot,
      bridge: {
        ...snapshot.bridge,
        state: "idle",
        workspaceSn: "ws-kept",
      },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return detachedSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    const steps = wrapper.findAll(".pipeline .step");
    expect(steps).toHaveLength(3);
    const workspaceStep = steps[2];
    expect(workspaceStep.classes()).not.toContain("done");
    expect(workspaceStep.classes()).not.toContain("current");
    expect(workspaceStep.text()).toContain("按需连接");
    expect(workspaceStep.text()).not.toContain("等待重连");
    expect(wrapper.text()).not.toContain("等待建立通道");
    expect(wrapper.get(".title-row .badge").text()).toBe("按需连接");
    expect(wrapper.find(".head-actions .primary").text()).toContain("立即桥接");
    wrapper.unmount();
  });

  it("shows repository provisioning as on demand before the first bridge", async () => {
    const unpreparedSnapshot = {
      ...snapshot,
      bridge: {
        ...snapshot.bridge,
        state: "idle",
        repoSlug: null,
        workspaceSn: null,
      },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return unpreparedSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    expect(wrapper.findAll(".pipeline .step")[1].text()).toContain("按需准备");
    expect(wrapper.text()).not.toContain("等待自动准备");
    wrapper.unmount();
  });

  it("shows an explicit failure instead of an on-demand cloud state", async () => {
    const failedSnapshot = {
      ...snapshot,
      bridge: {
        ...snapshot.bridge,
        state: "failed",
        workspaceSn: null,
        error: "建立 SSH 通道失败",
      },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return failedSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    const workspaceStep = wrapper.findAll(".pipeline .step")[2];
    expect(workspaceStep.classes()).toContain("error");
    expect(workspaceStep.text()).toContain("连接失败");
    expect(workspaceStep.text()).not.toContain("按需连接");
    wrapper.unmount();
  });

  it("shows a closing label while the bridge is stopping", async () => {
    const stoppingSnapshot = {
      ...snapshot,
      bridge: {
        ...snapshot.bridge,
        state: "stopping",
        workspaceSn: "ws-stopping",
      },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return stoppingSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    const action = wrapper.get(".head-actions .primary");
    expect(action.text()).toContain("正在断开");
    expect(action.text()).not.toContain("正在启动");
    expect(wrapper.get(".title-row .badge").text()).toBe("正在断开");
    const workspaceStep = wrapper.findAll(".pipeline .step")[2];
    expect(workspaceStep.text()).toContain("正在断开");
    expect(workspaceStep.text()).not.toContain("工作空间已保留");
    expect(workspaceStep.text()).not.toContain("工作空间");
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

    const cpu = wrapper.get(".usage-section.dev-cpu");
    expect(cpu.text()).toContain("云原生开发-CPU");
    expect(cpu.text()).toContain("380.66 / 1,600.00 核时");
    expect(cpu.get(".usage-percent").text()).toBe("23.8%");
    const cpuProgress = cpu.get('[role="progressbar"]');
    expect(cpuProgress.attributes("aria-label")).toBe(
      "云原生开发-CPU 使用进度",
    );
    expect(cpuProgress.attributes("aria-valuenow")).toBe("23.8");
    expect(
      (cpuProgress.get(".usage-progress-fill").element as HTMLElement).style
        .width,
    ).toBe("23.8%");
    wrapper.unmount();
  });

  it("orders client authentication before endpoint and model catalog cards", async () => {
    mockSnapshotIpc();
    const wrapper = mount(App);
    await flushPromises();

    const order = wrapper
      .findAll(".connect-card-grid > .connect-card")
      .map((card) => {
        return [
          "account-card",
          "api-key-card",
          "endpoint-card",
          "model-card",
        ].find((className) => card.classes().includes(className));
      });

    expect(order).toEqual([
      "account-card",
      "api-key-card",
      "endpoint-card",
      "model-card",
    ]);
    wrapper.unmount();
  });

  it("shows the local endpoint as listening while the cloud bridge is idle", async () => {
    const idleSnapshot = {
      ...snapshot,
      bridge: {
        ...snapshot.bridge,
        state: "idle",
        baseUrl: "http://127.0.0.1:11233/v1",
      },
    };
    mockSnapshotIpc(idleSnapshot);
    const wrapper = mount(App);
    await flushPromises();

    const endpointCard = wrapper.get(".endpoint-card");
    expect(endpointCard.classes()).toContain("is-live");
    expect(endpointCard.get(".badge").text()).toBe("监听中");
    expect(endpointCard.text()).not.toContain("按需启动");
    wrapper.unmount();
  });

  it("shows the local endpoint as unavailable when the gateway has no base URL", async () => {
    const unavailableSnapshot = {
      ...snapshot,
      bridge: {
        ...snapshot.bridge,
        state: "idle",
        baseUrl: null as string | null,
      },
    };
    mockSnapshotIpc(unavailableSnapshot);
    const wrapper = mount(App);
    await flushPromises();

    const endpointCard = wrapper.get(".endpoint-card");
    expect(endpointCard.classes()).not.toContain("is-live");
    expect(endpointCard.get(".badge").text()).toBe("未监听");
    wrapper.unmount();
  });

  it("omits model availability and count labels from the model catalog", async () => {
    mockSnapshotIpc();
    const wrapper = mount(App);
    await flushPromises();

    const modelCard = wrapper.get(".model-card");
    expect(modelCard.get(".model-name-row").text()).not.toContain("可用");
    expect(modelCard.text()).not.toContain("个模型");
    expect(modelCard.text()).toContain("deepseek-v4.1-flash");
    expect(modelCard.text()).toContain("工具调用");
    expect(modelCard.find(".copy-button").exists()).toBe(true);
    wrapper.unmount();
  });

  it("shows a centered sign-in guide and no connection cards when no account is saved", async () => {
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

    expect(wrapper.get(".connect-signed-out").text()).toContain("登录 CNB");
    expect(wrapper.get(".connect-signed-out").text()).toContain("左下角");
    expect(wrapper.find(".account-card").exists()).toBe(false);
    expect(wrapper.find(".sidebar-account-menu").exists()).toBe(false);
    expect(wrapper.find(".model-card").exists()).toBe(false);
    expect(wrapper.find(".endpoint-card").exists()).toBe(false);
    expect(wrapper.find(".api-key-card").exists()).toBe(false);
    expect(wrapper.find(".pipeline-card").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("扫码绑定 CNB");
    expect(wrapper.text()).toContain("桥接状态");
    expect(wrapper.text()).not.toContain("启动状态");

    await wrapper.get(".sidebar-status").trigger("click");
    expect(document.body.textContent).toContain("扫码登录 CNB 账号");
    wrapper.unmount();
  });

  it("derives the sidebar login status from the account while authorization is pending", async () => {
    const pendingSnapshot = {
      ...signedOutSnapshot,
      account: {
        ...account,
        state: "signingIn",
        username: "alice",
        displayName: "爱丽丝",
        avatarUrl: "https://cnb.cool/users/alice/avatar/l",
      },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return pendingSnapshot;
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

    expect(wrapper.get(".sidebar-status").text()).toContain("爱丽丝");
    expect(wrapper.get(".sidebar-status-badge").text()).toBe("等待授权");
    expect(wrapper.text()).not.toContain("取消扫码");
    expect(wrapper.get(".connect-signed-out").text()).toContain(
      "正在登录 爱丽丝",
    );
    wrapper.unmount();
  });

  it("logs in with a selected saved account and confirms removal", async () => {
    const calls: { command: string; args: unknown }[] = [];
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
    await wrapper.find(".sidebar-status").trigger("click");
    const accountRows = [
      ...document.body.querySelectorAll<HTMLElement>(".account-menu-row"),
    ];
    expect(accountRows).toHaveLength(2);
    expect(accountRows[0].textContent).toContain("爱丽丝");
    expect(accountRows[0].textContent).not.toContain("alice");

    (accountRows[0].querySelector(".account-choice") as HTMLElement).click();
    await flushPromises();
    expect(document.body.querySelector(".account-menu")).toBeNull();
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
    await wrapper.find(".sidebar-status").trigger("click");
    const removeButtons = [
      ...document.body.querySelectorAll<HTMLElement>(".remove-saved-account"),
    ];
    removeButtons[1].click();
    await flushPromises();
    expect(document.body.querySelector(".confirm-dialog")?.textContent).toContain(
      "从本机移除 鲍勃",
    );
    (
      document.body.querySelector(".confirm-dialog .danger") as HTMLElement
    ).click();
    await flushPromises();
    expect(
      calls.find((call) => call.command === "remove_account")?.args,
    ).toEqual({ username: "bob" });

    wrapper.unmount();
  });

  it("switches between all views through the sidebar", async () => {
    mockSnapshotIpc();
    const wrapper = mount(App);
    await flushPromises();
    expect(wrapper.find(".account-main").text()).toContain("Tester 昵称");
    expect(wrapper.find(".account-avatar img").attributes("src")).toBe(
      "https://cnb.cool/users/tester/avatar/l",
    );
    expect(wrapper.find(".account-head .badge").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("退出登录");

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

  it("shows newest runtime logs first while preserving HTTP record order", async () => {
    const runtimeLogs = [
      { time: "2026-11-18 10:00:00", level: "info", message: "oldest info" },
      { time: "2026-11-18 10:00:01", level: "error", message: "old error" },
      { time: "2026-11-18 10:00:02", level: "info", message: "middle info" },
      { time: "2026-11-18 10:00:03", level: "error", message: "latest error" },
    ];
    const httpTraces = [
      {
        id: "new-trace",
        startedAt: "2026-11-18T10:00:03Z",
        method: "POST",
        path: "/v1/responses",
        status: 200,
        elapsedMs: 10,
        protocol: "responses->chat",
        stream: true,
        requestBytes: 10,
        responseBytes: 20,
        requestTruncated: false,
        responseTruncated: false,
        result: "completed",
      },
      {
        id: "old-trace",
        startedAt: "2026-11-18T10:00:01Z",
        method: "POST",
        path: "/v1/chat/completions",
        status: 200,
        elapsedMs: 20,
        protocol: "chat",
        stream: true,
        requestBytes: 10,
        responseBytes: 20,
        requestTruncated: false,
        responseTruncated: false,
        result: "completed",
      },
    ];
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return snapshot;
          case "get_logs":
            return runtimeLogs;
          case "list_http_traces":
            return httpTraces;
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();
    await wrapper.findAll("nav button")[2].trigger("click");
    await flushPromises();

    expect(wrapper.findAll(".log-row").map((row) => row.text())).toEqual([
      expect.stringContaining("latest error"),
      expect.stringContaining("middle info"),
      expect.stringContaining("old error"),
      expect.stringContaining("oldest info"),
    ]);

    await wrapper.get(".filter-control .select-trigger").trigger("click");
    (
      [...document.body.querySelectorAll<HTMLElement>(".select-option")].find(
        (option) => option.textContent?.trim() === "Error",
      ) as HTMLElement
    ).click();
    await flushPromises();
    expect(wrapper.findAll(".log-row").map((row) => row.text())).toEqual([
      expect.stringContaining("latest error"),
      expect.stringContaining("old error"),
    ]);

    await wrapper
      .findAll(".mode-switch button")
      .find((button) => button.text() === "HTTP 记录")!
      .trigger("click");
    await flushPromises();
    expect(
      wrapper.findAll(".http-log-row").map((row) => row.text()),
    ).toEqual([
      expect.stringContaining("/v1/responses"),
      expect.stringContaining("/v1/chat/completions"),
    ]);
    wrapper.unmount();
  });

  it("keeps an organization warning badge on a connected account", async () => {
    const needsOrganizationSnapshot = {
      ...snapshot,
      account: { ...account, needsOrganization: true },
      bridge: {
        ...snapshot.bridge,
        account: { ...account, needsOrganization: true },
      },
    };
    mockWindows("main");
    mockIPC(
      (command) => {
        switch (command) {
          case "get_snapshot":
            return needsOrganizationSnapshot;
          case "get_logs":
          case "list_http_traces":
          case "list_namespaces":
            return [];
          case "get_cnb_usage":
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    const wrapper = mount(App);
    await flushPromises();

    expect(wrapper.get(".account-head .badge").text()).toBe("组织未就绪");
    expect(wrapper.text()).not.toContain("退出登录");
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

    const modelButton = wrapper.get(".model-card .copy-button");
    const endpointButton = wrapper.get(".endpoint-card .copy-button");

    await modelButton.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("deepseek-v4.1-flash");
    expect(modelButton.text()).toContain("已复制");
    expect(wrapper.find(".banner.notice").exists()).toBe(false);

    await endpointButton.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("http://127.0.0.1:11233/v1");
    expect(endpointButton.text()).toContain("已复制");
    expect(modelButton.text()).toContain("复制 ID");
    expect(wrapper.find(".banner.notice").exists()).toBe(false);

    await vi.advanceTimersByTimeAsync(2500);
    await flushPromises();
    expect(endpointButton.text()).toContain("复制地址");

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

  it("shows version 1.0.0 and opens the project homepage from About", async () => {
    const calls: { command: string; args: unknown }[] = [];
    mockWindows("main");
    mockIPC(
      (command, args) => {
        calls.push({ command, args });
        switch (command) {
          case "get_snapshot":
            return { ...snapshot, version: "1.0.0" };
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

    const aboutButton = wrapper
      .findAll("nav button")
      .find((button) => button.text().includes("关于"));
    expect(aboutButton).toBeDefined();
    await aboutButton?.trigger("click");
    await flushPromises();

    expect(wrapper.get(".details-card").text()).toContain("1.0.0");
    const projectLink = wrapper.get(".project-home-action");
    expect(projectLink.text()).toContain("项目主页");
    await projectLink.trigger("click");
    await flushPromises();

    expect(
      calls.find((call) => call.command === "open_external")?.args,
    ).toEqual({ url: "https://github.com/xljiulang/cnb-bridge" });
    wrapper.unmount();
  });
});
