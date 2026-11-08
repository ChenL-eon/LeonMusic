<script setup lang="ts">
/** 主窗口外壳：初始化 + 桌面/移动端分流 + 全局浮层。
 *  桌面「正在播放」和「搜索」都是覆盖层：前者点播放条歌曲打开，
 *  后者从首页搜索框打开；两者都不再是侧栏一级页。 */
import { computed, onMounted, onUnmounted, ref, watch } from "vue";

import QueueDrawer from "@/components/QueueDrawer.vue";
import PlayerBar from "@/components/PlayerBar.vue";
import Sidebar from "@/components/Sidebar.vue";
import TitleBar from "@/components/TitleBar.vue";
import AccountView from "@/views/Account.vue";
import Favorites from "@/views/Favorites.vue";
import HomeView from "@/views/Home.vue";
import Library from "@/views/Library.vue";
import NowPlaying from "@/views/NowPlaying.vue";
import PlaylistView from "@/views/PlaylistView.vue";
import Search from "@/views/Search.vue";
import SettingsView from "@/views/Settings.vue";
import MobileShell from "@/mobile/MobileShell.vue";
import { player as playerApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore, type Page } from "@/stores/ui";

const ui = useUiStore();
const player = usePlayerStore();
const library = useLibraryStore();
const settingsStore = useSettingsStore();

const booting = ref(true);
/** 遮罩内容延迟出现：初始化真的超过 150ms 才显示 spinner。
 *  以前无条件显示，安卓冷启动时必然会明确闪一下这屏。 */
const showBoot = ref(false);
let bootTimer: ReturnType<typeof setTimeout> | undefined;
const bootError = ref("");

const views: Record<Page, unknown> = {
  home: HomeView,
  // 移动端「我的」在桌面没有对应视图：窗口从手机宽度放大时退到首页。
  me: HomeView,
  // `now` 只是覆盖层信号，覆盖层下面衬着回退页，不能空白。
  now: HomeView,
  library: Library,
  playlist: PlaylistView,
  favorites: Favorites,
  search: Search,
  account: AccountView,
  settings: SettingsView,
};

/** 播放详情覆盖层下衬着的页面：进覆盖层前记住从哪来，关闭时回去。 */
const lastPage = ref<Page>("home");
watch(
  () => ui.page,
  (p) => {
    if (p !== "now") lastPage.value = p;
  },
  { immediate: true },
);
const nowOpen = computed(() => ui.page === "now");
const currentView = computed(() => views[ui.page === "now" ? lastPage.value : ui.page] ?? HomeView);
/** 缓存与挂载的 key 用「实际渲染的页」而不是 ui.page：
 *  打开播放覆盖层时 key 不变，衬底的页面不会重挂载。 */
const viewKey = computed(() => (ui.page === "now" ? lastPage.value : ui.page));
function closeNow() {
  ui.go(lastPage.value);
}

/** 与 main.css 的移动端断点保持一致（桌面窗口 minWidth 940，不会误判）。 */
const mobileQuery = window.matchMedia("(max-width: 860px)");
const isMobile = ref(mobileQuery.matches);
function onBreakpoint(e: MediaQueryListEvent) {
  isMobile.value = e.matches;
}
mobileQuery.addEventListener("change", onBreakpoint);
onUnmounted(() => mobileQuery.removeEventListener("change", onBreakpoint));

onMounted(async () => {
  bootTimer = setTimeout(() => (showBoot.value = true), 150);
  try {
    // 首屏只依赖「设置（主题/默认音源）」与「播放器状态（播放条）」。
    await settingsStore.init();
    // 曲库是启动里最重的一步（最多 200 首 Track 要过 IPC），不再挡首屏：
    // 与播放器并行，首页用 library.ready 自己显示骨架，数据到了直接填充。
    library.init().catch((e) => {
      console.error(e);
      ui.toast(`读取曲库失败：${e}`, "error");
    });
    await player.init();
  } catch (e) {
    bootError.value = String(e);
    console.error(e);
  } finally {
    clearTimeout(bootTimer);
    booting.value = false;
  }
});

