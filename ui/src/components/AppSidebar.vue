<script setup lang="ts">
import {
  Activity,
  AlertTriangle,
  Cable,
  CirclePause,
  Info,
  LoaderCircle,
  ScanQrCode,
  ScrollText,
  Server,
  Settings as SettingsIcon,
} from "@lucide/vue";
import appIcon from "../assets/cnb-favicon.svg";
import type { DesktopApp } from "../composables/useDesktopApp";

defineProps<{ app: DesktopApp }>();
</script>

<template>
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
        :class="{ active: app.tab === 'connect' }"
        @click="app.openConnect"
      >
        <Cable :size="17" aria-hidden="true" />
        <span>连接</span>
        <span class="nav-status" :class="{ live: app.ready }" />
      </button>
      <button
        type="button"
        :class="{ active: app.tab === 'settings' }"
        @click="app.openSettings"
      >
        <SettingsIcon :size="17" aria-hidden="true" />
        <span>设置</span>
      </button>
      <button
        type="button"
        :class="{ active: app.tab === 'logs' }"
        @click="app.openLogs"
      >
        <ScrollText :size="17" aria-hidden="true" />
        <span>日志</span>
      </button>
      <button
        type="button"
        :class="{ active: app.tab === 'about' }"
        @click="app.openAbout"
      >
        <Info :size="17" aria-hidden="true" />
        <span>关于</span>
      </button>
    </nav>

    <div class="sidebar-status" :class="{ live: app.ready }">
      <span class="status-icon">
        <ScanQrCode
          v-if="app.sidebarStatusIcon === 'scan'"
          :size="16"
          aria-hidden="true"
        />
        <Activity
          v-else-if="app.sidebarStatusIcon === 'live'"
          :size="16"
          aria-hidden="true"
        />
        <LoaderCircle
          v-else-if="app.sidebarStatusIcon === 'busy'"
          class="spin"
          :size="16"
          aria-hidden="true"
        />
        <CirclePause
          v-else-if="app.sidebarStatusIcon === 'pause'"
          :size="16"
          aria-hidden="true"
        />
        <AlertTriangle
          v-else-if="app.sidebarStatusIcon === 'error'"
          :size="16"
          aria-hidden="true"
        />
        <Server v-else :size="16" aria-hidden="true" />
      </span>
      <div class="status-copy">
        <strong>{{ app.statusLabel }}</strong>
        <small>
          {{
            app.accountConnected
              ? app.snapshot?.account.username || "CNB 已绑定"
              : "等待扫码"
          }}
        </small>
      </div>
      <span class="port-chip">{{ app.snapshot?.settings.localPort ?? 11233 }}</span>
    </div>
  </aside>
</template>
