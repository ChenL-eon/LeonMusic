<script setup lang="ts">
/** 移动端搜索页：返回 + 圆角搜索框 + 取消、音源 chip、KA 风格结果行。
 *  逻辑与桌面 Search.vue 一致（同一批 IPC），只是布局换成移动版。 */
import { computed, onMounted, ref } from "vue";

import Icon from "@/components/Icon.vue";
import MobileTrackRow from "@/mobile/MobileTrackRow.vue";
import { errorText, plugins as pluginApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";
import type { PluginInfo, Track } from "@/types";

const props = defineProps<{ initialKeyword?: string }>();
const emit = defineEmits<{ (e: "back"): void }>();

const player = usePlayerStore();
const library = useLibraryStore();
const settingsStore = useSettingsStore();
const ui = useUiStore();

const keyword = ref(props.initialKeyword ?? "");
/** 与桌面首页 / 桌面搜索页共享同一份音源状态，并受「记住搜索平台」控制 */
const activePlugin = computed(() => settingsStore.searchSource);
const results = ref<Track[]>([]);
const loading = ref(false);
const page = ref(1);
const inputEl = ref<HTMLInputElement | null>(null);

const enabledPlugins = computed(() =>
  settingsStore.pluginList
    .filter((p) => p.enabled && p.manifest.capabilities.includes("search"))
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

const missing = computed(() => {
  const p = active.value;
  if (!p) return [];
  return p.manifest.config
    .filter((f) => f.required && !(p.config[f.key] ?? "").trim())
    .map((f) => f.label);
});

onMounted(async () => {
  await settingsStore.init();
  settingsStore.initSearchSource(enabledPlugins.value.map((p) => p.manifest.id));
  inputEl.value?.focus();
});

function onSourceChange(id: string) {
  settingsStore.selectSearchSource(id);
}

async function search(reset = true) {
  const q = keyword.value.trim();
  if (!q) return;
  if (!activePlugin.value) {
    ui.toast("还没有可用的音源插件", "error");
    return;
  }
  if (missing.value.length) {
    ui.toast(`请先填写「${active.value?.manifest.name}」的${missing.value.join("、")}`, "error");
    emit("back");
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

async function play(t: Track) {
  const i = results.value.findIndex((x) => x.id === t.id);
  ui.toast("正在解析播放地址…");
  await player.playTracks(results.value, Math.max(0, i));
}

async function fav(t: Track) {
  const existing = library.tracks.find((x) => x.id === t.id);
  if (!existing) {
    ui.toast("在线曲目先播放一次即可收藏", "info");
    return;
  }
  await library.toggleFavorite(t.id);
}

function more(t: Track) {
  void player.addToQueue([t], false);
  ui.toast(`「${t.title}」已加入队列`);
}

async function loadMore() {
  page.value += 1;
  await search(false);
}

function clear() {
  keyword.value = "";
  results.value = [];
  inputEl.value?.focus();
}
</script>

<template>
  <div class="m-page search-page">
    <div class="head">
      <button class="m-icon back" aria-label="返回" @click="emit('back')">
        <Icon name="chevron" :size="22" />
      </button>
      <div class="box m-card">
        <Icon name="search" :size="18" />
        <input
          ref="inputEl"
          v-model="keyword"
          placeholder="搜索歌曲，歌手"
          @keyup.enter="search()"
        />
        <button v-if="keyword" class="clear" aria-label="清空" @click="clear">
          <Icon name="close" :size="16" />
        </button>
      </div>
      <button class="cancel" @click="emit('back')">取消</button>
    </div>

    <div class="chips">
      <button
        v-for="p in enabledPlugins"
        :key="p.manifest.id"
        class="m-chip"
        :class="{ on: activePlugin === p.manifest.id }"
        @click="onSourceChange(p.manifest.id)"
      >
        {{ p.manifest.name.replace(/音乐|（.*）|\(.*\)/g, "").trim() || p.manifest.name }}
      </button>
      <button v-if="!enabledPlugins.length" class="m-chip" @click="emit('back')">
        未启用音源
      </button>
      <button class="go" :disabled="loading || !keyword.trim()" @click="search()">
        {{ loading ? "搜索中…" : "搜索" }}
      </button>
    </div>

    <div v-if="results.length" class="rows">
      <MobileTrackRow
        v-for="(t, i) in results"
        :key="t.id + i"
        :track="t"
        :fav="library.favoriteIds.has(t.id)"
        :current="player.snap.track?.id === t.id"
        @play="play"
        @favorite="fav"
        @more="more"
      />
      <button class="more" :disabled="loading" @click="loadMore">加载更多</button>
    </div>
    <div v-else class="m-empty">
      <Icon name="search" :size="34" />
      <div>{{ keyword.trim() ? (loading ? "搜索中…" : "没有找到结果") : "输入关键词开始搜索" }}</div>
      <button v-if="keyword.trim() && !loading" class="m-chip on" @click="search()">重新搜索</button>
    </div>
  </div>
</template>

<style scoped>
.search-page {
  padding-left: 14px;
  padding-right: 14px;
}
.head {
  display: flex;
  align-items: center;
  gap: 6px;
}
.back {
  width: 36px;
  height: 36px;
}
.back :deep(svg) {
  transform: rotate(180deg);
}
.box {
  display: flex;
  align-items: center;
  gap: 9px;
  flex: 1;
  min-width: 0;
  height: 46px;
  padding: 0 14px;
  color: var(--text-3);
}
.box input {
  flex: 1;
  min-width: 0;
  height: 100%;
  border: none;
  outline: none;
  background: transparent;
  color: var(--text);
  font-size: 16px !important;
}
.clear {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  border-radius: 50%;
  background: var(--panel-3);
  color: var(--text-2);
}
.cancel {
  font-size: 13px;
  color: var(--accent);
  padding: 0 4px;
  white-space: nowrap;
}
.chips {
  display: flex;
  align-items: center;
  gap: 8px;
  overflow-x: auto;
  padding: 16px 2px 4px;
  scrollbar-width: none;
}
.chips::-webkit-scrollbar {
  display: none;
}
.chips .m-chip {
  height: 34px;
  padding: 0 14px;
  font-size: 12.5px;
}
.go {
  height: 34px;
  padding: 0 16px;
  margin-left: auto;
  border-radius: 999px;
  background: var(--accent);
  color: #fff;
  font-size: 12.5px;
  font-weight: 600;
  white-space: nowrap;
}
.go:disabled {
  opacity: 0.5;
}
.rows {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-top: 6px;
}
.more {
  height: 42px;
  margin-top: 8px;
  color: var(--accent);
  font-size: 13px;
}
</style>
