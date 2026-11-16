<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref } from "vue";
import { Check, ChevronDown } from "@lucide/vue";

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: { value: string; label: string; disabled?: boolean }[];
    ariaLabel?: string;
    disabled?: boolean;
  }>(),
  { ariaLabel: "选择", disabled: false },
);

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

const trigger = ref<HTMLButtonElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const open = ref(false);
const highlighted = ref(-1);
const panelStyle = ref<Record<string, string>>({});

const selectedIndex = computed(() =>
  props.options.findIndex((option) => option.value === props.modelValue),
);
const selectedLabel = computed(
  () => props.options[selectedIndex.value]?.label ?? "请选择",
);

function firstEnabled(from: number, direction: 1 | -1): number {
  if (props.options.length === 0) return -1;
  let index = from;
  for (let count = 0; count < props.options.length; count += 1) {
    if (index < 0) index = props.options.length - 1;
    if (index >= props.options.length) index = 0;
    if (!props.options[index].disabled) return index;
    index += direction;
  }
  return -1;
}

function positionPanel() {
  const rect = trigger.value?.getBoundingClientRect();
  if (!rect) return;
  const estimatedHeight = Math.min(
    props.options.length * 36 + 10,
    Math.min(320, window.innerHeight - 24),
  );
  const openAbove =
    rect.bottom + 6 + estimatedHeight > window.innerHeight - 8 &&
    rect.top - 6 > estimatedHeight;
  panelStyle.value = {
    left: `${Math.max(8, Math.min(rect.left, window.innerWidth - rect.width - 8))}px`,
    top: openAbove
      ? `${Math.max(8, rect.top - estimatedHeight - 6)}px`
      : `${rect.bottom + 6}px`,
    width: `${rect.width}px`,
  };
}

async function show() {
  if (props.disabled) return;
  open.value = true;
  highlighted.value =
    selectedIndex.value >= 0
      ? selectedIndex.value
      : firstEnabled(0, 1);
  positionPanel();
  await nextTick();
  const selected = panel.value?.querySelector<HTMLElement>('[aria-selected="true"]');
  if (selected?.scrollIntoView) {
    selected.scrollIntoView({ block: "nearest" });
  }
}

function close() {
  open.value = false;
  highlighted.value = -1;
}

function choose(index: number) {
  const option = props.options[index];
  if (!option || option.disabled) return;
  emit("update:modelValue", option.value);
  close();
  trigger.value?.focus();
}

function move(direction: 1 | -1) {
  if (!open.value) {
    void show();
    return;
  }
  highlighted.value = firstEnabled(
    highlighted.value + direction,
    direction,
  );
}

function onKeydown(event: KeyboardEvent) {
  switch (event.key) {
    case "ArrowDown":
      event.preventDefault();
      move(1);
      break;
    case "ArrowUp":
      event.preventDefault();
      move(-1);
      break;
    case "Home":
      if (!open.value) return;
      event.preventDefault();
      highlighted.value = firstEnabled(0, 1);
      break;
    case "End":
      if (!open.value) return;
      event.preventDefault();
      highlighted.value = firstEnabled(props.options.length - 1, -1);
      break;
    case "Enter":
    case " ":
      event.preventDefault();
      if (open.value) choose(highlighted.value);
      else void show();
      break;
    case "Escape":
      if (!open.value) return;
      event.preventDefault();
      close();
      break;
    case "Tab":
      close();
      break;
  }
}

function onDocumentPointer(event: PointerEvent) {
  const target = event.target as Node;
  if (!trigger.value?.contains(target) && !panel.value?.contains(target)) {
    close();
  }
}

function onViewportChange() {
  if (open.value) positionPanel();
}

document.addEventListener("pointerdown", onDocumentPointer);
window.addEventListener("resize", onViewportChange);
window.addEventListener("scroll", onViewportChange, true);

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocumentPointer);
  window.removeEventListener("resize", onViewportChange);
  window.removeEventListener("scroll", onViewportChange, true);
});
</script>

<template>
  <div class="select-menu">
    <button
      ref="trigger"
      type="button"
      class="select-trigger"
      :class="{ open, placeholder: selectedIndex < 0 }"
      role="combobox"
      :aria-label="ariaLabel"
      :aria-expanded="open"
      aria-haspopup="listbox"
      :disabled="disabled"
      @click="open ? close() : show()"
      @keydown="onKeydown"
    >
      <span>{{ selectedLabel }}</span>
      <ChevronDown :size="15" aria-hidden="true" />
    </button>
    <Teleport to="body">
      <div
        v-if="open"
        ref="panel"
        class="select-panel"
        role="listbox"
        :aria-label="ariaLabel"
        :style="panelStyle"
        @keydown="onKeydown"
      >
        <button
          v-for="(option, index) in options"
          :key="option.value"
          type="button"
          role="option"
          class="select-option"
          :class="{ highlighted: highlighted === index }"
          :aria-selected="option.value === modelValue"
          :disabled="option.disabled"
          @mouseenter="!option.disabled && (highlighted = index)"
          @click="choose(index)"
        >
          <span>{{ option.label }}</span>
          <Check
            v-if="option.value === modelValue"
            :size="15"
            aria-hidden="true"
          />
        </button>
      </div>
    </Teleport>
  </div>
</template>
