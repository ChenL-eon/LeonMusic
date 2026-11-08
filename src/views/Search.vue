<script setup lang="ts">
/** 在线音源搜索（走插件）。桌面首页已内置内联搜索，这里是兜底视图
 *  （views 映射里 `search` 键仍指向它）；移动端用独立的 MobileSearch。 */
import { computed, onMounted, ref } from "vue";

import Icon from "@/components/Icon.vue";
import TrackList from "@/components/TrackList.vue";
import { plugins as pluginApi } from "@/api/ipc";
import { errorText } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";
import type { PluginInfo, Track } from "@/types";

const player = usePlayerStore();
const library = useLibraryStore();
const settingsStore = useSettingsStore();
const ui = useUiStore();

const keyword = ref("");
const activePlugin = ref("");
const results = ref<Track[]>([]);
const loading = ref(false);
const page = ref(1);

const enabledPlugins = computed(() =>
  settingsStore.pluginList
    .filter((p) => p.enabled && p.manifest.capabilities.includes("search"))
    // 已配置好必填项的排前面：否则默认选中的往往是字母序第一个（Emby），
    // 用户点搜索只会收到「未配置服务器地址」。
    // 都没配时把网易云放最前——它是唯一开箱即用的公共音源，
    // 自建服务（Navidrome/Jellyfin/Emby）都应该由用户主动选。
    .sort(
      (a, b) =>
        Number(isConfigured(b)) - Number(isConfigured(a)) ||
        Number(b.manifest.id === "netease") - Number(a.manifest.id === "netease") ||
        a.manifest.name.localeCompare(b.manifest.name),
    ),
);

function isConfigured(p: PluginInfo): boolean {
  return p.manifest.config
    .filter((f) => f.required)
    .every((f) => (p.config[f.key] ?? "").trim().length > 0);
}

const active = computed(
  () => enabledPlugins.value.find((p) => p.manifest.id === activePlugin.value) ?? null,
);

/**
 * 当前音源还缺哪些必填项。
 *
 * 空着就直接搜不会「返回空结果」，而是插件里拼 URL 时抛错、或在
 * HTTP 层拿 404——报错信息对用户没有可操作性。所以在点搜索之前就拦住，
 * 并把人送到设置页对应的输入框。
 * （内置的网易云没有必填项，这里恒为空，不会拦。）
 */
const missing = computed(() => {
  const p = active.value;
  if (!p) return [];
  return p.manifest.config
    .filter((f) => f.required && !(p.config[f.key] ?? "").trim())
    .map((f) => f.label);
});

onMounted(async () => {
  await settingsStore.init();
  if (!activePlugin.value && enabledPlugins.value.length) {
    activePlugin.value = enabledPlugins.value[0].manifest.id;
  }
});

async function search(reset = true) {
  const q = keyword.value.trim();
  if (!q) return;
  if (!activePlugin.value) {
    ui.toast("还没有可用的音源插件", "error");
    return;
  }
  if (missing.value.length) {
    ui.toast(`请先填写「${active.value?.manifest.name}」的${missing.value.join("、")}`, "error");
    ui.goSettings("source");
    return;
  }
  if (reset) page.value = 1;
  loading.value = true;
  try {
    const items = await pluginApi.search(activePlugin.value, q, page.value, 40);
    results.value = reset ? items : [...results.value, ...items];
    if (!items.length && reset) ui.toast("没有找到结果", "info");
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    loading.value = false;
  }
}

async function loadMore() {
  page.value += 1;
  await search(false);
}

async function play(_t: Track, i: number) {
  ui.toast("正在解析播放地址…");
  // 搜索结果还没入库，走对象路径；id 查询只对本地库有效
  await player.playTracks(results.value, i);
}

function addQueue(t: Track) {
  void player.addToQueue([t], false);
  ui.toast("已加入队列");
}

async function fav(t: Track) {
  // 在线曲目需要先入库，收藏才有意义
  const existing = library.tracks.find((x) => x.id === t.id);
  if (!existing) {
    ui.toast("在线曲目先播放一次即可收藏", "info");
    return;
  }
  await library.toggleFavorite(t.id);
}
</script>

