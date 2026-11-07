<script setup lang="ts">
/** 播放队列抽屉。 */
import { storeToRefs } from "pinia";

import Icon from "@/components/Icon.vue";
import { player as playerApi } from "@/api/ipc";
import { usePlayerStore } from "@/stores/player";
import { useUiStore } from "@/stores/ui";

const player = usePlayerStore();
const ui = useUiStore();
const { queue } = storeToRefs(player);

function fmt(ms: number): string {
  if (!ms) return "--:--";
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, "0")}`;
}

async function playAt(i: number) {
  const ids = queue.value.items.map((t) => t.id);
  await playerApi.playQueue(ids, i);
}

async function removeAt(i: number) {
  await playerApi.removeFromQueue(i);
}

async function move(from: number, to: number) {
  if (to < 0 || to >= queue.value.items.length) return;
  await playerApi.moveQueue(from, to);
}
</script>

<template>
  <transition name="fade">
    <div v-if="ui.queueOpen" class="backdrop" @click="ui.queueOpen = false" />
  </transition>

  <aside class="drawer" :class="{ open: ui.queueOpen }">
    <header class="head">
      <div>
        <div class="title">播放队列</div>
        <div class="sub">{{ queue.items.length }} 首</div>
      </div>
      <div class="ops">
        <button class="icon-btn" title="清空" @click="playerApi.clearQueue()">
          <Icon name="trash" :size="17" />
        </button>
        <button class="icon-btn" title="收起" @click="ui.queueOpen = false">
          <Icon name="chevron" :size="18" />
        </button>
      </div>
    </header>

    <div class="list scroll">
      <div v-if="!queue.items.length" class="empty">
        <div class="big">♪</div>
        <div>队列是空的</div>
      </div>

      <div
        v-for="(t, i) in queue.items"
        :key="t.id + i"
        class="item"
        :class="{ on: i === queue.current }"
        @click="playAt(i)"
      >
        <div class="idx mono">{{ i + 1 }}</div>
        <div class="meta">
          <div class="t">{{ t.title }}</div>
          <div class="a">{{ t.artist || "未知" }}</div>
        </div>
        <div class="dur mono">{{ fmt(t.durationMs) }}</div>
        <div class="row-ops">
          <!-- Icon 是单根 svg，透传的 style 会落到根元素上；
               借用 chevron-down 旋转 180° 表示「上移」，少维护一个图标 -->
          <button class="icon-btn tiny" title="上移" @click.stop="move(i, i - 1)">
            <Icon name="chevron-down" :size="13" style="transform: rotate(180deg)" />
          </button>
          <button class="icon-btn tiny" title="移除" @click.stop="removeAt(i)">
            <Icon name="close" :size="13" />
          </button>
        </div>
      </div>
    </div>
  </aside>
</template>

<style scoped>
/*
 * 遮罩只在移动端出现。
 *
 * 桌面端抽屉是「侧边浮层」，点外面本来就能继续操作，遮罩反而碍事；
 * 移动端改成了底部 sheet，不遮挡的话手指会误点到底下的列表。
 */
.backdrop {
  display: none;
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.22s;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.drawer {
  position: fixed;
  /* 无框窗口：给自绘标题栏让位（38px 带 + 原 18px 视觉留白） */
  top: calc(var(--titlebar-h) + 18px);
  right: 18px;
  bottom: 104px;
  width: 340px;
  z-index: 40;
  display: flex;
  flex-direction: column;
  background: color-mix(in srgb, var(--panel) 94%, transparent);
  backdrop-filter: blur(24px);
  border: 1px solid var(--border);
  border-radius: var(--radius-l);
  box-shadow: var(--shadow);
  transform: translateX(calc(100% + 24px));
  opacity: 0;
  pointer-events: none;
  transition: transform 0.32s var(--ease), opacity 0.24s;
}
.drawer.open {
  transform: translateX(0);
  opacity: 1;
  pointer-events: auto;
}
.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 16px 10px;
  border-bottom: 1px solid var(--border);
}
.title {
  font-size: 14px;
  font-weight: 600;
}
.sub {
  font-size: 11px;
  color: var(--text-3);
  margin-top: 2px;
}
.ops {
  display: flex;
  gap: 2px;
}
.list {
  flex: 1;
  min-height: 0;
  padding: 6px;
}
.item {
  display: grid;
  grid-template-columns: 22px minmax(0, 1fr) auto auto;
  align-items: center;
  gap: 8px;
  padding: 7px 8px;
  border-radius: 10px;
}
@media (hover: hover) {
  .item:hover {
    background: var(--hover);
  }
}
.item {
  cursor: pointer;
}
.item.on .t {
  color: var(--accent);
}
.idx {
  font-size: 10.5px;
  color: var(--text-3);
  text-align: center;
}
.meta {
  min-width: 0;
}
.t {
  font-size: 12.5px;
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.a {
  font-size: 10.5px;
  color: var(--text-3);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.dur {
  font-size: 10.5px;
  color: var(--text-3);
}
.row-ops {
  display: none;
  gap: 1px;
}
@media (hover: hover) {
  .item:hover .row-ops {
    display: flex;
  }
  .item:hover .dur {
    display: none;
  }
}
.icon-btn.tiny {
  width: 22px;
  height: 22px;
}

@media (max-width: 860px) {
  .backdrop {
    display: block;
    position: fixed;
    inset: 0;
    z-index: 39;
    background: rgba(0, 0, 0, 0.42);
    backdrop-filter: blur(2px);
  }

  /*
   * 移动端改成底部抽屉（bottom sheet）。
   *
   * 旧值是 top:auto / bottom:148px / height:52vh —— 148 是按「播放条 78 +
   * tab 62」算的，而现在这两块高度都变了，写死的 magic number 必然错位；
   * 而且 fixed 元素不吃 .shell 的安全区 padding，底部会被手势条压住。
   */
  .drawer {
    top: auto;
    left: 0;
    right: 0;
    bottom: 0;
    width: auto;
    height: min(66vh, 560px);
    border-radius: 22px 22px 0 0;
    border-bottom: none;
    padding-bottom: var(--sab);
    transform: translateY(100%);
    /* 移动端常叠在播放页的模糊封面上，94% 半透明会让文字糊成一团：
       换成实底面板色 + 更强毛玻璃兜底（桌面端侧边浮层保持半透明不变） */
    background: var(--panel);
    backdrop-filter: blur(32px) saturate(1.2);
    -webkit-backdrop-filter: blur(32px) saturate(1.2);
  }
  .drawer.open {
    transform: translateY(0);
  }

  /* 顶部小把手，暗示可以往下拖/点外面关闭 */
  .head {
    position: relative;
    padding: 22px 16px 10px;
  }
  .head::before {
    content: "";
    position: absolute;
    top: 8px;
    left: 50%;
    transform: translateX(-50%);
    width: 36px;
    height: 4px;
    border-radius: 2px;
    background: var(--border-strong);
  }
  .title {
    font-size: 15px;
  }

  .item {
    grid-template-columns: 24px minmax(0, 1fr) auto;
    padding: 11px 8px;
    gap: 10px;
  }
  .item.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .t {
    font-size: 13.5px;
  }
  .a {
    font-size: 11.5px;
  }
  /* 触摸设备没有 hover，操作按钮必须常显 */
  .dur {
    display: none;
  }
  .row-ops {
    display: flex;
  }
  .row-ops .icon-btn.tiny {
    width: 32px;
    height: 32px;
  }
}
</style>
