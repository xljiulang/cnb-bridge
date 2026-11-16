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

    <div class="settings-stack">
      <article class="card settings-card">
        <div class="settings-card-intro">
          <span class="card-label">仓库</span>
          <h2>
            <FolderGit2 :size="16" aria-hidden="true" />
            创建位置
          </h2>
          <p>桥接配置会保存在该 CNB 仓库中。</p>
        </div>
        <div class="settings-fields">
          <div class="settings-field-grid">
            <label>
              <span>命名空间</span>
              <SelectMenu
                v-model="app.settingsDraft.repoNamespace"
                :options="app.namespaceOptions"
                aria-label="仓库命名空间"
              />
              <small>从已登录账号的 CNB 命名空间中选择。</small>
            </label>
            <label>
              <span>仓库名</span>
              <input
                v-model="app.settingsDraft.repoName"
                spellcheck="false"
              />
              <small>桥接隧道与配置使用的仓库名称。</small>
            </label>
          </div>
        </div>
      </article>

      <article class="card settings-card">
        <div class="settings-card-intro">
          <span class="card-label">日志</span>
          <h2>
            <ScrollText :size="16" aria-hidden="true" />
            HTTP 日志
          </h2>
          <p>控制请求日志的写入范围，便于排查问题。</p>
        </div>
        <div class="settings-fields">
          <label class="narrow-field">
            <span>记录策略</span>
            <SelectMenu
              v-model="app.settingsDraft.httpLogFilter"
              :options="app.httpLogOptions"
              aria-label="HTTP 日志记录策略"
            />
            <small>仅异常时只保存失败响应，便于控制磁盘占用。</small>
          </label>
        </div>
      </article>

      <article class="card settings-card">
        <div class="settings-card-intro">
          <span class="card-label">连接</span>
          <h2>
            <Network :size="16" aria-hidden="true" />
            本机端口
          </h2>
          <p>OpenAI 兼容端点监听的本地端口。</p>
        </div>
        <div class="settings-fields">
          <label class="narrow-field">
            <span>监听端口</span>
            <input
              v-model.number="app.settingsDraft.localPort"
              type="number"
              min="1"
              max="65535"
            />
            <small>默认只监听本机，修改后网关立即重新监听。</small>
          </label>
        </div>
      </article>

      <article class="card settings-card">
        <div class="settings-card-intro">
          <span class="card-label">桌面</span>
          <h2>
            <Power :size="16" aria-hidden="true" />
            启动行为
          </h2>
          <p>控制应用随系统启动的行为。</p>
        </div>
        <div class="settings-fields">
          <label class="switch-row">
            <input
              v-model="app.settingsDraft.launchAtLogin"
              type="checkbox"
            />
            <span>
              <strong>登录 Windows 时自动启动</strong>
              <small>登录 Windows 后随系统在托盘启动。</small>
            </span>
          </label>
        </div>
      </article>
    </div>
  </section>
</template>
