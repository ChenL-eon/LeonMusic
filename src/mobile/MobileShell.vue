<script setup lang="ts">
/** 移动端外壳：一级页 + 二级页 + 全屏播放覆盖层 + 悬浮胶囊导航。
 *  桌面完全不走这里（App.vue 按 860px 断点分流），改动不会波及侧栏布局。 */
import { computed, onMounted, ref, watch, type Component } from "vue";

import Icon from "@/components/Icon.vue";
import AccountView from "@/views/Account.vue";
import Library from "@/views/Library.vue";
import PlaylistView from "@/views/PlaylistView.vue";
import SettingsView from "@/views/Settings.vue";
import MobileHome from "@/mobile/MobileHome.vue";
import MobileListPage from "@/mobile/MobileListPage.vue";
import MobileMe from "@/mobile/MobileMe.vue";
import MobileNav from "@/mobile/MobileNav.vue";
import MobilePlayer from "@/mobile/MobilePlayer.vue";
import MobileSearch from "@/mobile/MobileSearch.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore, type Page } from "@/stores/ui";
import "@/mobile/mobile.css";

defineProps<{ booting: boolean; bootError: string }>();

type MobileTab = "home" | "search" | "favorites" | "me";
type ListKind = "recent" | "cached";

const ui = useUiStore();
const library = useLibraryStore();
const player = usePlayerStore();

const TABS: MobileTab[] = ["home", "search", "favorites", "me"];
const SUB_PAGES: Page[] = ["library", "playlist", "account", "settings"];
const SUB_TITLES: Record<string, string> = {
  library: "本地音乐",
  playlist: "歌单",
  account: "账号与登录",
  settings: "设置",
  recent: "最近播放",
  cached: "已缓存",
};
const SUB_VIEWS: Partial<Record<Page, Component>> = {
  library: Library,
  playlist: PlaylistView,
  account: AccountView,
  settings: SettingsView,
};

const lastTab = ref<MobileTab>("home");
const listKind = ref<ListKind | null>(null);
/** 「我喜欢」不再是底栏 tab，记下从哪个分区进的，返回时回到原处。 */
const favoritesReturn = ref<MobileTab>("home");
const playerOpen = ref(false);
const searchKeyword = ref("");
const searchKey = ref(0);

/** 应用内导航栈：记录每个「前进」动作发生时的页面快照，
 *  返回（返回按钮 / 安卓侧滑手势）逐级弹出，实现「回到上一个页面」。 */
type NavEntry = { page: Page; list: ListKind | null; searchKey?: number; searchKeyword?: string };
const navStack = ref<NavEntry[]>([]);

function pushNav(extra?: Partial<NavEntry>) {
  navStack.value.push({ page: ui.page, list: listKind.value, ...extra });
}

/** 安卓系统返回（全面屏侧滑 / 返回键）由 WryActivity 转发给 WebView 的
 *  history.goBack()；SPA 没有历史条目时它会直接结束 Activity（现象=回到桌面）。
 *  预压一条占位历史，popstate 里改走应用内返回，再补一条占位——
 *  返回手势永远只消费 WebView 历史，应用本身永不退出。 */
function onSysBack() {
  if (playerOpen.value) {
    playerOpen.value = false;
  } else if (isSub.value) {
    back();
  }
  // 一级页：吞掉返回（不退出应用）
  history.pushState({ mguard: true }, "");
}

/** ui store 的默认页是桌面的 'now'。初始化阶段碰到它只回首页，
 *  不能当成「运行期跳转播放页」，否则移动端一启动就弹出全屏播放层。 */
let bootstrapped = false;
onMounted(() => {
  if (!TABS.includes(ui.page as MobileTab) && !SUB_PAGES.includes(ui.page)) {
    ui.go("home");
  }
  bootstrapped = true;
  history.pushState({ mguard: true }, "");
  window.addEventListener("popstate", onSysBack);
});

/** 桌面视图里少数地方会调 ui.go('now')（播放后跳播放页）。
 *  移动端没有「正在播放」这个一级页，改成打开全屏覆盖层并留在当前 tab。 */
watch(
  () => ui.page,
  (p) => {
    if (p === "now") {
      if (!bootstrapped) {
        ui.go("home");
        return;
      }
      playerOpen.value = true;
      listKind.value = null;
      ui.go(lastTab.value);
      return;
    }
    if (TABS.includes(p as MobileTab)) {
      lastTab.value = p as MobileTab;
      listKind.value = null;
    } else if (SUB_PAGES.includes(p)) {
      listKind.value = null;
    }
  },
  { immediate: true },
);

const primaryTab = computed<MobileTab>(() =>
  TABS.includes(ui.page as MobileTab) ? (ui.page as MobileTab) : lastTab.value,
);
/** 搜索 / 我喜欢已不在底栏：进入时仍高亮所属分区，避免底栏失去选中态。 */
const navActive = computed<MobileTab>(() => {
  const p = primaryTab.value;
  if (p === "search") return "home";
  if (p === "favorites") return favoritesReturn.value;
  return p;
});
const isSub = computed(() => listKind.value !== null || SUB_PAGES.includes(ui.page));
const subTitle = computed(() => {
  if (listKind.value) return SUB_TITLES[listKind.value];
  if (ui.page === "playlist") return library.activePlaylist?.name ?? "歌单";
  return SUB_TITLES[ui.page] ?? "";
});
const listTracks = computed(() =>
  listKind.value === "recent"
    ? library.recent
    : library.tracks.filter((t) => t.source.kind === "plugin"),
);

