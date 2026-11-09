<script setup lang="ts">
/** 无框窗口的自绘标题栏（2026-11-07：与主页面融合，无系统分割线）。
 *  - 整条是拖拽区（data-tauri-drag-region，双击最大化由 Tauri 注入脚本处理）；
 *  - 完全透明，与主界面连成一片：标题栏一旦自带底衬，就会把主界面顶部的
 *    --bg-grad 径向光晕盖成一条纯色带（浅色下白带、深色下黑带）；
 *    右侧三个窗口控制按钮，hover 才出现底色；开自定义背景图时按钮组会
 *    自带一小块玻璃底（规则在 main.css），否则图标压在照片上看不清；
 *  - fixed 悬浮在所有页面（含播放页覆盖层 z35、队列抽屉 z40）之上。
 *
 * 2026-11-09：根节点改 pointer-events:none，事件只由 .dragbar / .controls 接；
 * 播放页打开时 .dragbar 左侧让出 64px，否则 z60 的标题带会把播放页左上角
 * 「收起」按钮的上半截吞掉（点下去变成拖窗口）。 */
import { computed, onMounted, onUnmounted, ref } from "vue";

import Icon from "@/components/Icon.vue";
import { useUiStore } from "@/stores/ui";
import { getCurrentWindow } from "@tauri-apps/api/window";

const ui = useUiStore();
const overNow = computed(() => ui.page === "now");

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
  <div class="titlebar" :class="{ 'over-now': overNow }">
    <div class="dragbar" data-tauri-drag-region />
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
  /* 根节点不接事件：样式照画，鼠标交给 .dragbar（拖拽）与 .controls（按钮），
     这样播放页可以在标题带左上角挖一个洞把「收起」按钮露出来。 */
  pointer-events: none;
  /* 不画任何底色 —— 标题栏要像「没有标题栏」一样和主界面连成一片。
     这里原先是一条 --bg 88% → transparent 的渐变，本意是给纯色界面做
     「内容穿过标题带」的柔和过渡；但它只含 --bg 纯色、不含 --bg-grad 的
     径向光晕，等于把主界面顶部那片光晕盖掉了：浅色下多一条白带、深色下
     多一条黑带。而内容其实穿不过去（.sidebar / .main 的 padding-top 里
     都含 --titlebar-h），所以它只需要是透明的。
     鼠标事件由下面的 .dragbar 单独承接（Tauri 的 drag-region 注入脚本
     靠 mousedown 命中 data-tauri-drag-region 才启动拖拽）。 */
  background: transparent;
}

/* 真正的拖拽命中层：整条标题带 */
.dragbar {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: auto;
}
/* 播放页：左上角 64px（收起按钮 + 外扩命中区）留给页面，别在这里拖窗口 */
.titlebar.over-now .dragbar {
  left: 64px;
}

.controls {
  position: relative;
  z-index: 1;
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
