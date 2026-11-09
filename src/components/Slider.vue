<script setup lang="ts">
/** 通用滑块：鼠标/触摸拖动，拖拽时只在松手后提交 change。 */
import { computed, ref, watch } from "vue";

const props = withDefaults(
  defineProps<{
    modelValue: number;
    min?: number;
    max?: number;
    step?: number;
    disabled?: boolean;
    thin?: boolean;
    /** 竖直模式：轨道竖排、fill 自底向上、按 clientY 取值（音量弹出条用）。 */
    vertical?: boolean;
  }>(),
  { min: 0, max: 1, step: 0.001, disabled: false, thin: false, vertical: false },
);

const emit = defineEmits<{
  (e: "update:modelValue", v: number): void;
  (e: "change", v: number): void;
}>();

const el = ref<HTMLElement | null>(null);
const dragging = ref(false);
const local = ref(props.modelValue);

const pct = computed(() => {
  const span = props.max - props.min;
  if (span <= 0) return 0;
  return Math.max(0, Math.min(1, (local.value - props.min) / span)) * 100;
});

function valueAt(clientX: number, clientY: number): number {
  const node = el.value;
  if (!node) return local.value;
  const rect = node.getBoundingClientRect();
  const ratio = props.vertical
    ? (rect.bottom - clientY) / rect.height
    : (clientX - rect.left) / rect.width;
  const clamped = Math.max(0, Math.min(1, ratio));
  const raw = props.min + clamped * (props.max - props.min);
  const stepped = Math.round(raw / props.step) * props.step;
  return Math.max(props.min, Math.min(props.max, Number(stepped.toFixed(6))));
}

function onDown(e: PointerEvent) {
  if (props.disabled) return;
  dragging.value = true;
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
  local.value = valueAt(e.clientX, e.clientY);
  emit("update:modelValue", local.value);
}

function onMove(e: PointerEvent) {
  if (!dragging.value || props.disabled) return;
  local.value = valueAt(e.clientX, e.clientY);
  emit("update:modelValue", local.value);
}

function onUp() {
  if (!dragging.value) return;
  dragging.value = false;
  emit("change", local.value);
}

// 外部值变化时同步（拖动中不打断用户）
watch(
  () => props.modelValue,
  (v) => {
    if (!dragging.value) local.value = v;
  },
);
</script>

<template>
  <div
    ref="el"
    class="slider"
    :class="{ thin, dragging, disabled, vertical }"
    @pointerdown="onDown"
    @pointermove="onMove"
    @pointerup="onUp"
    @pointercancel="onUp"
  >
    <div class="track">
      <div class="fill" :style="vertical ? { height: pct + '%' } : { width: pct + '%' }" />
      <div
        class="thumb"
        :style="vertical ? { top: 100 - pct + '%' } : { left: pct + '%' }"
      />
    </div>
  </div>
</template>

<style scoped>
.slider {
  position: relative;
  height: 18px;
  display: flex;
  align-items: center;
  cursor: pointer;
  touch-action: none;
  user-select: none;
}
.slider.disabled {
  opacity: 0.5;
  cursor: default;
}
.track {
  position: relative;
  width: 100%;
  height: 4px;
  border-radius: 999px;
  background: var(--border-strong);
  overflow: visible;
}
.thin .track {
  height: 3px;
}
.fill {
  position: absolute;
  inset: 0 auto 0 0;
  border-radius: 999px;
  /* 纯色填充：进度/音量条不再用 accent → accent-2 渐变，
     免得短进度段看起来偏色、也和沉浸式页面不搭 */
  background: var(--accent);
}
.thumb {
  position: absolute;
  top: 50%;
  width: 11px;
  height: 11px;
  border-radius: 50%;
  background: #fff;
  transform: translate(-50%, -50%) scale(0);
  transition: transform 0.15s var(--ease);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.4);
}
.slider:hover .thumb,
.slider.dragging .thumb {
  transform: translate(-50%, -50%) scale(1);
}

/* 竖直模式：轨道竖排，fill 自底向上，thumb 用 top 定位 */
.slider.vertical {
  width: 20px;
  justify-content: center;
}
.vertical .track {
  width: 4px;
  height: 100%;
}
.vertical .fill {
  inset: auto 0 0 0;
  width: 100%;
}
.vertical .thumb {
  left: 50%;
}
</style>
