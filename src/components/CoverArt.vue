<script setup lang="ts">
/**
 * 封面。
 *
 * 本地封面来自 Rust 生成的 `covers/*.jpg`，通过 `asset://` 协议加载
 * （见 tauri.conf.json 的 csp: img-src asset:）。
 */
import { computed, ref, watch } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";

const props = withDefaults(
  defineProps<{
    coverPath?: string | null;
    coverUrl?: string | null;
    size?: string;
    radius?: string;
    spinning?: boolean;
  }>(),
  { size: "100%", radius: "var(--radius-l)", spinning: false },
);

const failed = ref(false);

const src = computed(() => {
  if (props.coverPath) {
    try {
      return convertFileSrc(props.coverPath);
    } catch {
      /* 某些环境下 asset 协议不可用，退回 http */
    }
  }
  return props.coverUrl ?? "";
});

watch(src, () => {
  failed.value = false;
});
</script>

<template>
  <div
    class="cover"
    :style="{ width: size, height: size, borderRadius: radius }"
    :class="{ spinning }"
  >
    <img v-if="src && !failed" :src="src" @error="failed = true" alt="" />
    <div v-else class="fallback">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.2">
        <path d="M9 18V6l10-2v12" stroke-linecap="round" stroke-linejoin="round" />
        <circle cx="6" cy="18" r="3" />
        <circle cx="16" cy="16" r="3" />
      </svg>
    </div>
  </div>
</template>

<style scoped>
.cover {
  position: relative;
  overflow: hidden;
  background: linear-gradient(135deg, var(--panel-3), var(--panel-2));
  flex: none;
  box-shadow: var(--cover-shadow);
}
.cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.fallback {
  width: 100%;
  height: 100%;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.fallback svg {
  width: 38%;
  height: 38%;
}
.spinning {
  border-radius: 50% !important;
  animation: spin 22s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
