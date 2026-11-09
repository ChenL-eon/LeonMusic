<script setup lang="ts">
/**
 * 搜索音源选择器。
 *
 * 原生 `<select>` 的弹出层由系统绘制 —— Windows WebView2 上是一块灰色
 * 方角列表，CSS 完全改不动，和 App 的胶囊 / 圆角卡片风格割裂。这里换成
 * 应用内弹层：主题变量配色、当前项打勾、点击外部 / Esc 关闭、方向键与
 * 回车可操作。桌面首页（inline）与独立搜索页（pill）共用同一个组件。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import Icon from "@/components/Icon.vue";

interface SourceOption {
  id: string;
  name: string;
}

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: SourceOption[];
    /** inline = 搜索框内的紧凑标签；pill = 独立搜索栏里的胶囊按钮 */
    variant?: "inline" | "pill";
    /** 菜单相对触发按钮的对齐方向（搜索框内靠右，独立页靠左） */
    align?: "left" | "right";
    placeholder?: string;
    disabled?: boolean;
  }>(),
  {
    variant: "pill",
    align: "left",
    placeholder: "选择音源",
    disabled: false,
  },
);

const emit = defineEmits<{
  (e: "update:modelValue", id: string): void;
  (e: "change", id: string): void;
}>();

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);

const current = computed(() => props.options.find((o) => o.id === props.modelValue) ?? null);
const label = computed(() => current.value?.name ?? props.placeholder);

function menuItems(): HTMLButtonElement[] {
  return Array.from(root.value?.querySelectorAll<HTMLButtonElement>(".ss-item") ?? []);
}

/** 打开后把焦点放到当前选中项，键盘用户不用从头按方向键。 */
async function focusCurrent() {
  await nextTick();
  const items = menuItems();
  const idx = Math.max(
    0,
    props.options.findIndex((o) => o.id === props.modelValue),
  );
  items[idx]?.focus();
}

function toggle() {
  if (props.disabled) return;
  open.value = !open.value;
  if (open.value) void focusCurrent();
}

function openMenu() {
  if (props.disabled || open.value) return;
  open.value = true;
  void focusCurrent();
}

function close(backToTrigger = false) {
  if (!open.value) return;
  open.value = false;
  if (backToTrigger) trigger.value?.focus();
}

function pick(id: string) {
  close();
  if (id !== props.modelValue) {
    emit("update:modelValue", id);
    emit("change", id);
  }
  trigger.value?.focus();
}

function onMenuKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    close(true);
    return;
  }
  if (e.key === "Tab") {
    close();
    return;
  }
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  e.preventDefault();
  const items = menuItems();
  if (!items.length) return;
  const at = items.indexOf(document.activeElement as HTMLButtonElement);
  const step = e.key === "ArrowDown" ? 1 : -1;
  items[(at + step + items.length) % items.length]?.focus();
}

function onPointerDown(e: PointerEvent) {
  if (!root.value?.contains(e.target as Node)) close();
}

onMounted(() => document.addEventListener("pointerdown", onPointerDown, true));
onBeforeUnmount(() => document.removeEventListener("pointerdown", onPointerDown, true));
</script>

<template>
  <div
    ref="root"
    class="ss-root"
    :class="[`ss-${variant}`, `ss-align-${align}`]"
  >
    <button
      ref="trigger"
      type="button"
      class="ss-trigger"
      :disabled="disabled || !options.length"
      :aria-expanded="open"
      aria-haspopup="listbox"
      aria-label="搜索音源"
      @click="toggle"
      @keydown.down.prevent="openMenu"
      @keydown.up.prevent="openMenu"
      @keydown.esc.prevent="close(true)"
    >
      <span class="ss-label">{{ label }}</span>
      <Icon name="chevron-down" :size="14" class="ss-chev" :class="{ flip: open }" />
    </button>

    <Transition name="ss-pop">
      <div
        v-if="open"
        class="ss-menu"
        role="listbox"
        aria-label="搜索音源"
        @keydown="onMenuKey"
      >
        <button
          v-for="o in options"
          :key="o.id"
          type="button"
          class="ss-item"
          :class="{ on: o.id === modelValue }"
          role="option"
          :aria-selected="o.id === modelValue"
          @click="pick(o.id)"
        >
          <i class="ss-dot" />
          <span class="ss-name">{{ o.name }}</span>
          <svg
            v-if="o.id === modelValue"
            class="ss-check"
            width="14"
            height="14"
            viewBox="0 0 16 16"
            fill="none"
            aria-hidden="true"
          >
            <path
              d="M3 8.5l3.2 3.2L13 5"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
        <div v-if="!options.length" class="ss-empty">没有可用的音源</div>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.ss-root {
  position: relative;
  flex: none;
}