<template>
  <div class="page">
    <header class="head">
      <div>
        <h1 class="page-title">在线搜索</h1>
        <p class="page-sub">通过音源插件搜索，播放时自动缓存到本地</p>
      </div>
    </header>

    <div class="bar">
      <select v-model="activePlugin" class="select">
        <option v-for="p in enabledPlugins" :key="p.manifest.id" :value="p.manifest.id">
          {{ p.manifest.name }}
        </option>
        <option v-if="!enabledPlugins.length" value="">未启用插件</option>
      </select>
      <div class="search">
        <Icon name="search" :size="17" />
        <input
          v-model="keyword"
          placeholder="歌名 / 艺术家 / 专辑"
          @keyup.enter="search()"
        />
      </div>
      <button class="btn primary" :disabled="loading || !activePlugin" @click="search()">
        {{ loading ? "搜索中…" : "搜索" }}
      </button>
    </div>

    <div v-if="!enabledPlugins.length" class="empty">
      <div class="big">🔌</div>
      <div>还没有可用的音源插件</div>
      <div class="dim" style="font-size: 12px">
        去「设置 → 音源插件」填写 API 地址并启用
      </div>
    </div>

    <div v-else-if="active && missing.length" class="notice">
      <Icon name="plug" :size="16" />
      <div class="notice-text">
        <b>{{ active.manifest.name }}</b> 还缺 <b>{{ missing.join("、") }}</b
        >，现在搜索拿不到结果
      </div>
      <button class="btn ghost" @click="ui.goSettings('source')">去配置</button>
    </div>

    <div class="body scroll">
      <TrackList
        :tracks="results"
        :current-id="player.snap.track?.id"
        :favorite-ids="library.favoriteIds"
        empty-text="输入关键词开始搜索"
        @play="play"
        @favorite="fav"
        @add-queue="addQueue"
        @add-playlist="addQueue"
        @remove="() => {}"
      />
      <div v-if="results.length" class="more">
        <button class="btn ghost" :disabled="loading" @click="loadMore">加载更多</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.head {
  padding-bottom: 4px;
}
.bar {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 8px 0 14px;
  flex-wrap: wrap;
}
.select {
  height: 40px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  padding: 0 14px;
  font-size: 12.5px;
  outline: none;
}
.search {
  flex: 1;
  min-width: 200px;
  display: flex;
  align-items: center;
  gap: 10px;
  height: 40px;
  padding: 0 16px;
  border-radius: 999px;
  background: var(--panel);
  border: 1px solid var(--border);
  color: var(--text-3);
}
.search input {
  flex: 1;
  border: none;
  background: none;
  outline: none;
  color: var(--text);
  font-size: 14px;
}
.body {
  flex: 1;
  min-height: 0;
}
.more {
  display: flex;
  justify-content: center;
  padding: 4px 0 28px;
}

/* 音源未配置：把「为什么搜不到」写在页面上，并给一条直达设置的路 */
.notice {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px 10px 14px;
  margin-bottom: 12px;
  border-radius: var(--radius-m);
  border: 1px solid color-mix(in srgb, var(--accent) 32%, transparent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  color: var(--text-2);
  font-size: 12.5px;
  line-height: 1.5;
}
.notice > svg {
  color: var(--accent);
}
.notice-text {
  flex: 1;
  min-width: 0;
}
.notice-text b {
  color: var(--text);
  font-weight: 600;
}
.notice .btn {
  flex: none;
  height: 32px;
  padding: 0 12px;
  font-size: 12.5px;
}

/*
 * 竖屏手机：把「选择音源 / 搜索框 / 搜索按钮」从一行挤三样
 * 改成「搜索框独占一行 + 第二行音源与按钮」。
 * 用 order 而不是改 DOM：桌面上的阅读顺序（先选音源再输入）仍然成立。
 */
@media (max-width: 860px) {
  .head {
    padding-bottom: 2px;
  }
  .bar {
    flex-wrap: wrap;
    gap: 8px;
    padding: 6px 0 12px;
  }
  .search {
    order: 1;
    flex: 1 1 100%;
    min-width: 0;
    height: 46px;
  }
  .select {
    order: 2;
    flex: 1 1 0;
    min-width: 0;
    height: 44px;
  }
  .bar .btn {
    order: 3;
    flex: 0 0 auto;
    height: 44px;
    padding: 0 22px;
  }
  .more {
    padding-bottom: 16px;
  }
  /* 手机上按钮独占一行，否则提示文字会被压成两三行窄条 */
  .notice {
    flex-wrap: wrap;
    align-items: flex-start;
  }
  .notice .btn {
    width: 100%;
    height: 38px;
    border: 1px solid color-mix(in srgb, var(--accent) 42%, transparent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
}

/* 横屏：搜索框与音源、按钮回到同一行（高度紧张，宽度富余） */
@media (max-width: 900px) and (max-height: 520px) {
  .bar {
    flex-wrap: nowrap;
    gap: 8px;
    padding: 2px 0 8px;
  }
  .search {
    flex: 1 1 auto;
    min-width: 0;
    height: 40px;
  }
  .select {
    flex: 0 0 150px;
    height: 40px;
  }
  .bar .btn {
    flex: 0 0 auto;
    height: 40px;
    padding: 0 18px;
  }
}
</style>
