<script setup lang="ts">
/** 通用曲目列表页：我喜欢的 / 最近播放 / 已缓存共用一套 KA 风格行。 */
import Icon from "@/components/Icon.vue";
import MobileTrackRow from "@/mobile/MobileTrackRow.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { Track } from "@/types";

const props = withDefaults(
  defineProps<{
    title: string;
    tracks: Track[];
    emptyText?: string;
    /** 嵌在二级页容器里：不自己绝对定位，也不重复画返回头 */
    embedded?: boolean;
  }>(),
  { emptyText: "这里还没有歌曲", embedded: false },
);

const emit = defineEmits<{ (e: "back"): void }>();

const library = useLibraryStore();
const player = usePlayerStore();
const ui = useUiStore();

function playAll() {
  if (!props.tracks.length) return;
  void player.playTracks(props.tracks, 0);
  ui.toast("已开始播放");
}

function playRow(t: Track) {
  const i = props.tracks.findIndex((x) => x.id === t.id);
  void player.playTracks(props.tracks, Math.max(0, i));
}

async function fav(t: Track) {
  const on = await library.toggleFavorite(t.id);
  ui.toast(on ? "已加入我喜欢的音乐" : "已取消喜欢", "success");
}

function more(t: Track) {
  void player.addToQueue([t], false);
  ui.toast(`「${t.title}」已加入队列`);
}
</script>

<template>
  <div class="m-page list-page" :class="{ embedded }">
    <div v-if="!embedded" class="m-top">
      <button class="m-back" @click="emit('back')">
        <Icon name="chevron" :size="20" class="chev" />
        <span class="m-title">{{ title }}</span>
      </button>
      <button class="m-round primary" aria-label="播放全部" @click="playAll">
        <Icon name="play" :size="18" filled />
      </button>
    </div>

    <div v-if="tracks.length" class="rows">
      <MobileTrackRow
        v-for="t in tracks"
        :key="t.id"
        :track="t"
        :fav="library.favoriteIds.has(t.id)"
        :current="player.snap.track?.id === t.id"
        @play="playRow"
        @favorite="fav"
        @more="more"
      />
    </div>
    <div v-else class="m-empty">
      <Icon name="music" :size="34" />
      <div>{{ emptyText }}</div>
    </div>
  </div>
</template>

<style scoped>
.list-page {
  padding-left: 18px;
  padding-right: 18px;
}
.m-page.embedded {
  position: relative;
  inset: auto;
  padding: 0 0 24px;
  overflow: visible;
}
.m-back {
  min-width: 0;
}
.m-title {
  font-size: 22px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.rows {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
</style>
