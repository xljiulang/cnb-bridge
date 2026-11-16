<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  cancelLogin,
  ensureSshKey,
  getLogs,
  getSnapshot,
  logout,
  openExternal,
  openLogsFolder,
  openSshKeysPage,
  regenerateSshKey,
  saveSettings,
  startBridge,
  startLogin,
  stopBridge,
} from "./api";
import {
  bridgeLabel,
  formatError,
  shortenKey,
  validatePort,
} from "./format";
import type { AppSnapshot, LogEntry, Settings } from "./types";

type Tab = "connect" | "settings" | "logs" | "about";

const snapshot = ref<AppSnapshot | null>(null);
const tab = ref<Tab>("connect");
const busy = ref(false);
const error = ref<string | null>(null);
const notice = ref<string | null>(null);
const logs = ref<LogEntry[]>([]);
const logFilter = ref("all");
const settingsDirty = ref(false);
const settingsDraft = reactive<Settings>({
  repoName: "cnb-ai-bridge",
  localPort: 11233,
  minimizeToTray: true,
  launchAtLogin: false,
});
let timer: number | undefined;
let unlisten: UnlistenFn | undefined;

const ready = computed(() => snapshot.value?.bridge.state === "ready");
const accountState = computed(() => snapshot.value?.account.state ?? "signedOut");
const statusLabel = computed(() =>
  snapshot.value ? bridgeLabel(snapshot.value.bridge.state) : "正在加载",
);
const baseUrl = computed(
  () => snapshot.value?.bridge.baseUrl ?? `http://127.0.0.1:${snapshot.value?.settings.localPort ?? 11233}/v1`,
);
const publicKey = computed(() => snapshot.value?.bridge.sshPublicKey ?? null);
const filteredLogs = computed(() =>
  logFilter.value === "all"
    ? logs.value
    : logs.value.filter((entry) => entry.level === logFilter.value),
);

