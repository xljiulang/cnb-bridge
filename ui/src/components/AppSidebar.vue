<script setup lang="ts">
import {
  Cable,
  Info,
  ScrollText,
  Settings as SettingsIcon,
} from "@lucide/vue";
import appIcon from "../assets/cnb-favicon.svg";
import type { DesktopApp } from "../composables/useDesktopApp";
import SidebarAccountMenu from "./SidebarAccountMenu.vue";

defineProps<{ app: DesktopApp }>();
</script>

<template>
  <aside class="sidebar">
    <div class="brand">
      <img class="brand-mark" :src="appIcon" alt="" aria-hidden="true" />
      <div class="brand-copy">
        <div class="brand-name">CNB Bridge</div>
        <div class="brand-sub">本机 AI 连接</div>
      </div>
    </div>

    <nav class="nav" aria-label="主导航">
      <button
        type="button"
        :class="{ active: app.tab === 'connect' }"
        @click="app.openConnect"
      >
        <Cable :size="16" aria-hidden="true" />
        <span>连接</span>
        <span class="nav-status" :class="{ live: app.ready }" />
      </button>
      <button
        type="button"
        :class="{ active: app.tab === 'settings' }"
        @click="app.openSettings"
      >
        <SettingsIcon :size="16" aria-hidden="true" />
        <span>设置</span>
      </button>
      <button
        type="button"
        :class="{ active: app.tab === 'logs' }"
        @click="app.openLogs"
      >
        <ScrollText :size="16" aria-hidden="true" />
        <span>日志</span>
      </button>
      <button
        type="button"
        :class="{ active: app.tab === 'about' }"
        @click="app.openAbout"
      >
        <Info :size="16" aria-hidden="true" />
        <span>关于</span>
      </button>
    </nav>

    <SidebarAccountMenu
      :account-state="app.accountState"
      :accounts="app.accounts"
      :active-account="Boolean(app.activeAccount)"
      :busy-username="app.accountLoginUsername"
      :display-name="
        app.snapshot?.account.displayName ||
        (app.accountState === 'signingIn' ? '正在登录' : '登录 CNB')
      "
      :avatar-url="app.snapshot?.account.avatarUrl"
      :status-label="app.statusLabel"
      :live="app.ready"
      :disabled="app.busy || app.accountState === 'signingIn'"
      @login="app.loginSavedAccount"
      @remove="app.removeSavedAccount"
      @scan="app.login"
      @logout="app.logout"
    />
  </aside>
</template>
