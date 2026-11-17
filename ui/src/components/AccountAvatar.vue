<script setup lang="ts">
import { CircleUserRound } from "@lucide/vue";
import { ref, watch } from "vue";

const props = withDefaults(
  defineProps<{
    avatarUrl?: string | null;
    size?: number;
  }>(),
  {
    avatarUrl: null,
    size: 20,
  },
);

const failed = ref(false);

watch(
  () => props.avatarUrl,
  () => {
    failed.value = false;
  },
);
</script>

<template>
  <img
    v-if="avatarUrl && !failed"
    class="account-avatar-image"
    :src="avatarUrl"
    alt=""
    aria-hidden="true"
    decoding="async"
    loading="lazy"
    referrerpolicy="no-referrer"
    @error="failed = true"
  />
  <CircleUserRound v-else :size="size" aria-hidden="true" />
</template>
