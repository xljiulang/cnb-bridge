<script setup lang="ts">
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  ClipboardPaste,
  Copy as CopyIcon,
  Redo2,
  Scissors,
  TextSelect,
  Undo2,
} from "@lucide/vue";
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  type Component,
} from "vue";

type MenuAction = "undo" | "redo" | "cut" | "copy" | "paste" | "selectAll";

interface MenuItem {
  action: MenuAction;
  label: string;
  icon: Component;
  iconName: string;
}

const menuItems: MenuItem[] = [
  { action: "undo", label: "撤销", icon: Undo2, iconName: "Undo2" },
  { action: "redo", label: "重做", icon: Redo2, iconName: "Redo2" },
  { action: "cut", label: "剪切", icon: Scissors, iconName: "Scissors" },
  { action: "copy", label: "复制", icon: CopyIcon, iconName: "Copy" },
  {
    action: "paste",
    label: "粘贴",
    icon: ClipboardPaste,
    iconName: "ClipboardPaste",
  },
  {
    action: "selectAll",
    label: "全选",
    icon: TextSelect,
    iconName: "TextSelect",
  },
];

const open = ref(false);
const x = ref(0);
const y = ref(0);
const menu = ref<HTMLElement | null>(null);
const target = ref<HTMLElement | null>(null);
let previousFocus: HTMLElement | null = null;

const availability = reactive({
  undo: false,
  redo: false,
  cut: false,
  copy: false,
  paste: false,
  selectAll: false,
});

const menuStyle = computed(() => ({
  left: `${x.value}px`,
  top: `${y.value}px`,
}));

function isTextInput(element: HTMLElement | null): element is HTMLInputElement {
  if (!(element instanceof HTMLInputElement)) return false;
  return !["button", "checkbox", "color", "file", "hidden", "image", "radio", "range", "reset", "submit"].includes(
    element.type,
  );
}

function isEditableElement(element: HTMLElement | null): boolean {
  return (
    isTextInput(element) ||
    element instanceof HTMLTextAreaElement ||
    Boolean(element?.isContentEditable)
  );
}

function controlSelection(element: HTMLInputElement | HTMLTextAreaElement) {
  const start = element.selectionStart ?? element.value.length;
  const end = element.selectionEnd ?? start;
  return { start, end, text: element.value.slice(start, end) };
}

function selectedText(): string {
  const element = target.value;
  if (isTextInput(element) || element instanceof HTMLTextAreaElement) {
    return element.type === "password" ? "" : controlSelection(element).text;
  }
  return window.getSelection()?.toString() ?? "";
}

function queryCommandEnabled(command: string): boolean {
  try {
    return document.queryCommandEnabled(command);
  } catch {
    return false;
  }
}

function refreshAvailability() {
  const element = target.value;
  const editable = isEditableElement(element);
  const text = selectedText();
  availability.undo = editable && queryCommandEnabled("undo");
  availability.redo = editable && queryCommandEnabled("redo");
  availability.cut = editable && text.length > 0;
  availability.copy = text.length > 0;
  availability.paste = editable;
  availability.selectAll = editable || Boolean(document.body.textContent?.trim());
}

function focusFirstAvailable() {
  const item = [...(menu.value?.querySelectorAll<HTMLButtonElement>(
    '[role="menuitem"]:not(:disabled)',
  ) ?? [])][0];
  item?.focus();
}

async function positionMenu(clientX: number, clientY: number) {
  x.value = clientX;
  y.value = clientY;
  await nextTick();
  const bounds = menu.value?.getBoundingClientRect();
  if (!bounds) return;
  const margin = 8;
  x.value = Math.max(
    margin,
    Math.min(clientX, window.innerWidth - bounds.width - margin),
  );
  y.value = Math.max(
    margin,
    Math.min(clientY, window.innerHeight - bounds.height - margin),
  );
}

async function show(event: MouseEvent) {
  event.preventDefault();
  event.stopPropagation();
  previousFocus =
    document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
  target.value =
    event.target instanceof HTMLElement ? event.target : null;
  refreshAvailability();
  open.value = true;
  await positionMenu(event.clientX, event.clientY);
  focusFirstAvailable();
}

function close(restoreFocus: boolean) {
  if (!open.value) return;
  open.value = false;
  if (restoreFocus) {
    previousFocus?.focus();
  }
}

function dispatchInput(
  element: HTMLInputElement | HTMLTextAreaElement,
  inputType: string,
  data: string | null,
) {
  element.dispatchEvent(
    new InputEvent("input", {
      bubbles: true,
      inputType,
      data,
    }),
  );
}

async function copyText(text: string) {
  if (!text) return;
  await writeText(text);
}

