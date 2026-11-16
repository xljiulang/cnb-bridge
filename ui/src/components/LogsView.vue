<script setup lang="ts">
import {
  AlertTriangle,
  FolderOpen,
  ListFilter,
  RefreshCw,
  ScrollText,
} from "@lucide/vue";
import SelectMenu from "./SelectMenu.vue";
import type { DesktopApp } from "../composables/useDesktopApp";

defineProps<{ app: DesktopApp }>();
</script>

<template>
  <section class="page">
    <header class="page-head">
      <div class="page-title">
        <p class="eyebrow">LOGS</p>
        <h1>日志</h1>
        <p class="muted code-path">
          {{ app.logMode === "runtime" ? app.snapshot?.logsPath : app.snapshot?.httpLogsPath }}
        </p>
      </div>
      <div class="head-actions log-actions">
        <div class="mode-switch" aria-label="日志类型">
          <button
            type="button"
            :class="{ active: app.logMode === 'runtime' }"
            @click="app.logMode = 'runtime'; app.loadLogs()"
          >
            运行日志
          </button>
          <button
            type="button"
            :class="{ active: app.logMode === 'http' }"
            @click="app.logMode = 'http'; app.loadHttpTraces()"
          >
            HTTP 记录
          </button>
        </div>
        <div
          v-if="app.logMode === 'runtime'"
          class="filter-control"
          title="筛选日志级别"
        >
          <ListFilter :size="15" aria-hidden="true" />
          <SelectMenu
            v-model="app.logFilter"
            :options="app.logLevelOptions"
            aria-label="日志级别"
          />
        </div>
        <button
          type="button"
          class="secondary"
          @click="app.logMode === 'runtime' ? app.loadLogs() : app.loadHttpTraces()"
        >
          <RefreshCw :size="15" aria-hidden="true" />
          刷新
        </button>
        <button type="button" class="ghost" @click="app.openLogsFolder">
          <FolderOpen :size="15" aria-hidden="true" />
          打开目录
        </button>
      </div>
    </header>

    <div v-if="app.logMode === 'runtime'" class="log-list">
      <div v-if="app.filteredLogs.length === 0" class="empty-state">
        <ScrollText :size="28" aria-hidden="true" />
        <strong>暂无日志</strong>
        <span>启动桥接后，运行信息会显示在这里。</span>
      </div>
      <div
        v-for="entry in app.filteredLogs"
        :key="`${entry.time}-${entry.message}`"
        class="log-row"
      >
        <span class="log-time">{{ entry.time }}</span>
        <span class="log-level" :class="entry.level">{{ entry.level }}</span>
        <span class="log-message">{{ entry.message }}</span>
      </div>
    </div>

    <template v-else>
      <div class="trace-warning">
        <AlertTriangle :size="16" aria-hidden="true" />
        上游报文包含认证头、Cookie、API Key 和完整对话正文，请勿直接分享。
        当前设置为记录{{ app.snapshot?.settings.httpLogFilter === "all" ? "所有请求" : "仅异常请求" }}。
      </div>
      <div class="http-log-list">
        <div v-if="app.httpTraces.length === 0" class="empty-state">
          <ScrollText :size="28" aria-hidden="true" />
          <strong>暂无 HTTP 记录</strong>
          <span>请求发往上游并收到响应后，会写入一对平铺报文文件。</span>
        </div>
        <div v-if="app.httpTraces.length > 0" class="http-log-header" aria-hidden="true">
          <span>请求</span>
          <span>协议</span>
          <span>状态</span>
          <span>请求 / 响应大小</span>
          <span class="http-log-duration">耗时</span>
        </div>
        <button
          v-for="trace in app.httpTraces"
          :key="trace.id"
          type="button"
          class="http-log-row"
          :aria-label="app.httpTraceLabel(trace)"
          @click="app.openTrace(trace.id)"
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
            {{ app.formatBytes(trace.requestBytes) }} /
            {{ app.formatBytes(trace.responseBytes) }}
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
</template>
