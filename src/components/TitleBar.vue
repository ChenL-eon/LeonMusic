<script setup lang="ts">
/** 无框窗口的自绘标题栏（2026-11-07：与主页面融合，无系统分割线）。
 *  - 整条是拖拽区（data-tauri-drag-region，双击最大化由 Tauri 注入脚本处理）；
 *  - 背景全透明，右侧三个窗口控制按钮，hover 才出现底色；
 *  - fixed 悬浮在所有页面（含播放页覆盖层 z35、队列抽屉 z40）之上。 */
import { onMounted, onUnmounted, ref } from "vue";

import Icon from "@/components/Icon.vue";
import { getCurrentWindow } from "@tauri-apps/api/window";

const appWindow = getCurrentWindow();
const maximized = ref(false);

let unlisten: (() => void) | null = null;

async function syncMaximized() {
  try {
    maximized.value = await appWindow.isMaximized();
  } catch {
    /* 非 Tauri 环境（纯浏览器开发）忽略 */
  }
}

onMounted(async () => {
  void syncMaximized();
  try {
    unlisten = await appWindow.onResized(() => void syncMaximized());
  } catch {
    /* 同上 */
  }
});
onUnmounted(() => unlisten?.());

function minimize() {
  void appWindow.minimize();
}
function toggleMaximize() {
  void appWindow.toggleMaximize();
}
function close() {
  void appWindow.close();
}
</script>

<template>
  <div class="titlebar" data-tauri-drag-region>
    <div class="controls">
      <button class="tbtn" title="最小化" @click="minimize">
        <Icon name="win-min" :size="15" />
      </button>
      <button
        class="tbtn"
        :title="maximized ? '向下还原' : '最大化'"
        @click="toggleMaximize"
      >
        <Icon :name="maximized ? 'win-restore' : 'win-max'" :size="14" />
      </button>
      <button class="tbtn close" title="关闭" @click="close">
        <Icon name="win-close" :size="14" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.titlebar {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  height: var(--titlebar-h);
  z-index: 60;
  display: flex;
  align-items: center;
  justify-content: flex-end;
  /* 柔和的背景延伸：无分割线，只是把页面渐变顶部略略压实，
     列表滚动时内容穿过标题带不至于生硬。
     整条正常接收鼠标事件：Tauri 的 drag-region 注入脚本靠
     mousedown 命中 data-tauri-drag-region 才启动拖拽，
     页面内容已通过 padding-top 补偿到标题带之下，无遮挡冲突。 */
  background: linear-gradient(
    to bottom,
    color-mix(in srgb, var(--bg) 88%, transparent),
    transparent
  );
  /* 播放页沉浸背景（深色专辑图）下底衬会形成浅色带 → 完全透明。
     规则本体在 main.css（scoped 的 :global 组合编译器不支持）。 */
  transition: background 0.25s ease;
}

.controls {
  display: flex;
  height: 100%;
  pointer-events: auto;
  -webkit-app-region: no-drag;
}

.tbtn {
  width: 46px;
  height: 100%;
  display: grid;
  place-items: center;
  color: var(--text-2);
  transition: background 0.12s ease, color 0.12s ease;
}
.tbtn:hover {
  background: color-mix(in srgb, var(--panel-3) 80%, transparent);
  color: var(--text);
}
.tbtn.close:hover {
  background: #e81123;
  color: #fff;
}
</style>
