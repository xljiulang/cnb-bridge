<script setup lang="ts">
import { AlertTriangle, CheckCircle2, ExternalLink, X } from "@lucide/vue";
import AppSidebar from "./components/AppSidebar.vue";
import AppContextMenu from "./components/AppContextMenu.vue";
import ConnectView from "./components/ConnectView.vue";
import SettingsView from "./components/SettingsView.vue";
import LogsView from "./components/LogsView.vue";
import AboutView from "./components/AboutView.vue";
import { useDesktopApp } from "./composables/useDesktopApp";

const app = useDesktopApp();
</script>

<template>
  <AppContextMenu />
  <div class="shell">
    <AppSidebar :app="app" />

    <main class="content">
      <div v-if="app.error" class="banner error" role="alert">
        <AlertTriangle :size="18" aria-hidden="true" />
        <span>{{ app.error }}</span>
        <div class="banner-actions">
          <button
            v-if="app.accountErrorAction"
            type="button"
            class="secondary compact identity-verification-action"
            @click="app.openAccountErrorAction"
          >
            <ExternalLink :size="15" aria-hidden="true" />
            {{ app.accountErrorAction.label }}
          </button>
          <button
            v-if="app.snapshot?.bridge.repoCreationUrl"
            type="button"
            class="secondary compact"
            @click="app.openRepoCreation"
          >
            <ExternalLink :size="15" aria-hidden="true" />
            打开 CNB 创建仓库
          </button>
          <button
            type="button"
            class="icon-button"
            aria-label="关闭错误"
            @click="app.dismissError"
          >
            <X :size="16" aria-hidden="true" />
          </button>
        </div>
      </div>
      <div v-if="app.notice" class="banner notice" role="status">
        <CheckCircle2 :size="18" aria-hidden="true" />
        <span>{{ app.notice }}</span>
        <button
          type="button"
          class="icon-button"
          aria-label="关闭提示"
          @click="app.clearNotice"
        >
          <X :size="16" aria-hidden="true" />
        </button>
      </div>

      <ConnectView v-if="app.tab === 'connect'" :app="app" />
      <SettingsView v-else-if="app.tab === 'settings'" :app="app" />
      <LogsView v-else-if="app.tab === 'logs'" :app="app" />
      <AboutView v-else :app="app" />
    </main>
  </div>
  <Teleport to="body">
    <div
      v-if="app.pendingAccountRemoval"
      class="confirm-backdrop"
      @click.self="app.cancelRemoveSavedAccount"
    >
      <section
        class="confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="remove-account-title"
      >
        <span class="card-label">移除账号</span>
        <h2 id="remove-account-title">
          从本机移除 {{ app.pendingAccountRemoval.displayName }}？
        </h2>
        <p>账号凭据会从本机删除，之后需要重新扫码或登录才能恢复。</p>
        <div class="confirm-actions">
          <button
            type="button"
            class="secondary"
            :disabled="app.busy"
            @click="app.cancelRemoveSavedAccount"
          >
            取消
          </button>
          <button
            type="button"
            class="danger"
            :disabled="app.busy"
            @click="app.confirmRemoveSavedAccount"
          >
            移除账号
          </button>
        </div>
      </section>
    </div>
  </Teleport>
</template>
