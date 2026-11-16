import {
  computed,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  watch,
} from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  cancelLogin as cancelLoginApi,
  createCnbNamespace,
  getCnbUsage,
  getLogs,
  getSnapshot,
  listHttpTraces,
  listNamespaces,
  logout as logoutApi,
  openExternal,
  openHttpTrace,
  openLogsFolder as openLogsFolderApi,
  saveSettings,
  startBridge as startBridgeApi,
  startLogin,
  stopBridge as stopBridgeApi,
} from "../api";
import {
  bridgeLabel,
  formatCreditsUsage,
  formatError,
  isBridgeTransitioning,
  settingsEqual,
  shouldShowConnectionDetails,
  statusIconKind,
  validatePort,
} from "../format";
import type {
  AppSnapshot,
  CnbUsageSummary,
  HttpTraceSummary,
  LogEntry,
  Namespace,
  Settings,
} from "../types";

export type Tab = "connect" | "settings" | "logs" | "about";
export type StepTone = "done" | "current" | "pending" | "error";

export interface ConnectionStep {
  label: string;
  detail: string;
  tone: StepTone;
}

export function useDesktopApp() {
  const snapshot = ref<AppSnapshot | null>(null);
  const tab = ref<Tab>("connect");
  const busy = ref(false);
  const error = ref<string | null>(null);
  const notice = ref<string | null>(null);
  const logs = ref<LogEntry[]>([]);
  const httpTraces = ref<HttpTraceSummary[]>([]);
  const namespaces = ref<Namespace[]>([]);
  const logFilter = ref("all");
  const logMode = ref<"runtime" | "http">("runtime");
  const copiedKey = ref<string | null>(null);
  const usage = ref<CnbUsageSummary | null>(null);
  const usageLoading = ref(false);
  const usageError = ref<string | null>(null);
  const creatingOrganization = ref(false);
  const settingsDraft = reactive<Settings>({
    repoNamespace: "",
    repoName: "cnb-ai-bridge",
    localPort: 11233,
    httpLogFilter: "abnormal",
    launchAtLogin: false,
  });
  let timer: number | undefined;
  let noticeTimer: number | undefined;
  let copyTimer: number | undefined;
  let usageTimer: number | undefined;
  let usageGeneration = 0;
  let unlisten: UnlistenFn | undefined;

  const ready = computed(() => snapshot.value?.bridge.state === "ready");
  const bridgeState = computed(() => snapshot.value?.bridge.state ?? "signedOut");
  const bridgeTransitioning = computed(() =>
    isBridgeTransitioning(bridgeState.value),
  );
  const idle = computed(() => bridgeState.value === "idle");
  const repoCreationNeeded = computed(
    () => bridgeState.value === "needsRepoCreation",
  );
  const accountState = computed(
    () => snapshot.value?.account.state ?? "signedOut",
  );
  const accountConnected = computed(() => accountState.value === "connected");
  const organizationRequired = computed(
    () =>
      accountConnected.value &&
      Boolean(snapshot.value?.account.needsOrganization),
  );
  const organizationReady = computed(
    () => accountConnected.value && !organizationRequired.value,
  );
  const sidebarStatusIcon = computed(() =>
    statusIconKind(
      bridgeState.value,
      accountConnected.value,
      creatingOrganization.value,
    ),
  );
  const settingsDirty = computed(() => {
    const persisted = snapshot.value?.settings;
    return persisted ? !settingsEqual(settingsDraft, persisted) : false;
  });
  const connectionDetailsVisible = computed(() =>
    shouldShowConnectionDetails(
      accountState.value,
      Boolean(snapshot.value?.account.needsOrganization),
    ),
  );
  const statusLabel = computed(() =>
    organizationRequired.value
      ? creatingOrganization.value
        ? "创建组织中"
        : "组织未就绪"
      : snapshot.value
        ? bridgeLabel(snapshot.value.bridge.state)
        : "正在加载",
  );
  const baseUrl = computed(
    () =>
      snapshot.value?.bridge.baseUrl ??
      `http://127.0.0.1:${snapshot.value?.settings.localPort ?? 11233}/v1`,
  );
  const namespaceAvailable = computed(() =>
    namespaces.value.some(
      (namespace) => namespace.path === settingsDraft.repoNamespace,
    ),
  );
  const filteredLogs = computed(() =>
    logFilter.value === "all"
      ? logs.value
      : logs.value.filter((entry) => entry.level === logFilter.value),
  );
  const namespaceOptions = computed(() => {
    const options = [{ value: "", label: "自动选择" }];
    if (settingsDraft.repoNamespace && !namespaceAvailable.value) {
      options.push({
        value: settingsDraft.repoNamespace,
        label: `${settingsDraft.repoNamespace}（当前配置不可用）`,
      });
    }
    for (const namespace of namespaces.value) {
      options.push({
        value: namespace.path,
        label: `${namespace.name}（${namespace.path}）`,
      });
    }
    return options;
  });
  const logLevelOptions = [
    { value: "all", label: "全部" },
    { value: "info", label: "Info" },
    { value: "warn", label: "Warn" },
    { value: "error", label: "Error" },
  ];
  const httpLogOptions = [
    { value: "abnormal", label: "仅异常" },
    { value: "all", label: "所有" },
  ];
  const creditsUsageLabel = computed(() => {
    if (usage.value) {
      return formatCreditsUsage(
        usage.value.creditsUsedMilli,
        usage.value.creditsTotalMilli,
      );
    }
    if (usageError.value) return "用量暂不可用";
    return usageLoading.value ? "正在获取…" : "-- / --";
  });
  const mainActionLabel = computed(() => {
    if (bridgeTransitioning.value) return "正在启动…";
    if (repoCreationNeeded.value) return "重新检查仓库";
    if (idle.value) return "立即启动";
    return "启动桥接";
  });
  const pipelineSteps = computed<ConnectionStep[]>(() => {
    const failed =
      bridgeState.value === "failed" || bridgeState.value === "needsSshKey";
    const accountDone = accountState.value === "connected";
    const repoDone =
      Boolean(snapshot.value?.bridge.repoSlug) && !repoCreationNeeded.value;
    const workspaceDone = ready.value;
    const tunnelActive = bridgeState.value === "connecting";
    const tone = (done: boolean, current: boolean): StepTone => {
      if (done) return "done";
      if (failed) return "error";
      return current ? "current" : "pending";
    };

    return [
      {
        label: "CNB 账号",
        detail: snapshot.value?.account.username || "等待扫码绑定",
        tone: accountDone
          ? "done"
          : accountState.value === "failed"
            ? "error"
            : "current",
      },
      {
        label: "仓库",
        detail: snapshot.value?.bridge.repoSlug || "等待自动准备",
        tone: tone(
          repoDone,
          accountDone &&
            !repoDone &&
            bridgeState.value === "preparingRepo",
        ),
      },
      {
        label: "云主机",
        detail: snapshot.value?.bridge.workspaceSn
          ? "工作空间与 SSH 通道"
          : "启动工作空间",
        tone: tone(
          workspaceDone,
          Boolean(snapshot.value?.bridge.workspaceSn) ||
            bridgeState.value === "startingWorkspace" ||
            tunnelActive,
        ),
      },
      {
        label: "本机端点",
        detail: ready.value
          ? "端口已监听"
          : idle.value
            ? "收到请求后建立通道"
            : "等待建立通道",
        tone: tone(ready.value, tunnelActive),
      },
    ];
  });

  async function refresh() {
    try {
      const syncDraft = !settingsDirty.value;
      const next = await getSnapshot();
      snapshot.value = next;
      if (next.account.state === "failed" && next.account.error) {
        error.value = next.account.error;
      } else if (
        next.bridge.error &&
        ["failed", "needsSshKey", "needsRepoCreation"].includes(
          next.bridge.state,
        )
      ) {
        error.value = next.bridge.error;
      } else if (next.bridge.state === "ready" && !next.account.error) {
        error.value = null;
      }
      if (syncDraft) applySettingsDraft(next.settings);
    } catch (cause) {
      error.value = formatError(cause);
    }
  }

  async function loadUsage() {
    if (!organizationReady.value || usageLoading.value) return;
    const generation = ++usageGeneration;
    usageLoading.value = true;
    try {
      const next = await getCnbUsage();
      if (generation !== usageGeneration || !organizationReady.value) return;
      usage.value = next;
      usageError.value = null;
    } catch (cause) {
      if (generation !== usageGeneration || !organizationReady.value) return;
      usageError.value = formatError(cause);
    } finally {
      if (generation === usageGeneration) usageLoading.value = false;
    }
  }

  function applySettingsDraft(settings: Settings) {
    settingsDraft.repoNamespace = settings.repoNamespace;
    settingsDraft.repoName = settings.repoName;
    settingsDraft.localPort = settings.localPort;
    settingsDraft.httpLogFilter = settings.httpLogFilter;
    settingsDraft.launchAtLogin = settings.launchAtLogin;
  }

  async function run(action: () => Promise<unknown>, message?: string) {
    if (busy.value) return;
    busy.value = true;
    error.value = null;
    clearTransientFeedback();
    try {
      await action();
      if (message) showNotice(message);
      await refresh();
    } catch (cause) {
      error.value = formatError(cause);
      await refresh();
    } finally {
      busy.value = false;
    }
  }

  async function login() {
    await run(() => startLogin());
  }

  async function logout() {
    await run(() => logoutApi());
  }

  async function cancelLogin() {
    await run(() => cancelLoginApi());
  }

  async function startBridge() {
    await run(() => startBridgeApi());
  }

  async function stopBridge() {
    await run(() => stopBridgeApi());
  }

  async function openLogsFolder() {
    await run(() => openLogsFolderApi());
  }

  async function copyValue(key: string, value: string) {
    try {
      await navigator.clipboard.writeText(value);
      clearNotice();
      copiedKey.value = key;
      if (copyTimer) window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(() => {
        copiedKey.value = null;
        copyTimer = undefined;
      }, 2500);
    } catch (cause) {
      error.value = formatError(cause);
    }
  }

  function showNotice(message: string) {
    clearNotice();
    notice.value = message;
    noticeTimer = window.setTimeout(() => {
      notice.value = null;
      noticeTimer = undefined;
    }, 2500);
  }

  function clearNotice() {
    if (noticeTimer) window.clearTimeout(noticeTimer);
    noticeTimer = undefined;
    notice.value = null;
  }

  function clearTransientFeedback() {
    clearNotice();
    if (copyTimer) window.clearTimeout(copyTimer);
    copyTimer = undefined;
    copiedKey.value = null;
  }

  function openConnect() {
    clearTransientFeedback();
    tab.value = "connect";
  }

  function openAbout() {
    clearTransientFeedback();
    tab.value = "about";
  }

  function handleVisibilityChange() {
    if (document.visibilityState === "visible" && organizationReady.value) {
      void loadUsage();
    }
  }

  async function createOrganization() {
    if (
      !organizationRequired.value ||
      creatingOrganization.value ||
      busy.value
    ) {
      return;
    }
    creatingOrganization.value = true;
    busy.value = true;
    error.value = null;
    try {
      await createCnbNamespace();
      await refresh();
    } catch (cause) {
      error.value = formatError(cause);
      await refresh();
    } finally {
      creatingOrganization.value = false;
      busy.value = false;
    }
  }

  async function loadNamespaces() {
    if (accountState.value !== "connected") {
      namespaces.value = [];
      return;
    }
    try {
      namespaces.value = await listNamespaces();
    } catch (cause) {
      namespaces.value = [];
      error.value = formatError(cause);
    }
  }

  async function openSettings() {
    clearTransientFeedback();
    tab.value = "settings";
    await loadNamespaces();
  }

  async function openLogs() {
    clearTransientFeedback();
    tab.value = "logs";
    await Promise.all([loadLogs(), loadHttpTraces()]);
  }

  async function openRepoCreation() {
    const url = snapshot.value?.bridge.repoCreationUrl;
    if (!url) return;
    try {
      await openExternal(url);
      showNotice("创建私有空仓库后返回这里，点击“重新检查仓库”。");
    } catch (cause) {
      error.value = formatError(cause);
    }
  }

  async function save() {
    const portError = validatePort(Number(settingsDraft.localPort));
    if (portError) {
      error.value = portError;
      return;
    }
    await run(async () => {
      const next = await saveSettings({
        ...settingsDraft,
        localPort: Number(settingsDraft.localPort),
      });
      snapshot.value = next;
      applySettingsDraft(next.settings);
    }, "设置已保存");
  }

  async function loadLogs() {
    try {
      logs.value = await getLogs(200);
    } catch (cause) {
      error.value = formatError(cause);
    }
  }

  async function loadHttpTraces() {
    try {
      httpTraces.value = await listHttpTraces(100);
    } catch (cause) {
      error.value = formatError(cause);
    }
  }

  async function openTrace(id: string) {
    try {
      await openHttpTrace(id);
    } catch (cause) {
      error.value = formatError(cause);
    }
  }

  function formatBytes(value: number): string {
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
    return `${(value / 1024 / 1024).toFixed(1)} MiB`;
  }

  function httpTraceLabel(trace: HttpTraceSummary): string {
    return `${trace.method} ${trace.path}，状态 ${trace.status}，请求 ${formatBytes(
      trace.requestBytes,
    )}，响应 ${formatBytes(trace.responseBytes)}，耗时 ${trace.elapsedMs} 毫秒`;
  }

  onMounted(async () => {
    await refresh();
    if (organizationReady.value) void loadUsage();
    await Promise.all([loadLogs(), loadHttpTraces()]);
    timer = window.setInterval(() => {
      void refresh();
      if (tab.value === "logs") {
        if (logMode.value === "runtime") void loadLogs();
        else void loadHttpTraces();
      }
    }, 1500);
    usageTimer = window.setInterval(() => {
      if (organizationReady.value) void loadUsage();
    }, 60_000);
    document.addEventListener("visibilitychange", handleVisibilityChange);
    unlisten = await listen("cnb-bridge://snapshot", () => {
      void refresh();
    });
  });

  onBeforeUnmount(() => {
    if (timer) window.clearInterval(timer);
    if (usageTimer) window.clearInterval(usageTimer);
    document.removeEventListener("visibilitychange", handleVisibilityChange);
    clearTransientFeedback();
    unlisten?.();
  });

  watch(
    [accountState, organizationRequired],
    ([state, needsOrganization]) => {
      if (state === "connected" && !needsOrganization) {
        void loadUsage();
        return;
      }
      if (state === "connected" && needsOrganization) {
        usageGeneration += 1;
        usage.value = null;
        usageError.value = null;
        usageLoading.value = false;
        return;
      }
      usageGeneration += 1;
      usage.value = null;
      usageError.value = null;
      usageLoading.value = false;
    },
    { immediate: true },
  );

  return reactive({
    snapshot,
    tab,
    busy,
    error,
    notice,
    logs,
    httpTraces,
    namespaces,
    logFilter,
    logMode,
    copiedKey,
    usage,
    usageLoading,
    usageError,
    creatingOrganization,
    settingsDraft,
    ready,
    bridgeState,
    bridgeTransitioning,
    sidebarStatusIcon,
    idle,
    repoCreationNeeded,
    accountState,
    accountConnected,
    organizationRequired,
    organizationReady,
    settingsDirty,
    connectionDetailsVisible,
    statusLabel,
    baseUrl,
    namespaceAvailable,
    filteredLogs,
    namespaceOptions,
    logLevelOptions,
    httpLogOptions,
    creditsUsageLabel,
    mainActionLabel,
    pipelineSteps,
    login,
    logout,
    cancelLogin,
    startBridge,
    stopBridge,
    createOrganization,
    openSettings,
    openLogs,
    openConnect,
    openAbout,
    save,
    copyValue,
    openRepoCreation,
    loadLogs,
    loadHttpTraces,
    openTrace,
    openLogsFolder,
    clearNotice,
    formatBytes,
    httpTraceLabel,
  });
}

export type DesktopApp = ReturnType<typeof useDesktopApp>;
