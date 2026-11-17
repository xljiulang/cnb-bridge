<script setup lang="ts">
import {
  LoaderCircle,
  LogOut,
  ScanQrCode,
  X,
} from "@lucide/vue";
import { computed, nextTick, onBeforeUnmount, ref } from "vue";
import type { AccountState, SavedAccount } from "../types";
import AccountAvatar from "./AccountAvatar.vue";

const props = withDefaults(
  defineProps<{
    displayName: string;
    avatarUrl?: string | null;
    statusLabel: string;
    accountState: AccountState;
    accounts: SavedAccount[];
    activeAccount: boolean;
    busyUsername?: string | null;
    live?: boolean;
    disabled?: boolean;
  }>(),
  {
    avatarUrl: null,
    busyUsername: null,
    live: false,
    disabled: false,
  },
);

const emit = defineEmits<{
  logout: [];
  login: [username: string];
  remove: [username: string];
  scan: [];
}>();

const trigger = ref<HTMLButtonElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const open = ref(false);
const panelStyle = ref<Record<string, string>>({});

const hasActiveAccount = computed(() => props.activeAccount);
const triggerName = computed(() => {
  if (props.displayName.trim()) return props.displayName;
  if (props.accountState === "signingIn") return "正在登录";
  return "登录 CNB";
});

async function positionPanel() {
  const rect = trigger.value?.getBoundingClientRect();
  if (!rect) return;
  const preferredWidth = hasActiveAccount.value
    ? Math.max(176, rect.width)
    : Math.max(320, rect.width);
  const width = Math.min(preferredWidth, window.innerWidth - 16);
  const height = panel.value?.offsetHeight || 52;
  const left = Math.max(8, Math.min(rect.left, window.innerWidth - width - 8));
  const above = rect.top - height - 8;
  const below = rect.bottom + 8;
  const top =
    above >= 8
      ? above
      : Math.max(8, Math.min(below, window.innerHeight - height - 8));
  panelStyle.value = {
    left: `${left}px`,
    top: `${top}px`,
    width: `${width}px`,
  };
}

async function show() {
  if (props.disabled) return;
  open.value = true;
  await nextTick();
  await positionPanel();
  panel.value?.querySelector<HTMLButtonElement>(".sidebar-logout-action")?.focus();
}

function close(returnFocus = false) {
  open.value = false;
  if (returnFocus) trigger.value?.focus();
}

async function toggle() {
  if (open.value) {
    close(true);
  } else {
    await show();
  }
}

function logout() {
  emit("logout");
  close(true);
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
  if (open.value) void positionPanel();
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
  <div class="sidebar-account-entry">
    <button
      ref="trigger"
      type="button"
      class="sidebar-status"
      :class="{ live }"
      :disabled="disabled"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :aria-label="hasActiveAccount ? '打开账号操作' : '打开登录菜单'"
      @click="toggle"
    >
      <span class="status-icon status-account-avatar">
        <LoaderCircle
          v-if="accountState === 'signingIn' && !avatarUrl"
          class="spin"
          :size="16"
          aria-hidden="true"
        />
        <AccountAvatar v-else :avatar-url="avatarUrl" :size="18" />
      </span>
      <div class="status-copy">
        <strong>{{ triggerName }}</strong>
      </div>
      <span
        class="badge sidebar-status-badge"
        :class="{ live }"
      >
        {{ statusLabel }}
      </span>
    </button>

    <Teleport to="body">
      <section
        v-if="open"
        ref="panel"
        class="sidebar-account-menu"
        role="dialog"
        aria-label="账号操作"
        :style="panelStyle"
      >
        <template v-if="hasActiveAccount">
          <button
            type="button"
            class="sidebar-logout-action"
            @click="logout"
          >
            <LogOut :size="15" aria-hidden="true" />
            退出登录
          </button>
        </template>
        <template v-else>
          <header class="account-menu-head">
            <strong>已保存账号</strong>
            <span>{{ accounts.length }}</span>
          </header>

          <div v-if="accounts.length" class="account-menu-list">
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
        </template>
      </section>
    </Teleport>
  </div>
</template>
