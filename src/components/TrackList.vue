<script setup lang="ts">
/**
 * 曲目列表。桌面双击播放、hover 出现操作按钮；
 * `click-play`（安卓触摸端）改为单击播放——触摸上双击反直觉。
 */
import { computed } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import type { Track } from "@/types";

const props = withDefaults(
  defineProps<{
    tracks: Track[];
    currentId?: string | null;
    favoriteIds?: Set<string>;
    showCover?: boolean;
    showIndex?: boolean;
    emptyText?: string;
    /** 单击整行即播放（安卓移动端二级页用），桌面默认仍为双击 */
    clickPlay?: boolean;
  }>(),
  { currentId: null, showCover: true, showIndex: true, emptyText: "这里还没有歌曲", clickPlay: false },
);

const emit = defineEmits<{
  (e: "play", track: Track, index: number): void;
  (e: "favorite", track: Track): void;
  (e: "add-playlist", track: Track): void;
  (e: "add-queue", track: Track): void;
  (e: "remove", track: Track): void;
}>();

const hasTracks = computed(() => props.tracks.length > 0);

function fmt(ms: number): string {
  if (!ms || ms < 0) return "--:--";
  const total = Math.round(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

function isFav(t: Track) {
  return props.favoriteIds?.has(t.id) ?? false;
}

function onRowClick(t: Track, i: number) {
  if (props.clickPlay) emit("play", t, i);
}
</script>

<template>
  <div class="list">
    <div v-if="!hasTracks" class="empty">
      <div class="big">♫</div>
      <div>{{ emptyText }}</div>
    </div>

    <div
      v-for="(t, i) in tracks"
      :key="t.id + i"
      class="row"
      :class="{ playing: t.id === currentId }"
      @click="onRowClick(t, i)"
      @dblclick="emit('play', t, i)"
    >
      <div v-if="showIndex" class="idx mono">
        <span class="num">{{ i + 1 }}</span>
        <span class="play-hint">
          <Icon :name="t.id === currentId ? 'volume-low' : 'play'" :size="15" filled />
        </span>
      </div>

      <CoverArt
        v-if="showCover"
        :cover-path="t.coverPath"
        :cover-url="t.coverUrl"
        size="42px"
        radius="8px"
        class="thumb"
      />

      <div class="meta">
        <div class="title">
          {{ t.title }}
          <span v-if="t.source.kind === 'plugin'" class="badge">{{ t.source.pluginId }}</span>
        </div>
        <div class="sub">{{ t.artist || "未知艺术家" }}<template v-if="t.album"> · {{ t.album }}</template></div>
      </div>

      <div class="actions">
        <button
          class="icon-btn small"
          :class="{ fav: isFav(t) }"
          title="收藏"
          @click.stop="emit('favorite', t)"
        >
          <Icon name="heart" :size="16" :filled="isFav(t)" />
        </button>
        <button class="icon-btn small" title="加入队列" @click.stop="emit('add-queue', t)">
          <Icon name="queue" :size="16" />
        </button>
        <button class="icon-btn small" title="加入歌单" @click.stop="emit('add-playlist', t)">
          <Icon name="plus" :size="16" />
        </button>
        <button class="icon-btn small" title="从列表移除" @click.stop="emit('remove', t)">
          <Icon name="close" :size="15" />
        </button>
      </div>

      <div class="dur mono">{{ fmt(t.durationMs) }}</div>
    </div>
  </div>
</template>

<style scoped>
.list {
  display: flex;
  flex-direction: column;
  padding: 4px 2px 20px;
}

.row {
  display: grid;
  grid-template-columns: 34px auto minmax(0, 1fr) auto 54px;
  align-items: center;
  gap: 12px;
  padding: 8px 10px;
  border-radius: var(--radius-m);
  cursor: pointer;
  transition: background 0.15s;
  position: relative;
}
/* hover 只在真的有指针的设备上生效：触摸设备上 tap 一次会让 :hover 粘住，
   整行一直亮着，看着像选中了。 */
@media (hover: hover) {
  .row:hover {
    background: var(--hover);
  }
}
.row.playing {
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.row.playing .title {
  color: var(--accent);
}
/* 正在播放的左侧色条：光靠背景色在浅色主题下不够明显 */
.row.playing::before {
  content: "";
  position: absolute;
  left: 2px;
  top: 50%;
  transform: translateY(-50%);
  width: 3px;
  height: 55%;
  border-radius: 3px;
  background: var(--accent);
}

.idx {
  font-size: 12px;
  color: var(--text-3);
  display: grid;
  place-items: center;
  height: 24px;
}
.play-hint {
  display: none;
  color: var(--text);
}
@media (hover: hover) {
  .row:hover .num {
    display: none;
  }
  .row:hover .play-hint {
    display: block;
  }
}

.thumb {
  box-shadow: none;
}

.meta {
  min-width: 0;
}
.title {
  font-size: 14px;
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  display: flex;
  align-items: center;
  gap: 8px;
}
.sub {
  font-size: 12px;
  color: var(--text-3);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
}
.badge {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--panel-3);
  color: var(--text-2);
  font-weight: 400;
  flex: none;
}

.actions {
  display: flex;
  gap: 2px;
  opacity: 0;
  transition: opacity 0.15s;
}
@media (hover: hover) {
  .row:hover .actions {
    opacity: 1;
  }
}
.icon-btn.small {
  width: 28px;
  height: 28px;
}
.icon-btn.fav {
  color: #ff5c8a;
}

.dur {
  font-size: 12px;
  color: var(--text-3);
  text-align: right;
}

@media (max-width: 860px) {
  /*
   * 竖屏只剩 356px 可用宽度，五项栅格（序号/封面/信息/按钮/时长）
   * 会把标题压成三四个字。这里砍到三项：封面 / 信息 / 收藏+队列。
   * 序号和时长在手机上本来也没什么用（想找第几首不如直接看封面）。
   */
  .row {
    grid-template-columns: auto minmax(0, 1fr) auto;
    padding: 9px 8px;
    gap: 10px;
    min-height: 60px;
  }
  .idx {
    display: none;
  }
  .dur {
    display: none;
  }
  .actions {
    opacity: 1;
    gap: 0;
  }
  /* 只留收藏和加入队列；加入歌单/移除在手机上没地方放 */
  .actions .icon-btn:nth-child(n + 3) {
    display: none;
  }
  .actions .icon-btn {
    width: 36px;
    height: 36px;
  }
  .title {
    font-size: 14px;
  }
  .sub {
    font-size: 11.5px;
    margin-top: 3px;
  }
  .badge {
    font-size: 9.5px;
    padding: 1px 5px;
  }
  .row:active {
    background: var(--hover);
  }
  .list {
    padding: 2px 0 12px;
  }
}
</style>
