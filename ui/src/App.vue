<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  Activity,
  AlertTriangle,
  Cable,
  CheckCircle2,
  ChevronRight,
  CircleUserRound,
  Copy,
  Cpu,
  ExternalLink,
  FolderGit2,
  FolderOpen,
  Info,
  KeyRound,
  LoaderCircle,
  LogOut,
  Network,
  Power,
  RefreshCw,
  ScanQrCode,
  ScrollText,
  Server,
  Settings as SettingsIcon,
  Square,
  X,
} from "@lucide/vue";
import {
  cancelLogin,
  getLogs,
  getSnapshot,
  listHttpTraces,
  listNamespaces,
  logout,
  openExternal,
  openHttpTrace,
  openLogsFolder,
  saveSettings,
  startBridge,
  startLogin,
  stopBridge,
} from "./api";
import appIcon from "./assets/cnb-favicon.svg";
import {
  bridgeLabel,
  formatError,
  formatContextWindow,
  formatInputModalities,
  isBridgeTransitioning,
  shouldShowConnectionDetails,
  validatePort,
} from "./format";
import type {
  AppSnapshot,
  HttpTraceSummary,
  LogEntry,
  ModelSummary,
  Namespace,
  Settings,
} from "./types";

type Tab = "connect" | "settings" | "logs" | "about";
type StepTone = "done" | "current" | "pending" | "error";

interface ConnectionStep {
  label: string;
  detail: string;
  tone: StepTone;
}

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
const settingsDirty = ref(false);
const copiedModelId = ref<string | null>(null);
const settingsDraft = reactive<Settings>({
  repoNamespace: "",
  repoName: "cnb-ai-bridge",
  localPort: 11233,
  minimizeToTray: true,
  launchAtLogin: false,
});
let timer: number | undefined;
let noticeTimer: number | undefined;
let modelCopyTimer: number | undefined;
let unlisten: UnlistenFn | undefined;

