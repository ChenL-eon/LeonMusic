<script setup lang="ts">
/** 本地媒体库。 */
import { computed, onMounted, ref } from "vue";
import { open } from "@tauri-apps/plugin-dialog";

import Icon from "@/components/Icon.vue";
import TrackList from "@/components/TrackList.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { Track } from "@/types";

const library = useLibraryStore();
const player = usePlayerStore();
const ui = useUiStore();

const keyword = ref("");
const sortBy = ref<"title" | "artist" | "album" | "added">("artist");

onMounted(() => {
  void library.init();
});

const sorted = computed(() => {
  const list = library.tracks.filter((t) => {
    const q = keyword.value.trim().toLowerCase();
    if (!q) return true;
    return (
      t.title.toLowerCase().includes(q) ||
      t.artist.toLowerCase().includes(q) ||
      t.album.toLowerCase().includes(q)
    );
  });
  const key = sortBy.value;
  return [...list].sort((a, b) => {
    if (key === "added") return b.addedAt - a.addedAt;
    return String(a[key] || "").localeCompare(String(b[key] || ""), "zh-Hans-CN");
  });
});

async function pickFolder() {
  const selected = await open({ directory: true, multiple: true, title: "选择音乐文件夹" });
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  ui.toast(`开始扫描 ${paths.length} 个目录…`);
  const summary = await library.scan(paths);
  if (summary) {
    ui.toast(
      `扫描完成：新增 ${summary.added}，更新 ${summary.updated}，失败 ${summary.failed}`,
      summary.failed ? "error" : "success",
    );
  }
}

function play(t: Track, i: number) {
  void player.playList(sorted.value, i);
}

async function fav(t: Track) {
  const on = await library.toggleFavorite(t.id);
  ui.toast(on ? "已喜欢" : "已取消", "success");
}

function addQueue(t: Track) {
  void player.addToQueue([t], false);
  ui.toast("已加入队列");
}

function addPlaylist(t: Track) {
  const target = library.playlists.find((p) => !p.system);
  if (!target) {
    void library.createPlaylist("我的歌单").then((p) => {
      void library.addToPlaylist(p.id, [t.id]);
      ui.toast(`已加入「${p.name}」`);
    });
    return;
  }
  void library.addToPlaylist(target.id, [t.id]);
  ui.toast(`已加入「${target.name}」`);
}

function remove(t: Track) {
  void import("@/api/ipc").then((m) => m.library.remove(t.id)).then(() => library.refresh());
}
</script>

<template>
  <div class="page">
    <header class="head">
      <div>
        <h1 class="page-title">本地音乐</h1>
        <p class="page-sub">
          共 {{ library.tracks.length }} 首本地曲目<template v-if="library.scanning">
            · 正在扫描 {{ library.scanProgress.done }}/{{ library.scanProgress.total }}</template
          >
        </p>
      </div>

      <div class="tools">
        <div class="search-mini">
          <Icon name="search" :size="16" />
          <input v-model="keyword" placeholder="筛选标题、艺术家、专辑" />
        </div>
        <select v-model="sortBy" class="select">
          <option value="artist">按艺术家</option>
          <option value="title">按标题</option>
          <option value="album">按专辑</option>
          <option value="added">最近添加</option>
        </select>
        <button class="btn primary" :disabled="library.scanning" @click="pickFolder">
          <Icon name="folder" :size="16" />
          {{ library.scanning ? "扫描中…" : "添加文件夹" }}
        </button>
      </div>
    </header>

    <div class="body scroll">
      <TrackList
        :tracks="sorted"
        :current-id="player.snap.track?.id"
        :favorite-ids="library.favoriteIds"
        empty-text="点右上角「添加文件夹」，选一个音乐目录开始"
        @play="play"
        @favorite="fav"
        @add-queue="addQueue"
        @add-playlist="addPlaylist"
        @remove="remove"
      />
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
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
  padding-bottom: 6px;
}
.tools {
  display: flex;
  align-items: center;
  gap: 8px;
  padding-bottom: 12px;
}
.search-mini {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 38px;
  padding: 0 12px;
  border-radius: 999px;
  background: var(--panel);
  border: 1px solid var(--border);
  color: var(--text-3);
}
.search-mini input {
  border: none;
  background: none;
  outline: none;
  color: var(--text);
  font-size: 13px;
  width: 190px;
}
.select {
  height: 38px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  padding: 0 12px;
  font-size: 12.5px;
  outline: none;
}
.body {
  flex: 1;
  min-height: 0;
}

/*
 * 竖屏手机：标题、筛选框、排序、按钮全部竖着排。
 * 原来是「搜索框 190px + 下拉 + 按钮」横着塞，384px 宽度下按钮被挤到
 * 第二行还超宽出屏幕。
 */
@media (max-width: 860px) {
  .head {
    align-items: stretch;
    gap: 4px;
    padding-bottom: 2px;
  }
  .tools {
    flex-wrap: wrap;
    gap: 8px;
    padding-bottom: 10px;
    width: 100%;
  }
  /* 搜索框独占一行，剩下的下拉和按钮平分第二行 */
  .search-mini {
    flex: 1 1 100%;
    height: 44px;
  }
  .search-mini input {
    width: 100%;
  }
  .select {
    flex: 1 1 0;
    height: 44px;
  }
  .btn {
    flex: 1 1 0;
  }
}

/*
 * 横屏：高度只有 390，工具条再竖着排三层就什么都不剩了。
 * 横向空间足够，标题 + 搜索框 + 排序 + 按钮挤回一行。
 */
@media (max-width: 900px) and (max-height: 520px) {
  .head {
    flex-wrap: nowrap;
    align-items: center;
    gap: 12px;
  }
  .head > div:first-child {
    flex: none;
  }
  .tools {
    flex: 1;
    min-width: 0;
    width: auto;
    flex-wrap: nowrap;
    gap: 6px;
    padding-bottom: 4px;
  }
  .search-mini {
    flex: 1 1 auto;
    min-width: 0;
    height: 40px;
  }
  .select {
    flex: 0 0 118px;
    height: 40px;
  }
  .btn {
    flex: 0 0 auto;
    height: 40px;
    padding: 0 14px;
  }
}
</style>
