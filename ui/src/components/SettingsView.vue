<script setup lang="ts">
import {
  CheckCircle2,
  FolderGit2,
  LoaderCircle,
  Network,
  Power,
  ScrollText,
} from "@lucide/vue";
import SelectMenu from "./SelectMenu.vue";
import type { DesktopApp } from "../composables/useDesktopApp";

defineProps<{ app: DesktopApp }>();
</script>

<template>
  <section class="page settings-page">
    <header class="page-head">
      <div class="page-title">
        <p class="eyebrow">SETTINGS</p>
        <h1>设置</h1>
      </div>
      <button
        type="button"
        :class="app.settingsDirty ? 'primary' : 'secondary'"
        :disabled="app.busy || !app.settingsDirty"
        @click="app.save"
      >
        <LoaderCircle v-if="app.busy" class="spin" :size="16" aria-hidden="true" />
        <CheckCircle2 v-else :size="16" aria-hidden="true" />
        {{ app.busy ? "保存中…" : app.settingsDirty ? "保存" : "已保存" }}
      </button>
    </header>

    <article class="card settings-card">
      <section class="form-section">
        <div class="section-head">
          <div>
            <span class="card-label">仓库</span>
            <h2>
              <FolderGit2 :size="16" aria-hidden="true" />
              创建位置
            </h2>
          </div>
        </div>
        <div class="form-grid">
          <label>
            <span>命名空间</span>
            <SelectMenu
              v-model="app.settingsDraft.repoNamespace"
              :options="app.namespaceOptions"
              aria-label="仓库命名空间"
            />
          </label>
          <label>
            <span>仓库名</span>
            <input
              v-model="app.settingsDraft.repoName"
              spellcheck="false"
            />
          </label>
        </div>
      </section>

      <section class="form-section divided">
        <div class="section-head">
          <div>
            <span class="card-label">日志</span>
            <h2>
              <ScrollText :size="16" aria-hidden="true" />
              HTTP 日志
            </h2>
          </div>
        </div>
        <label class="narrow-field">
          <span>记录策略</span>
          <SelectMenu
            v-model="app.settingsDraft.httpLogFilter"
            :options="app.httpLogOptions"
            aria-label="HTTP 日志记录策略"
          />
        </label>
      </section>

      <section class="form-section divided">
        <div class="section-head">
          <div>
            <span class="card-label">连接</span>
            <h2>
              <Network :size="16" aria-hidden="true" />
              本机端口
            </h2>
          </div>
        </div>
        <label class="narrow-field">
          <span>监听端口</span>
          <input
            v-model.number="app.settingsDraft.localPort"
            type="number"
            min="1"
            max="65535"
          />
        </label>
      </section>

      <section class="form-section divided">
        <div class="section-head">
          <div>
            <span class="card-label">桌面</span>
            <h2>
              <Power :size="16" aria-hidden="true" />
              启动行为
            </h2>
          </div>
        </div>
        <label class="switch-row">
          <span>
            <strong>登录 Windows 时自动启动</strong>
          </span>
          <input
            v-model="app.settingsDraft.launchAtLogin"
            type="checkbox"
          />
        </label>
      </section>
    </article>
  </section>
</template>