const ready = computed(() => snapshot.value?.bridge.state === "ready");
const bridgeState = computed(() => snapshot.value?.bridge.state ?? "signedOut");
const bridgeTransitioning = computed(() => isBridgeTransitioning(bridgeState.value));
const idle = computed(() => bridgeState.value === "idle");
const repoCreationNeeded = computed(() => bridgeState.value === "needsRepoCreation");
const accountState = computed(() => snapshot.value?.account.state ?? "signedOut");
const connectionDetailsVisible = computed(() =>
  shouldShowConnectionDetails(accountState.value),
);
const statusLabel = computed(() =>
  snapshot.value ? bridgeLabel(snapshot.value.bridge.state) : "正在加载",
);
const baseUrl = computed(
  () =>
    snapshot.value?.bridge.baseUrl ??
    `http://127.0.0.1:${snapshot.value?.settings.localPort ?? 11233}/v1`,
);
const namespaceAvailable = computed(() =>
  namespaces.value.some((namespace) => namespace.path === settingsDraft.repoNamespace),
);
const filteredLogs = computed(() =>
  logFilter.value === "all"
    ? logs.value
    : logs.value.filter((entry) => entry.level === logFilter.value),
);
const statusDescription = computed(() => {
  if (ready.value) return "本机 OpenAI 兼容端点已就绪，可以直接连接你的 AI 客户端。";
  if (repoCreationNeeded.value) {
    return "请先在 CNB 创建私有空仓库，然后返回这里重新检查。";
  }
  if (idle.value) {
    return "本地端点已就绪。收到请求后会按需启动 CNB 云主机。";
  }
  if (bridgeTransitioning.value) return "正在准备仓库、云主机与本机通道，请稍候。";
  if (bridgeState.value === "failed" || bridgeState.value === "needsSshKey") {
    return "启动未完成。下方错误信息会说明需要处理的问题。";
  }
  if (accountState.value !== "connected") {
    return "先完成一次扫码绑定，之后启动过程会自动完成建仓和连接。";
  }
  return "账号已绑定。启动后会自动准备仓库、云主机和本机通道。";
});
const mainActionLabel = computed(() => {
  if (bridgeTransitioning.value) return "正在启动…";
  if (repoCreationNeeded.value) return "重新检查仓库";
  if (idle.value) return "立即启动";
  return "启动桥接";
});
const pipelineSteps = computed<ConnectionStep[]>(() => {
  const failed = bridgeState.value === "failed" || bridgeState.value === "needsSshKey";
  const accountDone = accountState.value === "connected";
  const repoDone = Boolean(snapshot.value?.bridge.repoSlug) && !repoCreationNeeded.value;
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
      tone: accountDone ? "done" : accountState.value === "failed" ? "error" : "current",
    },
    {
      label: "仓库",
      detail: snapshot.value?.bridge.repoSlug || "等待自动准备",
      tone: tone(
        repoDone,
        accountDone && !repoDone && bridgeState.value === "preparingRepo",
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
    if (!settingsDirty.value) {
      settingsDraft.repoNamespace = next.settings.repoNamespace;
      settingsDraft.repoName = next.settings.repoName;
      settingsDraft.localPort = next.settings.localPort;
      settingsDraft.minimizeToTray = next.settings.minimizeToTray;
      settingsDraft.launchAtLogin = next.settings.launchAtLogin;
    }
  } catch (cause) {
    error.value = formatError(cause);
  }
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
  await run(async () => {
    await startLogin();
  });
}

async function copyText(value: string, message: string) {
  try {
    await navigator.clipboard.writeText(value);
    showNotice(message);
  } catch (cause) {
    error.value = formatError(cause);
  }
}

async function copyModelId(model: ModelSummary) {
  try {
    await navigator.clipboard.writeText(model.id);
    clearNotice();
    copiedModelId.value = model.id;
    if (modelCopyTimer) window.clearTimeout(modelCopyTimer);
    modelCopyTimer = window.setTimeout(() => {
      copiedModelId.value = null;
      modelCopyTimer = undefined;
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
  if (modelCopyTimer) window.clearTimeout(modelCopyTimer);
  modelCopyTimer = undefined;
  copiedModelId.value = null;
}

function openConnect() {
  clearTransientFeedback();
  tab.value = "connect";
}

function openAbout() {
  clearTransientFeedback();
  tab.value = "about";
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
    await saveSettings({
      ...settingsDraft,
      localPort: Number(settingsDraft.localPort),
    });
    settingsDirty.value = false;
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

onMounted(async () => {
  await refresh();
  await Promise.all([loadLogs(), loadHttpTraces()]);
  timer = window.setInterval(() => {
    void refresh();
    if (tab.value === "logs") {
      if (logMode.value === "runtime") void loadLogs();
      else void loadHttpTraces();
    }
  }, 1500);
  unlisten = await listen("cnb-bridge://snapshot", () => {
    void refresh();
  });
});

onBeforeUnmount(() => {
  if (timer) window.clearInterval(timer);
  clearTransientFeedback();
  unlisten?.();
});
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <img class="brand-mark" :src="appIcon" alt="" aria-hidden="true" />
        <div class="brand-copy">
          <div class="brand-name">CNB Bridge</div>
          <div class="brand-sub">本机 AI 桥接</div>
        </div>
      </div>

      <nav class="nav" aria-label="主导航">
        <button
          type="button"
          :class="{ active: tab === 'connect' }"
          @click="openConnect"
        >
          <Cable :size="17" aria-hidden="true" />
          <span>连接</span>
          <span class="nav-status" :class="{ live: ready }" />
        </button>
        <button
          type="button"
          :class="{ active: tab === 'settings' }"
          @click="openSettings"
        >
          <SettingsIcon :size="17" aria-hidden="true" />
          <span>设置</span>
        </button>
        <button
          type="button"
          :class="{ active: tab === 'logs' }"
          @click="openLogs"
        >
          <ScrollText :size="17" aria-hidden="true" />
          <span>日志</span>
        </button>
        <button
          type="button"
          :class="{ active: tab === 'about' }"
          @click="openAbout"
        >
          <Info :size="17" aria-hidden="true" />
          <span>关于</span>
        </button>
      </nav>

      <div class="sidebar-status" :class="{ live: ready }">
        <span class="status-icon">
          <ScanQrCode v-if="!connectionDetailsVisible" :size="16" aria-hidden="true" />
          <Activity v-else-if="ready" :size="16" aria-hidden="true" />
          <LoaderCircle
            v-else-if="bridgeTransitioning"
            class="spin"
            :size="16"
            aria-hidden="true"
          />
          <Server v-else :size="16" aria-hidden="true" />
        </span>
        <div class="status-copy">
          <strong>{{ statusLabel }}</strong>
          <small>
            {{ accountState === "connected" ? snapshot?.account.username || "CNB 已绑定" : "等待扫码" }}
          </small>
        </div>
        <span class="port-chip">{{ snapshot?.settings.localPort ?? 11233 }}</span>
      </div>
    </aside>

    <main class="content">
      <div v-if="error" class="banner error" role="alert">
        <AlertTriangle :size="18" aria-hidden="true" />
        <span>{{ error }}</span>
        <div class="banner-actions">
          <button
            v-if="snapshot?.bridge.repoCreationUrl"
            type="button"
            class="secondary compact"
            @click="openRepoCreation"
          >
            <ExternalLink :size="15" aria-hidden="true" />
            打开 CNB 创建仓库
          </button>
          <button type="button" class="icon-button" aria-label="关闭错误" @click="error = null">
            <X :size="16" aria-hidden="true" />
          </button>
        </div>
      </div>
      <div v-if="notice" class="banner notice" role="status">
        <CheckCircle2 :size="18" aria-hidden="true" />
        <span>{{ notice }}</span>
        <button type="button" class="icon-button" aria-label="关闭提示" @click="clearNotice">
          <X :size="16" aria-hidden="true" />
        </button>
      </div>

      <section v-if="tab === 'connect'" class="page">
        <header class="page-head hero-head">
          <div class="page-title">
            <p class="eyebrow">CNB BRIDGE</p>
            <div class="title-row">
              <h1>{{ statusLabel }}</h1>
              <span class="status-pill" :class="{ live: ready, busy: bridgeTransitioning }">
                <span />
                {{
                  ready
                    ? "在线"
                    : idle
                      ? "待命"
                      : bridgeTransitioning
                        ? "处理中"
                        : "待连接"
                }}
              </span>
            </div>
            <p class="muted">{{ statusDescription }}</p>
          </div>
          <div v-if="connectionDetailsVisible" class="head-actions">
            <button
              v-if="!ready"
              type="button"
              class="primary large"
              :disabled="busy || bridgeTransitioning || accountState !== 'connected'"
              @click="run(startBridge)"
            >
              <LoaderCircle
                v-if="bridgeTransitioning"
                class="spin"
                :size="17"
                aria-hidden="true"
              />
              <Power v-else :size="17" aria-hidden="true" />
              {{ mainActionLabel }}
            </button>
            <button
              v-else
              type="button"
              class="danger large"
              :disabled="busy"
              @click="run(stopBridge)"
            >
              <Square :size="15" aria-hidden="true" />
              停止桥接
            </button>
          </div>
        </header>

        <div class="connect-stack">
          <div class="connect-card-grid">
            <article class="card account-card connect-card">
              <div class="account-head">
                <div class="account-main">
                  <span class="account-avatar" :class="{ scan: !connectionDetailsVisible }">
                    <CircleUserRound
                      v-if="connectionDetailsVisible"
                      :size="23"
                      aria-hidden="true"
                    />
                    <ScanQrCode v-else :size="24" aria-hidden="true" />
                  </span>
                  <div>
                    <span class="card-label">CNB 账号</span>
                    <strong>{{ snapshot?.account.username || "尚未绑定" }}</strong>
                  </div>
                </div>
                <span class="pill" :class="accountState">
                  {{
                    accountState === "connected"
                      ? "已连接"
                      : accountState === "signingIn"
                        ? "等待扫码"
                        : "未登录"
                  }}
                </span>
              </div>
              <p v-if="!connectionDetailsVisible" class="account-intro">
                扫码完成 CNB 授权后，即可查看模型、本机接入地址并启动桥接。
              </p>
              <div class="account-actions">
                <button
                  v-if="!connectionDetailsVisible"
                  type="button"
                  class="primary"
                  :disabled="busy || accountState === 'signingIn'"
                  @click="login"
                >
                  <ScanQrCode
                    :class="{ 'scan-waiting': accountState === 'signingIn' }"
                    :size="17"
                    aria-hidden="true"
                  />
                  {{ accountState === "signingIn" ? "等待扫码" : "扫码绑定 CNB" }}
                </button>
                <button
                  v-else
                  type="button"
                  class="danger compact"
                  :disabled="busy"
                  @click="run(logout)"
                >
                  <LogOut :size="15" aria-hidden="true" />
                  退出登录
                </button>
                <button
                  v-if="accountState === 'signingIn'"
                  type="button"
                  class="ghost"
                  :disabled="busy"
                  @click="run(cancelLogin)"
                >
                  取消扫码
                </button>
              </div>
            </article>

            <template v-if="connectionDetailsVisible">
              <article class="card model-card connect-card">
                <div class="section-head">
                  <div>
                    <span class="card-label">模型目录</span>
                    <h2>可用于 Chat 与 Responses</h2>
                  </div>
                  <div class="section-icon">
                    <Cpu :size="19" aria-hidden="true" />
                    <span>{{ snapshot?.models.length ?? 0 }} 个模型</span>
                  </div>
                </div>
                <div class="model-list">
                  <section
                    v-for="model in snapshot?.models ?? []"
                    :key="model.id"
                    class="model-item"
                  >
                    <div class="model-item-head">
                      <div class="model-title">
                        <div class="model-name-row">
                          <code>{{ model.id }}</code>
                          <span class="model-available">可用</span>
                        </div>
                      </div>
                      <button
                        type="button"
                        class="secondary compact model-copy"
                        :aria-label="
                          copiedModelId === model.id
                            ? `${model.id} 已复制`
                            : `复制模型 ID ${model.id}`
                        "
                        @click="copyModelId(model)"
                      >
                        <CheckCircle2
                          v-if="copiedModelId === model.id"
                          :size="15"
                          aria-hidden="true"
                        />
                        <Copy v-else :size="15" aria-hidden="true" />
                        {{ copiedModelId === model.id ? "已复制" : "复制 ID" }}
                      </button>
                    </div>
                    <div class="model-capabilities" aria-label="模型能力">
                      <span v-if="model.contextWindow">
                        {{ formatContextWindow(model.contextWindow) }}
                      </span>
                      <span v-if="model.inputModalities.length">
                        {{ formatInputModalities(model.inputModalities) }}
                      </span>
                      <span v-if="model.supportsTools">工具调用</span>
                      <span v-if="model.supportsStreaming">流式输出</span>
                    </div>
                  </section>
                </div>
              </article>

              <article class="card connect-card endpoint-card" :class="{ live: ready }">
                <div class="section-head">
                  <div>
                    <span class="card-label">接入端点</span>
                    <h2>本机 OpenAI 兼容地址</h2>
                  </div>
                  <span class="endpoint-state">
                    {{ ready ? "监听中" : idle ? "按需启动" : "未连接" }}
                  </span>
                </div>
                <code class="endpoint-url">{{ baseUrl }}</code>
                <div class="endpoint-meta">
                  <button
                    type="button"
                    class="secondary compact"
                    @click="copyText(baseUrl, 'Base URL 已复制')"
                  >
                    <Copy :size="15" aria-hidden="true" />
                    复制地址
                  </button>
                </div>
              </article>

              <article class="card connect-card api-key-card">
                <div class="section-head">
                  <div>
                    <span class="card-label">客户端鉴权</span>
                    <h2>API Key 可任意填写</h2>
                  </div>
                  <span class="icon-tile">
                    <KeyRound :size="17" aria-hidden="true" />
                  </span>
                </div>
                <p>
                  网关不校验客户端 API Key，可填写 <code>local</code>、任意占位值或留空。
                  真正的 CNB 鉴权来自已登录账号。
                </p>
                <div class="api-key-example">
                  <span>示例</span>
                  <code>local</code>
                </div>
              </article>
            </template>
          </div>

          <article v-if="connectionDetailsVisible" class="card pipeline-card">
            <div class="section-head">
              <div>
                <span class="card-label">启动链路</span>
                <h2>自动连接状态</h2>
              </div>
              <span class="muted small">
                端口 {{ snapshot?.settings.localPort ?? 11233 }}
              </span>
            </div>
            <div class="pipeline">
              <div
                v-for="(step, index) in pipelineSteps"
                :key="step.label"
                class="step"
                :class="step.tone"
              >
                <span class="step-icon">
                  <CheckCircle2
                    v-if="step.tone === 'done'"
                    :size="16"
                    aria-hidden="true"
                  />
                  <LoaderCircle
                    v-else-if="step.tone === 'current' && bridgeTransitioning"
                    class="spin"
                    :size="16"
                    aria-hidden="true"
                  />
                  <AlertTriangle
                    v-else-if="step.tone === 'error'"
                    :size="16"
                    aria-hidden="true"
                  />
                  <span v-else>{{ index + 1 }}</span>
                </span>
                <div>
                  <strong>{{ step.label }}</strong>
                  <small>{{ step.detail }}</small>
                </div>
                <ChevronRight
                  v-if="index < pipelineSteps.length - 1"
                  class="step-arrow"
                  :size="15"
                  aria-hidden="true"
                />
              </div>
            </div>
          </article>
        </div>
      </section>

      <section v-else-if="tab === 'settings'" class="page">
        <header class="page-head">
          <div class="page-title">
            <p class="eyebrow">SETTINGS</p>
            <h1>设置</h1>
          </div>
          <button
            type="button"
            class="primary"
            :disabled="busy"
            @click="save"
          >
            <LoaderCircle v-if="busy" class="spin" :size="16" aria-hidden="true" />
            <CheckCircle2 v-else :size="16" aria-hidden="true" />
            保存
          </button>
        </header>

        <article class="card settings-card">
          <section class="form-section">
            <div class="section-head">
              <div>
                <span class="card-label">仓库</span>
                <h2>创建位置</h2>
              </div>
              <FolderGit2 :size="19" aria-hidden="true" />
            </div>
            <div class="form-grid">
              <label>
                <span>命名空间</span>
                <select v-model="settingsDraft.repoNamespace" @change="settingsDirty = true">
                  <option value="">自动选择</option>
                  <option
                    v-if="settingsDraft.repoNamespace && !namespaceAvailable"
                    :value="settingsDraft.repoNamespace"
                  >
                    {{ settingsDraft.repoNamespace }}（当前配置不可用）
                  </option>
                  <option
                    v-for="namespace in namespaces"
                    :key="namespace.path"
                    :value="namespace.path"
                  >
                    {{ namespace.name }}（{{ namespace.path }}）
                  </option>
                </select>
                <small>单组织自动选择；多组织请手动指定。</small>
              </label>
              <label>
                <span>仓库名</span>
                <input
                  v-model="settingsDraft.repoName"
                  spellcheck="false"
                  @input="settingsDirty = true"
                />
                <small>默认 cnb-ai-bridge。</small>
              </label>
            </div>
          </section>

          <section class="form-section divided">
            <div class="section-head">
              <div>
                <span class="card-label">连接</span>
                <h2>本机端口</h2>
              </div>
              <Network :size="19" aria-hidden="true" />
            </div>
            <label class="narrow-field">
              <span>监听端口</span>
              <input
                v-model.number="settingsDraft.localPort"
                type="number"
                min="1"
                max="65535"
                @input="settingsDirty = true"
              />
              <small>修改后需等待当前请求结束。</small>
            </label>
          </section>

          <section class="form-section divided">
            <div class="section-head">
              <div>
                <span class="card-label">桌面</span>
                <h2>启动行为</h2>
              </div>
              <Power :size="19" aria-hidden="true" />
            </div>
            <label class="switch-row">
              <span>
                <strong>关闭窗口时最小化到托盘</strong>
                <small>关闭窗口后继续运行。</small>
              </span>
              <input
                v-model="settingsDraft.minimizeToTray"
                type="checkbox"
                @change="settingsDirty = true"
              />
            </label>
            <label class="switch-row">
              <span>
                <strong>登录 Windows 时自动启动</strong>
                <small>启动时自动恢复已登录连接。</small>
              </span>
              <input
                v-model="settingsDraft.launchAtLogin"
                type="checkbox"
                @change="settingsDirty = true"
              />
            </label>
          </section>
        </article>
      </section>

      <section v-else-if="tab === 'logs'" class="page">
        <header class="page-head">
          <div class="page-title">
            <p class="eyebrow">LOGS</p>
            <h1>日志</h1>
            <p class="muted code-path">
              {{
                logMode === "runtime"
                  ? snapshot?.logsPath
                  : snapshot?.httpLogsPath
              }}
            </p>
          </div>
          <div class="head-actions log-actions">
            <div class="mode-switch" aria-label="日志类型">
              <button
                type="button"
                :class="{ active: logMode === 'runtime' }"
                @click="logMode = 'runtime'; loadLogs()"
              >
                运行日志
              </button>
              <button
                type="button"
                :class="{ active: logMode === 'http' }"
                @click="logMode = 'http'; loadHttpTraces()"
              >
                HTTP 记录
              </button>
            </div>
            <label v-if="logMode === 'runtime'" class="filter-control">
              <select v-model="logFilter" aria-label="日志级别">
                <option value="all">全部</option>
                <option value="info">Info</option>
                <option value="warn">Warn</option>
                <option value="error">Error</option>
              </select>
            </label>
            <button
              type="button"
              class="secondary"
              @click="logMode === 'runtime' ? loadLogs() : loadHttpTraces()"
            >
              <RefreshCw :size="15" aria-hidden="true" />
              刷新
            </button>
            <button type="button" class="ghost" @click="run(openLogsFolder)">
              <FolderOpen :size="15" aria-hidden="true" />
              打开目录
            </button>
          </div>
        </header>

        <template v-if="logMode === 'runtime'">
          <div class="log-list">
            <div v-if="filteredLogs.length === 0" class="empty-state">
              <ScrollText :size="28" aria-hidden="true" />
              <strong>暂无日志</strong>
              <span>启动桥接后，运行信息会显示在这里。</span>
            </div>
            <div
              v-for="entry in filteredLogs"
              :key="`${entry.time}-${entry.message}`"
              class="log-row"
            >
              <span class="log-time">{{ entry.time }}</span>
              <span class="log-level" :class="entry.level">{{ entry.level }}</span>
              <span class="log-message">{{ entry.message }}</span>
            </div>
          </div>
        </template>

        <template v-else>
          <div class="trace-warning">
            <AlertTriangle :size="16" aria-hidden="true" />
            上游报文包含认证头、Cookie、API Key 和完整对话正文，请勿直接分享。
          </div>
          <div class="http-log-list">
            <div v-if="httpTraces.length === 0" class="empty-state">
              <ScrollText :size="28" aria-hidden="true" />
              <strong>暂无 HTTP 记录</strong>
              <span>请求发往上游并收到响应后，会写入一对平铺报文文件。</span>
            </div>
            <button
              v-for="trace in httpTraces"
              :key="trace.id"
              type="button"
              class="http-log-row"
              @click="openTrace(trace.id)"
            >
              <span class="http-log-main">
                <strong>{{ trace.method }} {{ trace.path }}</strong>
                <small>{{ trace.startedAt }}</small>
              </span>
              <span class="http-log-protocol">{{ trace.protocol }}</span>
              <span class="http-log-status" :class="{ failed: trace.status >= 400 }">
                {{ trace.status }}
              </span>
              <span class="http-log-size">
                {{ formatBytes(trace.requestBytes) }} /
                {{ formatBytes(trace.responseBytes) }}
              </span>
              <span class="http-log-duration">{{ trace.elapsedMs }} ms</span>
              <span
                v-if="trace.requestTruncated || trace.responseTruncated"
                class="http-log-truncated"
              >
                已截断
              </span>
              <FolderOpen :size="15" aria-hidden="true" />
            </button>
          </div>
        </template>
      </section>

      <section v-else class="page">
        <header class="page-head">
          <div class="page-title">
            <p class="eyebrow">ABOUT</p>
            <h1>CNB Bridge</h1>
            <p class="muted">把 CNB 云工作空间中的 AI 端点安全桥接到本机 loopback。</p>
          </div>
        </header>

        <article class="card details-card">
          <div class="section-head">
            <div>
              <span class="card-label">产品</span>
              <h2>应用信息</h2>
            </div>
            <Info :size="19" aria-hidden="true" />
          </div>
          <dl>
            <div><dt>版本</dt><dd><code>{{ snapshot?.version || "0.1.0" }}</code></dd></div>
            <div><dt>运行方式</dt><dd>Tauri 桌面客户端</dd></div>
            <div><dt>安全边界</dt><dd>仅监听 127.0.0.1</dd></div>
          </dl>
        </article>
      </section>
    </main>
  </div>
</template>
