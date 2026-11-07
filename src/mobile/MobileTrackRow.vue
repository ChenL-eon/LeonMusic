<script setup lang="ts">
/** 参考 KA Music 的歌曲行：方形封面 + 标题/副标题 + 心形 + 三点。 */
import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import type { Track } from "@/types";

withDefaults(
  defineProps<{
    track: Track;
    fav?: boolean;
    current?: boolean;
  }>(),
  { fav: false, current: false },
);

const emit = defineEmits<{
  (e: "play", track: Track): void;
  (e: "favorite", track: Track): void;
  (e: "more", track: Track): void;
}>();
</script>

<template>
  <div class="row" :class="{ current }" @click="emit('play', track)">
    <CoverArt
      :cover-path="track.coverPath"
      :cover-url="track.coverUrl"
      size="52px"
      radius="12px"
      class="art"
    />
    <div class="meta">
      <div class="title">{{ track.title }}</div>
      <div class="sub">{{ track.artist || "未知艺术家" }}</div>
    </div>
    <button class="m-icon heart" :class="{ on: fav }" aria-label="喜欢" @click.stop="emit('favorite', track)">
      <Icon name="heart" :size="20" :filled="fav" />
    </button>
    <button class="m-icon" aria-label="更多" @click.stop="emit('more', track)">
      <Icon name="more" :size="18" filled />
    </button>
  </div>
</template>

<style scoped>
.row {
  display: grid;
  grid-template-columns: 52px minmax(0, 1fr) 36px 30px;
  align-items: center;
  gap: 12px;
  padding: 7px 2px;
  border-radius: var(--m-r-sm);
}
.row:active {
  background: var(--hover);
}
.art {
  box-shadow: var(--m-shadow);
}
.meta {
  min-width: 0;
}
.title,
.sub {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.title {
  font-size: 14.5px;
  font-weight: 600;
  color: var(--text);
}
.current .title {
  color: var(--accent);
}
.sub {
  font-size: 12px;
  color: var(--text-3);
  margin-top: 3px;
}
.m-icon {
  width: 36px;
  height: 36px;
}
.heart {
  color: var(--text-3);
}
.heart.on {
  color: #ef5a72;
}
</style>