async function execute(action: MenuAction) {
  const element = target.value;
  try {
    switch (action) {
      case "undo":
      case "redo":
        if (isEditableElement(element)) {
          element?.focus();
          document.execCommand(action);
        }
        break;
      case "copy":
        await copyText(selectedText());
        break;
      case "cut": {
        const text = selectedText();
        await copyText(text);
        if (isTextInput(element) || element instanceof HTMLTextAreaElement) {
          const { start, end } = controlSelection(element);
          element.setRangeText("", start, end, "end");
          dispatchInput(element, "deleteByCut", null);
        } else if (element?.isContentEditable) {
          element.focus();
          document.execCommand("delete");
        }
        break;
      }
      case "paste": {
        if (!isEditableElement(element)) break;
        const text = await readText();
        if (isTextInput(element) || element instanceof HTMLTextAreaElement) {
          const { start, end } = controlSelection(element);
          element.focus();
          element.setRangeText(text, start, end, "end");
          dispatchInput(element, "insertFromPaste", text);
        } else if (element?.isContentEditable) {
          element.focus();
          document.execCommand("insertText", false, text);
        }
        break;
      }
      case "selectAll":
        if (isTextInput(element) || element instanceof HTMLTextAreaElement) {
          element.focus();
          element.select();
        } else if (element?.isContentEditable) {
          element.focus();
          document.execCommand("selectAll");
        } else {
          const range = document.createRange();
          range.selectNodeContents(document.body);
          const selection = window.getSelection();
          selection?.removeAllRanges();
          selection?.addRange(range);
        }
        break;
    }
  } finally {
    close(true);
  }
}

function focusRelative(direction: 1 | -1) {
  const items = [...(menu.value?.querySelectorAll<HTMLButtonElement>(
    '[role="menuitem"]:not(:disabled)',
  ) ?? [])];
  if (items.length === 0) return;
  const current = items.indexOf(document.activeElement as HTMLButtonElement);
  const next = current < 0
    ? direction > 0
      ? 0
      : items.length - 1
    : (current + direction + items.length) % items.length;
  items[next]?.focus();
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (isBlockedShortcut(event)) {
    event.preventDefault();
    event.stopPropagation();
    return;
  }
  if (!open.value) return;
  if (event.key === "Escape") {
    event.preventDefault();
    close(true);
  } else if (
    (event.key === "Enter" || event.key === " ") &&
    document.activeElement instanceof HTMLButtonElement &&
    menu.value?.contains(document.activeElement)
  ) {
    event.preventDefault();
    document.activeElement.click();
  } else if (event.key === "ArrowDown") {
    event.preventDefault();
    focusRelative(1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    focusRelative(-1);
  }
}

function isBlockedShortcut(event: KeyboardEvent): boolean {
  if (["F5", "F11", "F12"].includes(event.key)) return true;
  if (event.altKey && ["ArrowLeft", "ArrowRight"].includes(event.key)) {
    return true;
  }
  if (!event.ctrlKey) return false;
  const key = event.key.toLowerCase();
  if (["r", "p", "s", "o", "u"].includes(key)) return true;
  if (event.shiftKey && ["r", "i", "j", "c"].includes(key)) return true;
  return ["+", "=", "-", "_", "0"].includes(key);
}

function onDocumentPointerDown(event: PointerEvent) {
  if (
    open.value &&
    event.target instanceof Node &&
    !menu.value?.contains(event.target)
  ) {
    close(false);
  }
}

function onViewportChange() {
  close(true);
}

onMounted(() => {
  document.addEventListener("contextmenu", show, true);
  window.addEventListener("keydown", onDocumentKeydown, true);
  document.addEventListener("pointerdown", onDocumentPointerDown, true);
  window.addEventListener("resize", onViewportChange);
  window.addEventListener("scroll", onViewportChange, true);
  window.addEventListener("blur", onViewportChange);
});

onBeforeUnmount(() => {
  document.removeEventListener("contextmenu", show, true);
  window.removeEventListener("keydown", onDocumentKeydown, true);
  document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  window.removeEventListener("resize", onViewportChange);
  window.removeEventListener("scroll", onViewportChange, true);
  window.removeEventListener("blur", onViewportChange);
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      ref="menu"
      class="app-context-menu"
      role="menu"
      aria-label="应用菜单"
      :style="menuStyle"
      @contextmenu.prevent
    >
      <button
        v-for="item in menuItems"
        :key="item.action"
        type="button"
        role="menuitem"
        :data-action="item.action"
        :disabled="!availability[item.action]"
        @click="execute(item.action)"
      >
        <component
          :is="item.icon"
          class="app-context-menu-icon"
          :size="16"
          :data-icon="item.iconName"
          aria-hidden="true"
        />
        <span>{{ item.label }}</span>
      </button>
    </div>
  </Teleport>
</template>