/* 触发器：与搜索框同色系，打开时用主色描边 */
.ss-trigger {
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: 7px;
  height: 40px;
  min-width: 116px;
  max-width: 190px;
  padding: 0 12px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  font-size: 12.5px;
  white-space: nowrap;
  cursor: pointer;
  transition:
    color 0.15s,
    border-color 0.15s,
    background 0.15s,
    box-shadow 0.15s;
}
.ss-trigger:hover:not(:disabled) {
  color: var(--text);
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
}
.ss-trigger[aria-expanded="true"] {
  color: var(--accent);
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 9%, var(--panel));
}
.ss-trigger:focus-visible {
  outline: none;
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent);
}
.ss-trigger:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}
.ss-label {
  overflow: hidden;
  text-overflow: ellipsis;
}
.ss-chev {
  flex: none;
  color: var(--text-3);
  transition: transform 0.18s var(--ease), color 0.15s;
}
.ss-chev.flip {
  transform: rotate(180deg);
  color: var(--accent);
}

/* 首页搜索框内的紧凑版：淡主色小标签，不再是一块灰方块 */
.ss-inline .ss-trigger {
  height: 27px;
  min-width: 0;
  max-width: 132px;
  padding: 0 8px 0 11px;
  gap: 4px;
  font-size: 11px;
  border-color: transparent;
  background: color-mix(in srgb, var(--accent) 11%, transparent);
  color: var(--text-2);
}
.ss-inline .ss-trigger:hover:not(:disabled) {
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  border-color: transparent;
}
.ss-inline .ss-chev {
  color: inherit;
  opacity: 0.75;
}

/* 弹层卡片 */
.ss-menu {
  position: absolute;
  top: calc(100% + 9px);
  left: 0;
  min-width: 168px;
  width: max-content;
  max-width: 260px;
  padding: 6px;
  border-radius: 14px;
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: 0 14px 36px rgba(0, 0, 0, 0.24);
  z-index: 80;
  transform-origin: top left;
}
.ss-align-right .ss-menu {
  left: auto;
  right: 0;
  transform-origin: top right;
}
.ss-item {
  display: flex;
  align-items: center;
  gap: 9px;
  width: 100%;
  padding: 9px 10px;
  border-radius: 9px;
  text-align: left;
  font-size: 12.5px;
  color: var(--text-2);
  white-space: nowrap;
  transition: background 0.12s, color 0.12s;
}
.ss-item:hover,
.ss-item:focus-visible {
  outline: none;
  background: var(--hover);
  color: var(--text);
}
.ss-item.on {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  font-weight: 600;
}
.ss-dot {
  width: 6px;
  height: 6px;
  flex: none;
  border-radius: 50%;
  background: var(--border-strong);
}
.ss-item.on .ss-dot {
  background: var(--accent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 16%, transparent);
}
.ss-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
}
.ss-check {
  flex: none;
  color: var(--accent);
}
.ss-empty {
  padding: 10px;
  font-size: 12px;
  color: var(--text-3);
}

.ss-pop-enter-active,
.ss-pop-leave-active {
  transition: opacity 0.16s var(--ease), transform 0.16s var(--ease);
}
.ss-pop-enter-from,
.ss-pop-leave-to {
  opacity: 0;
  transform: translateY(-6px) scale(0.96);
}
</style>
