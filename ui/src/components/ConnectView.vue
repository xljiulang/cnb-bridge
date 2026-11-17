<script setup lang="ts">
import {
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  CircleUserRound,
  Copy,
  Cpu,
  KeyRound,
  LoaderCircle,
  LogOut,
  Network,
  Power,
  RefreshCw,
  ScanQrCode,
  Square,
} from "@lucide/vue";
import {
  formatContextWindow,
  formatInputModalities,
} from "../format";
import type { DesktopApp } from "../composables/useDesktopApp";
import AccountAvatar from "./AccountAvatar.vue";
import AccountMenu from "./AccountMenu.vue";

defineProps<{ app: DesktopApp }>();
</script>

<template>
  <section class="page">
    <header class="page-head">
      <div class="page-title">
        <p class="eyebrow">CNB BRIDGE</p>
        <div class="title-row">
          <h1>连接</h1>
          <span
            class="badge dot"
            :class="{ live: app.ready, busy: app.bridgeTransitioning || app.creatingOrganization }"
          >
            {{
              app.organizationRequired
                ? "准备组织"
                : app.ready
                  ? "在线"
                  : app.idle
                    ? "待命"
                    : app.bridgeTransitioning
                      ? "处理中"
                      : "待连接"
            }}
          </span>
        </div>
      </div>
      <div v-if="app.connectionDetailsVisible" class="head-actions">
        <button
          v-if="!app.ready"
          type="button"
          class="primary"
          :disabled="app.busy || app.bridgeTransitioning || !app.accountConnected"
          @click="app.startBridge"
        >
          <LoaderCircle
            v-if="app.bridgeTransitioning"
            class="spin"
            :size="15"
            aria-hidden="true"
          />
          <Power v-else :size="15" aria-hidden="true" />
          {{ app.mainActionLabel }}
        </button>
        <button
          v-else
          type="button"
          class="danger"
          :disabled="app.busy"
          @click="app.stopBridge"
        >
          <Square :size="15" aria-hidden="true" />
          停止桥接
        </button>
      </div>
    </header>

    <div class="connect-stack">
      <div class="connect-card-grid">
        <article
          class="card account-card connect-card"
          :class="{ 'account-card-signed-out': !app.connectionDetailsVisible }"
        >
          <div class="account-head">
            <div class="account-main">
              <span v-if="app.accountConnected" class="account-avatar">
                <AccountAvatar
                  :avatar-url="app.snapshot?.account.avatarUrl"
                  :size="23"
                />
              </span>
              <div v-if="app.accountConnected">
                <span class="card-label">CNB 账号</span>
                <strong>{{ app.snapshot?.account.displayName }}</strong>
              </div>
              <AccountMenu
                v-else-if="app.accounts.length"
                :accounts="app.accounts"
                :busy-username="app.accountLoginUsername"
                :disabled="app.busy || app.accountState === 'signingIn'"
                @login="app.loginSavedAccount"
                @remove="app.removeSavedAccount"
                @scan="app.login"
              />
              <div v-else class="empty-account-title">
                <span class="account-picker-icon" aria-hidden="true">
                  <CircleUserRound :size="20" />
                </span>
                <div>
                  <span class="card-label">CNB 账号</span>
                  <strong>登录 CNB</strong>
                </div>
              </div>
            </div>
            <span
              v-if="app.organizationRequired"
              class="badge busy"
            >
              {{
                app.creatingOrganization
                  ? "创建组织中"
                  : "组织未就绪"
              }}
            </span>
          </div>
          <p v-if="!app.accountConnected" class="account-intro">
            {{
              app.accounts.length
                ? "账号凭据只保存在本机。选择账号后会验证登录状态。"
                : "首次使用请扫码完成 CNB 授权，之后可以从已保存账号快速登录。"
            }}
          </p>
          <p
            v-else-if="app.organizationRequired"
            class="account-intro organization-intro"
            aria-live="polite"
          >
            {{
              app.creatingOrganization
                ? `正在为账号 ${app.snapshot?.account.displayName || ""} 自动创建 CNB 根组织…`
                : "CNB 根组织尚未就绪。系统会自动使用当前账号名创建组织。"
            }}
          </p>
          <div
            v-else-if="app.organizationReady"
            class="account-usage"
            :class="[
              {
                loading: app.usageLoading && !app.usage,
                unavailable: !app.usage || app.usageError,
              },
            ]"
            :title="app.usageError ?? undefined"
            aria-live="polite"
          >
            <div
              class="usage-section credits"
              :class="`is-${app.creditsUsageTone}`"
            >
              <div class="usage-summary">
                <span>AI Credits</span>
                <strong class="usage-amount">{{ app.creditsUsageAmount }}</strong>
              </div>
              <div class="usage-meter-row">
                <div
                  class="usage-progress-track"
                  role="progressbar"
                  aria-label="AI Credits 使用进度"
                  aria-valuemin="0"
                  aria-valuemax="100"
                  :aria-valuenow="app.creditsUsageProgress ?? undefined"
                  :aria-valuetext="app.creditsUsageAriaText"
                  :aria-busy="app.usageLoading"
                >
                  <span
                    v-if="app.usageLoading && !app.usage"
                    class="usage-progress-loading"
                  />
                  <span
                    v-else-if="app.creditsUsageProgress !== null"
                    class="usage-progress-fill"
                    :style="{ width: `${app.creditsUsageProgress}%` }"
                  />
                </div>
                <strong class="usage-percent">{{ app.creditsUsagePercent }}</strong>
              </div>
            </div>
            <div
              class="usage-section dev-cpu"
              :class="`is-${app.devCpuUsageTone}`"
            >
              <div class="usage-summary">
                <span>云原生开发-CPU</span>
                <strong class="usage-amount">{{ app.devCpuUsageAmount }}</strong>
              </div>
              <div class="usage-meter-row">
                <div
                  class="usage-progress-track"
                  role="progressbar"
                  aria-label="云原生开发-CPU 使用进度"
                  aria-valuemin="0"
                  aria-valuemax="100"
                  :aria-valuenow="app.devCpuUsageProgress ?? undefined"
                  :aria-valuetext="app.devCpuUsageAriaText"
                  :aria-busy="app.usageLoading"
                >
                  <span
                    v-if="app.usageLoading && !app.usage"
                    class="usage-progress-loading"
                  />
                  <span
                    v-else-if="app.devCpuUsageProgress !== null"
                    class="usage-progress-fill"
                    :style="{ width: `${app.devCpuUsageProgress}%` }"
                  />
                </div>
                <strong class="usage-percent">{{ app.devCpuUsagePercent }}</strong>
              </div>
            </div>
          </div>
          <div class="account-actions">
            <template v-if="!app.accountConnected">
              <button
                v-if="!app.accounts.length"
                type="button"
                class="primary"
                :disabled="
                  app.busy ||
                  app.accountState === 'signingIn' ||
                  Boolean(app.activeAccount)
                "
                @click="app.login"
              >
                <ScanQrCode
                  :class="{ 'scan-waiting': app.accountState === 'signingIn' }"
                  :size="15"
                  aria-hidden="true"
                />
                {{ app.accountState === "signingIn" ? "等待扫码" : "扫码绑定 CNB" }}
              </button>
              <button
                v-if="app.accountState === 'signingIn'"
                type="button"
                class="ghost"
                :disabled="app.busy"
                @click="app.cancelLogin"
              >
                取消扫码
              </button>
              <button
                v-if="app.activeAccount"
                type="button"
                class="danger compact"
                :disabled="app.busy"
                @click="app.logout"
              >
                <LogOut :size="15" aria-hidden="true" />
                退出登录
              </button>
            </template>
            <button
              v-if="app.organizationRequired"
              type="button"
              class="primary"
              :disabled="app.busy"
              @click="app.createOrganization"
            >
              <LoaderCircle
                v-if="app.creatingOrganization"
                class="spin"
                :size="16"
                aria-hidden="true"
              />
              <RefreshCw v-else :size="16" aria-hidden="true" />
              {{ app.creatingOrganization ? "正在创建…" : "重试创建组织" }}
            </button>
          </div>
        </article>

        <template v-if="app.connectionDetailsVisible">
          <article class="card connect-card model-card">
            <div class="section-head">
              <div>
                <span class="card-label">模型目录</span>
                <h2>
                  <Cpu :size="16" aria-hidden="true" />
                  可用于 Chat 与 Responses
                </h2>
              </div>
              <span class="card-meta">
                {{ app.snapshot?.models.length ?? 0 }} 个模型
              </span>
            </div>
            <div class="model-list">
              <section
                v-for="model in app.snapshot?.models ?? []"
                :key="model.id"
                class="model-item"
              >
                <div class="model-item-head">
                  <div class="model-title">
                    <div class="model-name-row">
                      <code>{{ model.id }}</code>
                      <span class="badge live">可用</span>
                    </div>
                  </div>
                  <button
                    type="button"
                    class="secondary compact copy-button"
                    :aria-label="
                      app.copiedKey === `model:${model.id}`
                        ? `${model.id} 已复制`
                        : `复制模型 ID ${model.id}`
                    "
                    @click="app.copyValue(`model:${model.id}`, model.id)"
                  >
                    <CheckCircle2
                      v-if="app.copiedKey === `model:${model.id}`"
                      :size="15"
                      aria-hidden="true"
                    />
                    <Copy v-else :size="15" aria-hidden="true" />
                    {{ app.copiedKey === `model:${model.id}` ? "已复制" : "复制 ID" }}
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

          <article
            class="card connect-card endpoint-card"
            :class="{ 'is-live': app.ready }"
          >
            <div class="section-head">
              <div>
                <span class="card-label">接入端点</span>
                <h2>
                  <Network :size="16" aria-hidden="true" />
                  本机 OpenAI 兼容地址
                </h2>
              </div>
              <span class="badge" :class="app.ready ? 'live' : 'muted'">
                {{ app.ready ? "监听中" : app.idle ? "按需启动" : "未连接" }}
              </span>
            </div>
            <code class="endpoint-url">{{ app.baseUrl }}</code>
            <div class="endpoint-meta">
              <button
                type="button"
                class="secondary compact copy-button"
                :aria-label="
                  app.copiedKey === 'endpoint' ? `${app.baseUrl} 已复制` : '复制本机端点地址'
                "
                @click="app.copyValue('endpoint', app.baseUrl)"
              >
                <CheckCircle2
                  v-if="app.copiedKey === 'endpoint'"
                  :size="15"
                  aria-hidden="true"
                />
                <Copy v-else :size="15" aria-hidden="true" />
                {{ app.copiedKey === "endpoint" ? "已复制" : "复制地址" }}
              </button>
            </div>
          </article>

          <article class="card connect-card api-key-card">
            <div class="section-head">
              <div>
                <span class="card-label">客户端鉴权</span>
                <h2>
                  <KeyRound :size="16" aria-hidden="true" />
                  API Key 可任意填写
                </h2>
              </div>
            </div>
            <p>
              网关不校验客户端 API Key，可填写 <code>local</code>、任意占位值或留空。
              真正的 CNB 鉴权来自已登录账号。
            </p>
          </article>
        </template>
      </div>

      <article v-if="app.connectionDetailsVisible" class="card pipeline-card">
        <div class="section-head">
          <div>
            <span class="card-label">启动链路</span>
            <h2>自动连接状态</h2>
          </div>
          <span class="card-meta">
            端口 {{ app.snapshot?.settings.localPort ?? 11233 }}
          </span>
        </div>
        <div class="pipeline">
          <div
            v-for="(step, index) in app.pipelineSteps"
            :key="step.label"
            class="step"
            :class="step.tone"
          >
            <span class="step-icon">
              <CheckCircle2 v-if="step.tone === 'done'" :size="16" aria-hidden="true" />
              <LoaderCircle
                v-else-if="step.tone === 'current' && app.bridgeTransitioning"
                class="spin"
                :size="16"
                aria-hidden="true"
              />
              <AlertTriangle v-else-if="step.tone === 'error'" :size="16" aria-hidden="true" />
              <span v-else>{{ index + 1 }}</span>
            </span>
            <div>
              <strong>{{ step.label }}</strong>
              <small>{{ step.detail }}</small>
            </div>
            <ChevronRight
              v-if="index < app.pipelineSteps.length - 1"
              class="step-arrow"
              :size="15"
              aria-hidden="true"
            />
          </div>
        </div>
      </article>
    </div>
  </section>
</template>