async function toggleLyricWindow() {
  try {
    await playerApi.toggleLyricWindow();
  } catch {
    ui.toast("桌面悬浮歌词仅在 Windows 上可用", "error");
    ui.lyricWindowOn = false;
  }
}
</script>

<template>
  <div class="app-root">
    <MobileShell v-if="isMobile" :booting="booting" :show-boot="showBoot" :boot-error="bootError" />

    <div v-else class="shell">
      <TitleBar />
      <Sidebar />

      <main class="main">
        <div v-if="booting" class="boot">
          <template v-if="showBoot">
            <div class="spinner" />
            <div class="dim">正在启动…</div>
          </template>
        </div>
        <div v-else-if="bootError" class="boot">
          <div class="empty">
            <div class="big">!</div>
            <div>启动失败</div>
            <div class="dim" style="font-size: 12px">{{ bootError }}</div>
          </div>
        </div>
        <transition v-else name="fade" mode="out-in">
          <KeepAlive>
            <component :is="currentView" :key="viewKey" />
          </KeepAlive>
        </transition>
      </main>

      <div class="player-bar">
        <PlayerBar @toggle-lyric="toggleLyricWindow" />
      </div>

      <!-- 播放详情覆盖层：盖住侧栏与主区，QueueDrawer(40) 仍在它之上 -->
      <transition name="now-pop">
        <div v-if="nowOpen" class="now-overlay">
          <NowPlaying embedded @close="closeNow" />
        </div>
      </transition>
    </div>

    <QueueDrawer />

    <div class="toasts">
      <transition-group name="fade">
        <div v-for="t in ui.toasts" :key="t.id" class="toast" :class="t.kind">
          {{ t.text }}
        </div>
      </transition-group>
    </div>
  </div>
</template>

<style scoped>
.app-root {
  height: 100%;
}

/* 播放详情覆盖层：全屏，进出带轻微缩放。
   背景铺满全窗口（浅色渐变在非沉浸时延续到标题带下；沉浸时被
   NowPlaying 自己的深色专辑背景盖住），内容让位由 .now 自己做。 */
.now-overlay {
  position: fixed;
  inset: 0;
  z-index: 35;
  background: var(--bg);
  background-image: var(--bg-grad);
}
.now-pop-enter-active,
.now-pop-leave-active {
  transition: opacity 0.22s var(--ease, ease), transform 0.22s var(--ease, ease);
}
.now-pop-enter-from,
.now-pop-leave-to {
  opacity: 0;
  transform: scale(1.03) translateY(12px);
}

.boot {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
  font-size: 13px;
}
.spinner {
  width: 30px;
  height: 30px;
  border-radius: 50%;
  border: 2px solid var(--border-strong);
  border-top-color: var(--accent);
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.toasts {
  position: fixed;
  /* fixed 是相对视口定位的，不吃 .shell 的安全区 padding，
     所以这里要自己把底部手势条的高度加回来 */
  bottom: calc(108px + var(--sab));
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  flex-direction: column;
  gap: 8px;
  z-index: 90;
  pointer-events: none;
  align-items: center;
}
.toast {
  padding: 9px 18px;
  border-radius: 999px;
  font-size: 12.5px;
  background: color-mix(in srgb, var(--panel-3) 92%, transparent);
  border: 1px solid var(--border);
  backdrop-filter: blur(18px);
  box-shadow: var(--shadow);
  color: var(--text);
  max-width: 70vw;
  text-align: center;
}
.toast.error {
  color: #ff8f8f;
  border-color: rgba(255, 107, 107, 0.3);
}
.toast.success {
  color: var(--accent-2);
}

@media (max-width: 860px) {
  .toasts {
    /* 移动端只剩一条悬浮胶囊（60px + 底部留白），不再有播放条 + tab 两层 */
    bottom: calc(96px + var(--sab));
    width: max-content;
    max-width: calc(100vw - 48px);
  }
  .toast {
    font-size: 12px;
    padding: 8px 16px;
  }
}
</style>
