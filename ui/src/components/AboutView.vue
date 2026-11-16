<script setup lang="ts">
import { Info } from "@lucide/vue";
import type { DesktopApp } from "../composables/useDesktopApp";

defineProps<{ app: DesktopApp }>();
</script>

<template>
  <section class="page">
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
          <span class="card-label">应用</span>
          <h2>
            <Info :size="16" aria-hidden="true" />
            应用信息
          </h2>
        </div>
      </div>
      <dl>
        <div>
          <dt>版本</dt>
          <dd><code>{{ app.snapshot?.version || "0.1.0" }}</code></dd>
        </div>
        <div><dt>运行方式</dt><dd>Tauri 桌面客户端</dd></div>
        <div><dt>本机端点</dt><dd><code>{{ app.baseUrl }}</code></dd></div>
        <div><dt>作者</dt><dd>老九</dd></div>
      </dl>
    </article>
  </section>
</template>
