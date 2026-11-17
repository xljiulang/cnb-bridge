<script setup lang="ts">
import { LogOut } from "@lucide/vue";
import { nextTick, onBeforeUnmount, ref } from "vue";
import AccountAvatar from "./AccountAvatar.vue";

const props = withDefaults(
  defineProps<{
    displayName: string;
    avatarUrl?: string | null;
    statusLabel: string;
    live?: boolean;
    disabled?: boolean;
  }>(),
  {
    avatarUrl: null,
    live: false,
    disabled: false,
  },
);

const emit = defineEmits<{
  logout: [];
}>();

const trigger = ref<HTMLButtonElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const open = ref(false);
const panelStyle = ref<Record<string, string>>({});

async function positionPanel() {
  const rect = trigger.value?.getBoundingClientRect();
  if (!rect) return;
  const width = Math.min(
    220,
    Math.max(176, rect.width),
    window.innerWidth - 16,
  );
  const height = panel.value?.offsetHeight || 52;
  const left = Math.max(8, Math.min(rect.left, window.innerWidth - width - 8));
  const top = Math.max(8, rect.top - height - 8);
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
  close();
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
      aria-label="打开账号操作"
      @click="toggle"
    >
      <span class="status-icon status-account-avatar">
        <AccountAvatar :avatar-url="avatarUrl" :size="18" />
      </span>
      <div class="status-copy">
        <strong>{{ displayName }}</strong>
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
        <button
          type="button"
          class="sidebar-logout-action"
          @click="logout"
        >
          <LogOut :size="15" aria-hidden="true" />
          退出登录
        </button>
      </section>
    </Teleport>
  </div>
</template>
