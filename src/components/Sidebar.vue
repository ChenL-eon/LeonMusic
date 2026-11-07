<script setup lang="ts">
/** 侧边导航。窄屏时自动变成底部 tab。
 *  「正在播放」「在线搜索」不再是一级项：
 *  前者点击底部播放条的歌曲进入全屏覆盖层，后者从首页搜索框进入。 */
import { computed } from "vue";

import BrandLogo from "@/components/BrandLogo.vue";
import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useUiStore, type Page } from "@/stores/ui";

const ui = useUiStore();
const library = useLibraryStore();
const player = usePlayerStore();

interface Item {
  id: Page;
  label: string;
  icon: string;
}

const main: Item[] = [
  { id: "home", label: "首页", icon: "home" },
  { id: "library", label: "本地音乐", icon: "library" },
  { id: "favorites", label: "我喜欢的", icon: "heart" },
  { id: "account", label: "账号", icon: "user" },
];

const playlists = computed(() => library.playlists.filter((p) => !p.system));

const kHz = (hz?: number) => ((hz || 0) / 1000).toFixed(1);

/** 源采样率与输出率不同时显示「44.1→48.0」，相同（或未知）只显示输出率。 */
const rateLabel = computed(() => {
  const out = player.snap.outputSampleRate;
  const src = player.snap.sourceSampleRate || 0;
  if (!src || src === out) return `${kHz(out)} kHz`;
  return `${kHz(src)}→${kHz(out)} kHz`;
});

const rateTitle = computed(() => {
  const src = player.snap.sourceSampleRate || 0;
  return src && src !== player.snap.outputSampleRate
    ? `源文件 ${kHz(src)} kHz → 输出固定 ${kHz(player.snap.outputSampleRate)} kHz（重采样）`
    : `输出固定 ${kHz(player.snap.outputSampleRate)} kHz（引擎统一重采样率）`;
});

function go(p: Page) {
  ui.go(p);
}
</script>

<template>
  <aside class="sidebar">
    <div class="brand">
      <div class="logo">
        <BrandLogo :size="34" />
      </div>
      <div class="name">LeonMusic</div>
    </div>

    <nav class="nav">
      <button
        v-for="it in main"
        :key="it.id"
        class="nav-item"
        :class="{ on: ui.page === it.id }"
        @click="go(it.id)"
      >
        <Icon :name="it.icon" :size="19" />
        <span>{{ it.label }}</span>
        <span v-if="it.id === 'favorites' && library.favorites.length" class="count">{{
          library.favorites.length
        }}</span>
      </button>
    </nav>

    <div class="section">
      <div class="section-head">
        <span>我的歌单</span>
        <button class="icon-btn small" title="新建歌单" @click="go('playlist')">
          <Icon name="plus" :size="15" />
        </button>
      </div>
      <div class="plist">
        <button
          v-for="p in playlists"
          :key="p.id"
          class="pl-item"
          @click="library.openPlaylist(p.id); go('playlist')"
        >
          <CoverArt
            class="pl-cover"
            :cover-path="p.coverPath"
            :cover-url="p.coverUrl"
            size="26px"
            radius="7px"
          />
          <span class="pl-name">{{ p.name }}</span>
          <span class="count">{{ p.trackCount }}</span>
        </button>
        <div v-if="!playlists.length" class="hint">还没有自建歌单</div>
      </div>
    </div>

    <div class="spacer" />

    <button class="nav-item" :class="{ on: ui.page === 'settings' }" @click="go('settings')">
      <Icon name="settings" :size="19" />
      <span>设置</span>
    </button>

    <div
      class="status"
      v-if="player.snap.outputSampleRate"
      :title="rateTitle"
    >
      <span class="mono">{{ rateLabel }}</span>
      <span class="sep">·</span>
      <span>{{ player.snap.status === "playing" ? "播放中" : "就绪" }}</span>
    </div>
  </aside>
</template>

<style scoped>
.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 8px 4px;
}
.logo {
  width: 34px;
  height: 34px;
  /* BrandLogo 自带圆角渐变底；投影用 drop-shadow 跟随圆角轮廓 */
  filter: drop-shadow(0 8px 14px rgba(119, 69, 250, 0.38));
}
.name {
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.02em;
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 11px;
  height: 40px;
  padding: 0 12px;
  border-radius: 11px;
  color: var(--text-2);
  font-size: 13.5px;
  font-weight: 500;
  transition: background 0.15s, color 0.15s;
  text-align: left;
  width: 100%;
}
.nav-item:hover {
  background: var(--hover);
  color: var(--text);
}
.nav-item.on {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
}
.count {
  margin-left: auto;
  font-size: 11px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}

.section {
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 12px;
  font-size: 11px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--text-3);
}
.icon-btn.small {
  width: 24px;
  height: 24px;
}
.plist {
  overflow-y: auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.pl-item {
  display: flex;
  align-items: center;
  gap: 9px;
  height: 32px;
  padding: 0 12px;
  border-radius: 9px;
  color: var(--text-2);
  font-size: 13px;
  text-align: left;
}
.pl-item:hover {
  background: var(--hover);
  color: var(--text);
}
/* 歌单封面缩略图：去掉 CoverArt 自带的投影，26px 小图不需要 */
.pl-cover {
  box-shadow: none;
}
.pl-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.hint {
  font-size: 12px;
  color: var(--text-3);
  padding: 6px 12px;
}
.spacer {
  flex: 1;
}
.status {
  display: flex;
  gap: 6px;
  align-items: center;
  padding: 8px 12px 0;
  font-size: 10.5px;
  color: var(--text-3);
}
.sep {
  opacity: 0.5;
}

@media (max-width: 860px) {
  .brand,
  .section,
  .status,
  .spacer,
  .nav-item .count {
    display: none;
  }

  /* 移动端变成底部 tab。
     `.nav` 用 display:contents 让 4 个 tab 与末尾的「设置」成为同一层
     flex 项 —— 之前 `.nav` 带 width:100%，把设置按钮挤到右边只剩十几像素，
     「设置」两个字被压成竖排。 */
  .nav {
    display: contents;
  }

  .sidebar {
    flex-direction: row;
    align-items: stretch;
    justify-content: space-between;
    min-height: 54px;
  }

  /* 6 个 tab 均分（含「账号」），字号与图标略收一点避免 360px 宽屏挤压 */
  .nav-item {
    flex: 1 1 0;
    min-width: 0;
    flex-direction: column;
    justify-content: center;
    gap: 2px;
    width: auto;
    height: auto;
    min-height: 52px;
    margin: 2px 1px;
    padding: 4px 1px;
    border-radius: 13px;
    font-size: 10px;
    line-height: 1.2;
    white-space: nowrap;
    letter-spacing: 0;
  }
  /* Icon 的宽高是 svg 属性，这里用 CSS 覆盖成移动端尺寸 */
  .nav-item :deep(svg) {
    width: 20px;
    height: 20px;
  }
  /*
   * 选中态用胶囊底色 + 主色文字。
   * 试过在图标上方画一条 2.5px 的短横，截图看像是「下划线跑到了上面」，
   * 而且和图标几乎贴在一起，反而更乱。
   */
  .nav-item.on {
    background: color-mix(in srgb, var(--accent) 13%, transparent);
    color: var(--accent);
  }
  .nav-item:active {
    background: var(--hover);
  }
}
</style>
