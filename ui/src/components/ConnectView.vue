<script setup lang="ts">
import {
  AlertTriangle,
  ArrowDownLeft,
  CheckCircle2,
  ChevronRight,
  CircleUserRound,
  Copy,
  Cpu,
  KeyRound,
  LoaderCircle,
  Network,
  Power,
  RefreshCw,
  Square,
} from "@lucide/vue";
import {
  formatContextWindow,
  formatInputModalities,
} from "../format";
import type { DesktopApp } from "../composables/useDesktopApp";
import AccountAvatar from "./AccountAvatar.vue";

defineProps<{ app: DesktopApp }>();
</script>

<template>
  <section class="page" :class="{ 'page-signed-out': !app.accountConnected }">
    <header class="page-head">
      <div class="page-title">
        <p class="eyebrow">CNB BRIDGE</p>
        <div class="title-row">
          <h1>连接</h1>
          <span
            class="badge dot"
            :class="{ live: app.ready, busy: app.startingBridge || app.bridgeTransitioning || app.creatingOrganization }"
          >
            {{
              !app.accountConnected
                ? app.accountState === "signingIn"
                  ? "等待授权"
                  : app.accountState === "failed"
                    ? "登录失败"
                    : "未登录"
                : app.organizationRequired
                ? "准备组织"
                : app.bridgeState === "stopping"
                  ? "正在断开"
                  : app.ready
                    ? "在线"
                    : app.startingBridge
                      ? "正在桥接"
                      : app.idle
                        ? "按需连接"
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
          :disabled="app.busy || app.startingBridge || app.bridgeTransitioning || !app.accountConnected"
          @click="app.startBridge"
        >
          <LoaderCircle
            v-if="app.startingBridge || app.bridgeTransitioning"
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
          断开桥接
        </button>
      </div>
    </header>

    <div v-if="!app.accountConnected" class="connect-signed-out">
      <div
        class="connect-signed-out-icon"
        :class="{ busy: app.accountState === 'signingIn' }"
        aria-hidden="true"
      >
        <LoaderCircle
          v-if="app.accountState === 'signingIn'"
          class="spin"
          :size="30"
        />
        <CircleUserRound v-else :size="30" />
      </div>
      <p class="eyebrow">
        {{ app.accountState === "signingIn" ? "CNB 登录" : "需要 CNB 账号" }}
      </p>
      <h2>
        {{
          app.accountState === "signingIn"
            ? app.snapshot?.account.displayName
              ? `正在登录 ${app.snapshot.account.displayName}`
              : "正在登录 CNB"
            : app.accountState === "failed"
              ? "登录未完成"
              : "登录后开始本机桥接"
        }}
      </h2>
      <p class="connect-signed-out-copy">
        {{
          app.accountState === "signingIn"
            ? "请完成账号验证。关闭扫码窗口即可取消本次登录。"
            : app.accountState === "failed"
              ? "请从左下角账号卡重新选择账号或扫码登录。"
              : "选择已保存账号或扫码登录 CNB。登录后即可查看模型、端点和桥接状态。"
        }}
      </p>
      <div class="connect-location-hint">
        <ArrowDownLeft :size="16" aria-hidden="true" />
        <span>账号入口位于左下角</span>
      </div>
    </div>

    <div v-else class="connect-stack">
      <div class="connect-card-grid">
        <article class="card account-card connect-card">
          <div class="account-head">
            <div class="account-main">
              <span class="account-avatar">
                <AccountAvatar
                  :avatar-url="app.snapshot?.account.avatarUrl"
                  :size="23"
                />
              </span>
              <div>
                <span class="card-label">CNB 账号</span>
                <strong>{{ app.snapshot?.account.displayName }}</strong>
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
          <p
            v-if="app.organizationRequired"
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
          <div v-if="app.organizationRequired" class="account-actions">
            <button
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

          <article
            class="card connect-card endpoint-card"
            :class="{ 'is-live': app.localEndpointReady }"
          >
            <div class="section-head">
              <div>
                <span class="card-label">接入端点</span>
                <h2>
                  <Network :size="16" aria-hidden="true" />
                  本机 OpenAI 兼容地址
                </h2>
              </div>
              <span
                class="badge"
                :class="app.localEndpointReady ? 'live' : 'muted'"
              >
                {{ app.localEndpointReady ? "监听中" : "未监听" }}
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

          <article class="card connect-card model-card">
            <div class="section-head">
              <div>
                <span class="card-label">模型目录</span>
                <h2>
                  <Cpu :size="16" aria-hidden="true" />
                  可用于 Chat 与 Responses
                </h2>
              </div>
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

        </template>
      </div>

      <article v-if="app.connectionDetailsVisible" class="card pipeline-card">
        <div class="section-head">
          <div>
            <span class="card-label">桥接流程</span>
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
                v-else-if="step.tone === 'current' && (app.startingBridge || app.bridgeTransitioning)"
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
