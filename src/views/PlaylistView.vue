<script setup lang="ts">
/** 歌单详情 / 歌单管理。 */
import { computed, onMounted, ref } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import TrackList from "@/components/TrackList.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { Track } from "@/types";

const library = useLibraryStore();
const player = usePlayerStore();
const ui = useUiStore();

const creating = ref(false);
const newName = ref("");

onMounted(() => void library.init());

const active = computed(() => library.activePlaylist);
const tracks = computed(() => library.playlistTracks);
/** 头图：歌单自己记的封面（云端导入写入）优先，否则回退到第一首有封面的曲目 */
const cover = computed(() => {
  const t = tracks.value.find((x) => x.coverPath || x.coverUrl);
  return {
    path: active.value?.coverPath ?? t?.coverPath ?? null,
    url: active.value?.coverUrl ?? t?.coverUrl ?? null,
  };
});

async function create() {
  const name = newName.value.trim();
  if (!name) return;
  const p = await library.createPlaylist(name);
  newName.value = "";
  creating.value = false;
  await library.openPlaylist(p.id);
  ui.toast(`已创建「${p.name}」`, "success");
}

function play(t: Track, i: number) {
  void player.playList(tracks.value, i);
}

async function remove(t: Track) {
  if (!active.value) return;
  await import("@/api/ipc").then((m) => m.playlist.removeTracks(active.value!.id, [t.id]));
  await library.openPlaylist(active.value.id);
  ui.toast("已从歌单移除");
}

async function del() {
  if (!active.value) return;
  await library.deletePlaylist(active.value.id);
  ui.toast("歌单已删除", "success");
}

async function addQueue(t: Track) {
  await player.addToQueue([t], false);
  ui.toast("已加入队列");
}
</script>

<template>
  <div class="page">
    <template v-if="active">
      <header class="head">
        <CoverArt :cover-path="cover.path" :cover-url="cover.url" size="120px" radius="18px" />
        <div class="meta">
          <div class="kind">歌单</div>
          <h1 class="page-title">{{ active.name }}</h1>
          <p class="page-sub">{{ active.trackCount }} 首 · 创建于 {{ new Date(active.createdAt).toLocaleDateString() }}</p>
          <div class="ops">
            <button class="btn primary" :disabled="!tracks.length" @click="player.playList(tracks, 0)">
              <Icon name="play" :size="15" filled /> 播放全部
            </button>
            <button class="btn ghost" @click="player.addToQueue(tracks, false)">
              <Icon name="queue" :size="15" /> 加入队列
            </button>
            <button v-if="!active.system" class="btn ghost danger" @click="del">
              <Icon name="trash" :size="15" /> 删除歌单
            </button>
          </div>
        </div>
      </header>

      <div class="body scroll">
        <TrackList
          :tracks="tracks"
          :current-id="player.snap.track?.id"
          :favorite-ids="library.favoriteIds"
          empty-text="歌单还是空的，去本地音乐右键添加"
          @play="play"
          @favorite="(t) => library.toggleFavorite(t.id)"
          @add-queue="addQueue"
          @add-playlist="addQueue"
          @remove="remove"
        />
      </div>
    </template>

    <template v-else>
      <header class="head">
        <div>
          <h1 class="page-title">我的歌单</h1>
          <p class="page-sub">{{ library.playlists.length }} 个歌单</p>
        </div>
        <button class="btn primary" @click="creating = true">
          <Icon name="plus" :size="16" /> 新建歌单
        </button>
      </header>

      <div v-if="creating" class="create card">
        <input v-model="newName" placeholder="歌单名称" @keyup.enter="create" autofocus />
        <button class="btn primary" @click="create">创建</button>
        <button class="btn ghost" @click="creating = false">取消</button>
      </div>

      <div class="grid scroll">
        <button
          v-for="p in library.playlists"
          :key="p.id"
          class="pl-card"
          @click="library.openPlaylist(p.id)"
        >
          <CoverArt
            class="pl-cover"
            :cover-path="p.coverPath"
            :cover-url="p.coverUrl"
            size="100%"
            radius="10px"
          />
          <div class="pl-name">{{ p.name }}</div>
          <div class="pl-count">{{ p.trackCount }} 首</div>
        </button>
      </div>
    </template>
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
  gap: 20px;
  align-items: flex-end;
  padding-bottom: 18px;
}
.meta {
  min-width: 0;
}
.kind {
  font-size: 10.5px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--text-3);
  margin-bottom: 4px;
}
.ops {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.danger {
  color: #ff6b6b;
}
.body {
  flex: 1;
  min-height: 0;
}
.create {
  display: flex;
  gap: 8px;
  padding: 12px;
  margin-bottom: 14px;
  align-items: center;
}
.create input {
  flex: 1;
  height: 38px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text);
  padding: 0 12px;
  outline: none;
  font-size: 13px;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
  gap: 14px;
  padding-bottom: 24px;
  min-height: 0;
}
.pl-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border-radius: var(--radius-m);
  background: var(--panel);
  border: 1px solid var(--border);
  text-align: left;
  transition: transform 0.16s var(--ease), background 0.16s;
}
.pl-card:hover {
  transform: translateY(-2px);
  background: var(--panel-2);
}
.pl-cover {
  aspect-ratio: 1;
  border-radius: 10px;
  width: 100%;
}
.pl-name {
  font-size: 13px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pl-count {
  font-size: 11px;
  color: var(--text-3);
}

@media (max-width: 860px) {
  /* 封面在左、信息在右：竖排会把 120px 封面 + 三个按钮叠成高高一列，
     列表被推到屏幕外 */
  .head {
    flex-direction: row;
    align-items: flex-start;
    gap: 13px;
    padding-bottom: 12px;
  }
  /* CoverArt 的宽高是内联 style */
  .head :deep(.cover) {
    width: 82px !important;
    height: 82px !important;
    border-radius: 14px !important;
  }
  .kind {
    margin-bottom: 2px;
    font-size: 9.5px;
  }
  .page-title {
    font-size: 19px;
  }
  .ops {
    gap: 6px;
    margin-top: 8px;
  }
  .ops .btn {
    height: 38px;
    padding: 0 14px;
    font-size: 12.5px;
  }
  .create {
    flex-wrap: wrap;
  }
  .create input {
    flex: 1 1 100%;
    height: 44px;
  }
  .grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
  }
  .pl-card {
    padding: 10px;
  }
}

@media (max-width: 900px) and (max-height: 520px) {
  .head {
    padding-bottom: 6px;
    gap: 10px;
  }
  .head :deep(.cover) {
    width: 58px !important;
    height: 58px !important;
    border-radius: 12px !important;
  }
  .ops {
    margin-top: 4px;
  }
  .ops .btn {
    height: 34px;
    font-size: 12px;
  }
  .grid {
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 8px;
  }
}
</style>
