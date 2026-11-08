<script setup lang="ts">
/** 我喜欢的音乐。 */
import { onMounted } from "vue";
import { storeToRefs } from "pinia";

import Icon from "@/components/Icon.vue";
import TrackList from "@/components/TrackList.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";
import type { Track } from "@/types";

const library = useLibraryStore();
const player = usePlayerStore();
const ui = useUiStore();
const { favorites } = storeToRefs(library);

onMounted(() => void library.init());

function play(_t: Track, i: number) {
  void player.playList(favorites.value, i);
}

async function addQueue(t: Track) {
  await player.addToQueue([t], false);
  ui.toast("已加入队列");
}
</script>

<template>
  <div class="page">
    <header class="head">
      <div class="badge">
        <Icon name="heart" :size="22" filled />
      </div>
      <div>
        <h1 class="page-title">我喜欢的音乐</h1>
        <p class="page-sub">{{ favorites.length }} 首</p>
        <button class="btn primary" :disabled="!favorites.length" @click="player.playList(favorites, 0)">
          <Icon name="play" :size="15" filled /> 播放全部
        </button>
      </div>
    </header>

    <div class="body scroll">
      <TrackList
        :tracks="favorites"
        :current-id="player.snap.track?.id"
        :favorite-ids="library.favoriteIds"
        empty-text="点歌曲右侧的心形收藏"
        @play="play"
        @favorite="(t) => library.toggleFavorite(t.id)"
        @add-queue="addQueue"
        @add-playlist="addQueue"
        @remove="(t) => library.toggleFavorite(t.id)"
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
  gap: 18px;
  align-items: center;
  padding-bottom: 16px;
}
.badge {
  width: 86px;
  height: 86px;
  border-radius: 24px;
  display: grid;
  place-items: center;
  background: linear-gradient(135deg, #ff5c8a, #ff8f6b);
  color: #fff;
  box-shadow: 0 16px 40px -16px #ff5c8a;
  flex: none;
}
.body {
  flex: 1;
  min-height: 0;
}

@media (max-width: 860px) {
  .head {
    gap: 14px;
    padding-bottom: 12px;
    /* 顶部对齐：居中会让 66px 的徽章下沉到「播放全部」那一行，
       看起来像和标题错位了 */
    align-items: flex-start;
  }
  .badge {
    width: 66px;
    height: 66px;
    border-radius: 19px;
    margin-top: 2px;
    box-shadow: 0 12px 28px -14px #ff5c8a;
  }
  .head .btn {
    margin-top: 10px;
  }
}

@media (max-width: 900px) and (max-height: 520px) {
  .head {
    padding-bottom: 6px;
    gap: 12px;
  }
  .badge {
    width: 48px;
    height: 48px;
    border-radius: 14px;
  }
  .badge :deep(svg) {
    width: 18px;
    height: 18px;
  }
  .head .btn {
    margin-top: 4px;
    height: 36px;
  }
}
</style>