function openSearch(keyword?: string) {
  pushNav({ searchKey: searchKey.value, searchKeyword: searchKeyword.value });
  searchKeyword.value = keyword ?? "";
  searchKey.value += 1;
  ui.go("search");
}

function openSub(page: Page) {
  pushNav();
  listKind.value = null;
  ui.go(page);
}

function openList(kind: ListKind) {
  pushNav();
  listKind.value = kind;
}

function back() {
  const prev = navStack.value.pop();
  if (prev) {
    listKind.value = prev.list;
    if (prev.page === "search") {
      // 恢复进入搜索页前的关键字与挂载 key（key 变化强制 MobileSearch 重建还原）
      searchKeyword.value = prev.searchKeyword ?? "";
      searchKey.value = prev.searchKey ?? searchKey.value;
    }
    if (ui.page !== prev.page) ui.go(prev.page);
  } else {
    // 没有栈记录的兜底：回当前分区一级页
    listKind.value = null;
    ui.go(lastTab.value);
  }
}

function navigate(tab: MobileTab, record = true) {
  if (tab === "favorites") {
    favoritesReturn.value = primaryTab.value === "me" ? "me" : "home";
  }
  if (record) pushNav();
  listKind.value = null;
  ui.go(tab);
}
</script>

<template>
  <div class="mobile-shell">
    <div v-if="booting" class="boot">
      <div class="spinner" />
      <div class="m-subtle">正在唤醒音频引擎…</div>
    </div>
    <div v-else-if="bootError" class="boot">
      <div class="m-empty">
        <div>启动失败</div>
        <div style="font-size: 12px">{{ bootError }}</div>
      </div>
    </div>

    <template v-else>
      <!-- 一级页：二级页/播放页覆盖在它上面，返回时状态还在。
           KeepAlive 缓存已访问的 tab：切回不重挂载、不闪加载；
           search 的 key 每次进入都变，靠 max 做 LRU 淘汰防堆积。 -->
      <transition name="m-fade" mode="out-in">
        <KeepAlive :max="4">
          <MobileHome
            v-if="primaryTab === 'home'"
            key="home"
            @open-search="openSearch"
            @open-settings="openSub('settings')"
          />
          <MobileSearch
            v-else-if="primaryTab === 'search'"
            :key="'search' + searchKey"
            :initial-keyword="searchKeyword"
            @back="back"
          />
          <MobileListPage
            v-else-if="primaryTab === 'favorites'"
            key="favorites"
            title="我喜欢的"
            :tracks="library.favorites"
            empty-text="还没有喜欢的歌曲"
            @back="back"
          />
          <MobileMe
            v-else
            key="me"
            @navigate="navigate"
            @open-sub="openSub"
            @open-list="openList"
          />
        </KeepAlive>
      </transition>

      <!-- 二级页：本地音乐 / 歌单 / 账号 / 设置 / 最近 / 已缓存 -->
      <transition name="m-slide">
        <div v-if="isSub" class="m-sub">
          <div class="m-sub-head">
            <button class="m-icon" aria-label="返回" @click="back">
              <Icon name="chevron" :size="22" class="back-chev" />
            </button>
            <b>{{ subTitle }}</b>
          </div>
          <div class="m-sub-body">
            <KeepAlive :max="4">
              <MobileListPage
                v-if="listKind"
                embedded
                :title="subTitle"
                :tracks="listTracks"
                @back="back"
              />
              <component :is="SUB_VIEWS[ui.page]" v-else :key="ui.page" />
            </KeepAlive>
          </div>
        </div>
      </transition>

      <transition name="m-slide-up">
        <MobilePlayer v-if="playerOpen" @close="playerOpen = false" />
      </transition>

      <MobileNav
        :active="navActive"
        :progress="player.progress"
        :cover-path="player.snap.track?.coverPath"
        :cover-url="player.snap.track?.coverUrl"
        :is-playing="player.isPlaying"
        @navigate="navigate"
        @open-player="playerOpen = true"
      />
    </template>
  </div>
</template>

<style scoped>
.boot {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
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
.m-sub {
  z-index: 30;
  background: var(--bg);
  background-image: var(--bg-grad);
}
.m-sub-head b {
  font-size: 16px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.back-chev {
  transform: rotate(180deg);
}
.m-slide-enter-active,
.m-slide-leave-active,
.m-slide-up-enter-active,
.m-slide-up-leave-active {
  transition: transform 0.26s var(--m-ease), opacity 0.26s var(--m-ease);
}
.m-slide-enter-from,
.m-slide-leave-to {
  transform: translateX(100%);
  opacity: 0.6;
}
.m-slide-up-enter-from,
.m-slide-up-leave-to {
  transform: translateY(100%);
}
</style>