async function refresh() {
  try {
    const next = await getSnapshot();
    snapshot.value = next;
    if (!settingsDirty.value) {
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
  busy.value = true;
  error.value = null;
  notice.value = null;
  try {
    await action();
    if (message) notice.value = message;
    await refresh();
  } catch (cause) {
    error.value = formatError(cause);
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
    notice.value = message;
  } catch (cause) {
    error.value = formatError(cause);
  }
}

async function makeKey() {
  await run(async () => {
    await ensureSshKey();
  });
}

async function remakeKey() {
  await run(async () => {
    await regenerateSshKey();
  }, "已生成新的 SSH 密钥，请重新添加到 CNB 公钥管理");
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

onMounted(async () => {
  await refresh();
  await makeKey();
  await loadLogs();
  timer = window.setInterval(() => {
    void refresh();
    if (tab.value === "logs") void loadLogs();
  }, 1500);
  unlisten = await listen("cnb-bridge://snapshot", () => {
    void refresh();
  });
});

onBeforeUnmount(() => {
  if (timer) window.clearInterval(timer);
  unlisten?.();
});
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-mark">CB</div>
        <div>
          <div class="brand-name">CNB Bridge</div>
          <div class="brand-sub">本机 AI 桥</div>
        </div>
      </div>

      <nav class="nav">
        <button :class="{ active: tab === 'connect' }" @click="tab = 'connect'">
          <span class="nav-dot" :class="{ live: ready }" />
          连接
        </button>
        <button :class="{ active: tab === 'settings' }" @click="tab = 'settings'">
          设置
        </button>
        <button :class="{ active: tab === 'logs' }" @click="tab = 'logs'">
          日志
        </button>
        <button :class="{ active: tab === 'about' }" @click="tab = 'about'">
          关于
        </button>
      </nav>

      <div class="sidebar-status">
        <span class="status-dot" :class="{ live: ready }" />
        <div>
          <strong>{{ statusLabel }}</strong>
          <small>{{ accountState === "connected" ? "CNB 已登录" : "等待登录" }}</small>
        </div>
      </div>
    </aside>

    <main class="content">
      <div v-if="error" class="banner error">
        <span>{{ error }}</span>
        <button class="ghost" @click="error = null">关闭</button>
      </div>
      <div v-if="notice" class="banner notice">
        <span>{{ notice }}</span>
        <button class="ghost" @click="notice = null">关闭</button>
      </div>

      <section v-if="tab === 'connect'" class="page">
        <header class="page-head">
          <div>
            <p class="eyebrow">LOCAL AI BRIDGE</p>
            <h1>{{ statusLabel }}</h1>
            <p class="muted">
              {{ ready ? "本机 OpenAI 兼容端点已就绪。" : "登录后自动完成建仓、启动云主机与 SSH 通道。" }}
            </p>
          </div>
          <div class="head-actions">
            <button
              v-if="!ready"
              class="primary"
              :disabled="busy || accountState !== 'connected'"
              @click="run(startBridge)"
            >
              启动桥接
            </button>
            <button v-else class="danger" :disabled="busy" @click="run(stopBridge)">
              停止桥接
            </button>
          </div>
        </header>

        <div class="endpoint-card" :class="{ live: ready }">
          <div>
            <span class="eyebrow">BASE URL</span>
            <code>{{ baseUrl }}</code>
          </div>
          <button class="ghost" @click="copyText(baseUrl, 'Base URL 已复制')">复制</button>
        </div>

        <div class="grid two">
          <article class="card">
            <div class="card-head">
              <h2>CNB 账号</h2>
              <span class="pill" :class="accountState">{{ accountState === "connected" ? "已登录" : "未登录" }}</span>
            </div>
            <p class="muted">
              {{ snapshot?.account.username || "使用系统浏览器完成 CNB 设备授权。" }}
            </p>
            <div v-if="snapshot?.account.userCode" class="device-code">
              <span>设备码</span>
              <strong>{{ snapshot.account.userCode }}</strong>
              <button class="ghost" @click="copyText(snapshot.account.userCode!, '设备码已复制')">复制</button>
            </div>
            <button
              v-if="snapshot?.account.verificationUrl"
              class="link link-button"
              @click="openExternal(snapshot.account.verificationUrl!)"
            >
              重新打开授权页面
            </button>
            <div class="card-actions">
              <button
                v-if="accountState !== 'connected'"
                class="primary"
                :disabled="busy || accountState === 'signingIn'"
                @click="login"
              >
                登录 CNB
              </button>
              <button v-else class="secondary" :disabled="busy" @click="run(logout)">
                退出登录
              </button>
              <button
                v-if="accountState === 'signingIn'"
                class="ghost"
                :disabled="busy"
                @click="run(cancelLogin)"
              >
                取消
              </button>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h2>专用 SSH 密钥</h2>
              <span class="pill">Ed25519</span>
            </div>
            <p class="muted">
              客户端自行生成并加密保存，不使用系统 ~/.ssh 中的任何密钥。
            </p>
            <code class="key-preview">{{ shortenKey(publicKey) }}</code>
            <div class="card-actions">
              <button class="secondary" :disabled="!publicKey" @click="copyText(publicKey!, 'SSH 公钥已复制')">
                复制公钥
              </button>
              <button class="secondary" @click="run(openSshKeysPage)">打开 CNB 公钥管理</button>
              <button class="ghost" :disabled="busy" @click="remakeKey">重新生成</button>
            </div>
          </article>
        </div>

        <article class="card">
          <div class="card-head">
            <h2>桥接链路</h2>
            <span class="muted small">端口 {{ snapshot?.settings.localPort ?? 11233 }}</span>
          </div>
          <div class="pipeline">
            <div class="step" :class="{ done: !!snapshot?.account.username }">
              <span>1</span>
              <div><strong>CNB 登录</strong><small>{{ snapshot?.account.username || "待登录" }}</small></div>
            </div>
            <div class="step" :class="{ done: !!publicKey }">
              <span>2</span>
              <div><strong>SSH 密钥</strong><small>{{ publicKey ? "已生成" : "待生成" }}</small></div>
            </div>
            <div class="step" :class="{ done: !!snapshot?.bridge.repoSlug }">
              <span>3</span>
              <div><strong>仓库</strong><small>{{ snapshot?.bridge.repoSlug || "待准备" }}</small></div>
            </div>
            <div class="step" :class="{ done: !!snapshot?.bridge.workspaceSn }">
              <span>4</span>
              <div><strong>云主机</strong><small>{{ snapshot?.bridge.workspaceSn || "待启动" }}</small></div>
            </div>
            <div class="step" :class="{ done: ready }">
              <span>5</span>
              <div><strong>本机端点</strong><small>{{ ready ? "11233 已就绪" : "待建立通道" }}</small></div>
            </div>
          </div>
        </article>
      </section>

      <section v-else-if="tab === 'settings'" class="page">
        <header class="page-head">
          <div>
            <p class="eyebrow">SETTINGS</p>
            <h1>设置</h1>
          </div>
          <button class="primary" :disabled="busy" @click="save">保存</button>
        </header>

        <article class="card form-card">
          <label>
            <span>仓库名</span>
            <input v-model="settingsDraft.repoName" spellcheck="false" @input="settingsDirty = true" />
            <small>自动创建在 CNB 用户名下，默认 cnb-ai-bridge。</small>
          </label>
          <label>
            <span>本地端口</span>
            <input v-model.number="settingsDraft.localPort" type="number" min="1" max="65535" @input="settingsDirty = true" />
            <small>SSH 本地转发与工作空间 agent 均使用该端口；远端固定 11233。</small>
          </label>
          <label class="switch-row">
            <span>关闭窗口时最小化到托盘</span>
            <input v-model="settingsDraft.minimizeToTray" type="checkbox" @change="settingsDirty = true" />
          </label>
          <label class="switch-row">
            <span>登录 Windows 时自动启动</span>
            <input v-model="settingsDraft.launchAtLogin" type="checkbox" @change="settingsDirty = true" />
          </label>
          <div class="danger-zone">
            <div>
              <strong>重新生成 SSH 密钥</strong>
              <small>生成后需要到 CNB 公钥管理页重新添加。</small>
            </div>
            <button class="ghost" :disabled="busy" @click="remakeKey">重新生成</button>
          </div>
        </article>
      </section>

      <section v-else-if="tab === 'logs'" class="page">
        <header class="page-head">
          <div>
            <p class="eyebrow">RUNTIME LOGS</p>
            <h1>日志</h1>
            <p class="muted small">{{ snapshot?.logsPath }}</p>
          </div>
          <div class="head-actions">
            <select v-model="logFilter">
              <option value="all">全部</option>
              <option value="info">info</option>
              <option value="warn">warn</option>
              <option value="error">error</option>
            </select>
            <button class="secondary" @click="loadLogs">刷新</button>
            <button class="ghost" @click="run(openLogsFolder)">打开目录</button>
          </div>
        </header>
        <div class="log-list">
          <div v-if="filteredLogs.length === 0" class="empty">暂无日志</div>
          <div v-for="entry in filteredLogs" :key="`${entry.time}-${entry.message}`" class="log-row">
            <span class="log-time">{{ entry.time }}</span>
            <span class="log-level" :class="entry.level">{{ entry.level }}</span>
            <span class="log-message">{{ entry.message }}</span>
          </div>
        </div>
      </section>

      <section v-else class="page">
        <header class="page-head">
          <div>
            <p class="eyebrow">ABOUT</p>
            <h1>CNB Bridge</h1>
            <p class="muted">把 CNB 云工作空间中的 AI 端点桥接到本机 loopback。</p>
          </div>
        </header>
        <article class="card about-card">
          <div><span>版本</span><code>{{ snapshot?.version || "0.1.0" }}</code></div>
          <div><span>Base URL</span><code>http://127.0.0.1:11233/v1</code></div>
          <div><span>健康检查</span><code>http://127.0.0.1:11233/healthz</code></div>
          <div><span>模型列表</span><code>/models · /v1/models</code></div>
          <div><span>Chat</span><code>/chat/completions · /v1/chat/completions</code></div>
          <div><span>安全</span><code>仅监听 127.0.0.1</code></div>
        </article>
      </section>
    </main>
  </div>
</template>
