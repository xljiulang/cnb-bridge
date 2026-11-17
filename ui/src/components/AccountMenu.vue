<script setup lang="ts">
import {
  ChevronDown,
  CircleUserRound,
  LoaderCircle,
  ScanQrCode,
  X,
} from "@lucide/vue";
import { computed, nextTick, onBeforeUnmount, ref } from "vue";
import type { SavedAccount } from "../types";
import AccountAvatar from "./AccountAvatar.vue";

const props = withDefaults(
  defineProps<{
    accounts: SavedAccount[];
    busyUsername?: string | null;
    disabled?: boolean;
  }>(),
  {
    busyUsername: null,
    disabled: false,
  },
);

const emit = defineEmits<{
  login: [username: string];
  remove: [username: string];
  scan: [];
}>();

const trigger = ref<HTMLButtonElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const open = ref(false);
const panelStyle = ref<Record<string, string>>({});

const triggerTitle = computed(() =>
  props.busyUsername ? "正在登录…" : "选择 CNB 账号",
);

function positionPanel() {
  const rect = trigger.value?.getBoundingClientRect();
  if (!rect) return;
  const preferredWidth = Math.min(460, Math.max(rect.width, 360));
  const width = Math.min(preferredWidth, window.innerWidth - 16);
  const estimatedHeight = Math.min(
    74 + props.accounts.length * 52 + 50,
    Math.min(440, window.innerHeight - 24),
  );
  const openAbove =
    rect.bottom + 8 + estimatedHeight > window.innerHeight - 8 &&
    rect.top - 8 > estimatedHeight;
  panelStyle.value = {
    left: `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`,
    top: openAbove
      ? `${Math.max(8, rect.top - estimatedHeight - 8)}px`
      : `${rect.bottom + 8}px`,
    width: `${width}px`,
  };
}

async function show() {
  if (props.disabled) return;
  open.value = true;
  positionPanel();
  await nextTick();
  panel.value?.querySelector<HTMLButtonElement>(".account-choice")?.focus();
}

function close(returnFocus = false) {
  open.value = false;
  if (returnFocus) trigger.value?.focus();
}

function choose(username: string) {
  if (props.busyUsername) return;
  emit("login", username);
  close(true);
}

function remove(username: string) {
  if (props.busyUsername) return;
  emit("remove", username);
}

function scan() {
  if (props.busyUsername) return;
  emit("scan");
  close(true);
}

function onDocumentPointer(event: PointerEvent) {
  const target = event.target as Node;
  if (!trigger.value?.contains(target) && !panel.value?.contains(target)) {
    close();
  }
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (event.key === "Escape" && open.value) {
    event.preventDefault();
    close(true);
  }
}

function onViewportChange() {
  if (open.value) positionPanel();
}

document.addEventListener("pointerdown", onDocumentPointer);
document.addEventListener("keydown", onDocumentKeydown);
window.addEventListener("resize", onViewportChange);
window.addEventListener("scroll", onViewportChange, true);

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocumentPointer);
  document.removeEventListener("keydown", onDocumentKeydown);
  window.removeEventListener("resize", onViewportChange);
  window.removeEventListener("scroll", onViewportChange, true);
});
</script>

<template>
  <div class="account-menu-entry">
    <button
      ref="trigger"
      type="button"
      class="account-picker-trigger"
      :disabled="disabled"
      aria-haspopup="dialog"
      :aria-expanded="open"
      aria-label="选择 CNB 账号"
      @click="open ? close() : show()"
    >
      <span class="account-picker-icon" aria-hidden="true">
        <CircleUserRound :size="20" />
      </span>
      <span class="account-picker-copy">
        <strong>{{ triggerTitle }}</strong>
      </span>
      <LoaderCircle
        v-if="busyUsername"
        class="spin"
        :size="16"
        aria-hidden="true"
      />
      <ChevronDown
        v-else
        class="account-picker-chevron"
        :class="{ open }"
        :size="17"
        aria-hidden="true"
      />
    </button>

    <Teleport to="body">
      <section
        v-if="open"
        ref="panel"
        class="account-menu"
        role="dialog"
        aria-label="已保存的 CNB 账号"
        :style="panelStyle"
      >
        <header class="account-menu-head">
          <strong>已保存账号</strong>
          <span>{{ accounts.length }}</span>
        </header>

        <div class="account-menu-list">
          <div
            v-for="account in accounts"
            :key="account.username"
            class="account-menu-row"
          >
            <button
              type="button"
              class="account-choice"
              :disabled="Boolean(busyUsername)"
              :aria-label="`使用账号 ${account.displayName} 登录`"
              @click="choose(account.username)"
            >
              <span class="account-choice-avatar" aria-hidden="true">
                <LoaderCircle
                  v-if="busyUsername === account.username"
                  class="spin"
                  :size="18"
                />
                <AccountAvatar
                  v-else
                  :avatar-url="account.avatarUrl"
                  :size="19"
                />
              </span>
              <span class="account-choice-copy">
                <strong>{{ account.displayName }}</strong>
              </span>
            </button>
            <button
              type="button"
              class="remove-saved-account"
              :disabled="Boolean(busyUsername) || account.active"
              :aria-label="`移除账号 ${account.displayName}`"
              :title="account.active ? '请先退出当前账号' : '从本机移除账号'"
              @click="remove(account.username)"
            >
              <X :size="16" aria-hidden="true" />
            </button>
          </div>
        </div>

        <footer class="account-menu-footer">
          <button
            type="button"
            class="account-choice add-account-action"
            :disabled="Boolean(busyUsername)"
            @click="scan"
          >
            <span class="account-choice-avatar" aria-hidden="true">
              <ScanQrCode :size="19" />
            </span>
            <span class="account-choice-copy">
              <strong>扫码登录 CNB 账号</strong>
            </span>
          </button>
        </footer>
      </section>
    </Teleport>
  </div>
</template>
