<script setup lang="ts">
/** 内联 SVG 图标，避免引入图标库（体积 + 风格统一）。 */
import { computed } from "vue";

const props = withDefaults(
  defineProps<{
    name: string;
    size?: number | string;
    stroke?: number;
    filled?: boolean;
  }>(),
  { size: 20, stroke: 1.8, filled: false },
);

const PATHS: Record<string, string> = {
  play: "M8 5.5v13l11-6.5z",
  pause: "M9 5v14M15 5v14",
  next: "M6 5l9 7-9 7zM17 5v14",
  prev: "M18 5l-9 7 9 7zM7 5v14",
  stop: "M7 7h10v10H7z",
  volume:
    "M4 10v4h3l4 4V6L7 10H4zM16 9a4 4 0 010 6M18.5 6.5a8 8 0 010 11",
  "volume-low": "M4 10v4h3l4 4V6L7 10H4zM15.5 9.5a3.5 3.5 0 010 5",
  "volume-mute": "M4 10v4h3l4 4V6L7 10H4zM16 9l5 6M21 9l-5 6",
  heart:
    "M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0016.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 002 8.5c0 2.3 1.5 4.05 3 5.5l7 7z",
  queue: "M4 6h11M4 11h11M4 16h7M17 13v6l4-3z",
  /* 桌面歌词：屏幕 + 内嵌歌词行（比纯文本行更直观表达「悬浮在桌面的歌词窗」） */
  lyrics:
    "M3.5 5h17a1 1 0 011 1v9a1 1 0 01-1 1h-17a1 1 0 01-1-1V6a1 1 0 011-1zM12 16.5v4M8.5 20.5h7M7 9.5h7.5M7 12.5h4.5",
  search: "M11 4a7 7 0 100 14 7 7 0 000-14zM20 20l-4-4",
  library:
    "M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2zM10.5 17.2V10l6-1.5v6.7M10.5 17.2a1.4 1.4 0 11-2.8 0 1.4 1.4 0 012.8 0M16.5 15.2a1.4 1.4 0 11-2.8 0 1.4 1.4 0 012.8 0",
  settings:
    "M12 15.5a3.5 3.5 0 100-7 3.5 3.5 0 000 7zM19.4 15a1.7 1.7 0 00.3 1.9l.1.1a2 2 0 11-2.8 2.8l-.1-.1a1.7 1.7 0 00-1.9-.3 1.7 1.7 0 00-1 1.5V21a2 2 0 11-4 0v-.1a1.7 1.7 0 00-1.1-1.5 1.7 1.7 0 00-1.9.3l-.1.1a2 2 0 11-2.8-2.8l.1-.1a1.7 1.7 0 00.3-1.9 1.7 1.7 0 00-1.5-1H3a2 2 0 110-4h.1A1.7 1.7 0 004.6 9a1.7 1.7 0 00-.3-1.9l-.1-.1a2 2 0 112.8-2.8l.1.1a1.7 1.7 0 001.9.3H9a1.7 1.7 0 001-1.5V3a2 2 0 114 0v.1a1.7 1.7 0 001 1.5 1.7 1.7 0 001.9-.3l.1-.1a2 2 0 112.8 2.8l-.1.1a1.7 1.7 0 00-.3 1.9V9a1.7 1.7 0 001.5 1H21a2 2 0 110 4h-.1a1.7 1.7 0 00-1.5 1z",
  shuffle: "M16 3h5v5M4 20l17-17M21 16v5h-5M15 15l6 6M4 4l5 5",
  repeat: "M17 2l4 4-4 4M3 11V9a4 4 0 014-4h14M7 22l-4-4 4-4M21 13v2a4 4 0 01-4 4H3",
  "repeat-one": "M17 2l4 4-4 4M3 11V9a4 4 0 014-4h14M7 22l-4-4 4-4M21 13v2a4 4 0 01-4 4H3M12 10v4",
  speed: "M12 20a8 8 0 100-16 8 8 0 000 16zM12 12l4-3",
  mic: "M12 3a3 3 0 00-3 3v5a3 3 0 006 0V6a3 3 0 00-3-3zM5 11a7 7 0 0014 0M12 18v3",
  plus: "M12 5v14M5 12h14",
  trash: "M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13",
  close: "M6 6l12 12M18 6L6 18",
  folder: "M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z",
  refresh: "M3 12a9 9 0 0115-6.7L21 8M21 3v5h-5M21 12a9 9 0 01-15 6.7L3 16M3 21v-5h5",
  sun: "M12 4V2M12 22v-2M4 12H2M22 12h-2M5.6 5.6L4.2 4.2M19.8 19.8l-1.4-1.4M18.4 5.6l1.4-1.4M4.2 19.8l1.4-1.4M12 8a4 4 0 100 8 4 4 0 000-8z",
  moon: "M20 14.5A8.5 8.5 0 019.5 4 8.5 8.5 0 1020 14.5z",
  music: "M9 18V6l10-2v12M9 18a3 3 0 11-6 0 3 3 0 016 0zM19 16a3 3 0 11-6 0 3 3 0 016 0z",
  chevron: "M9 6l6 6-6 6",
  "chevron-down": "M6 9l6 6 6-6",
  minimize: "M6 12h12",
  pin: "M12 17v5M8 3h8l-1 6 3 3v2H6v-2l3-3z",
  plug: "M9 2v6M15 2v6M6 8h12v3a6 6 0 01-12 0zM12 17v5",
  user:
    "M12 22a10 10 0 100-20 10 10 0 000 20zM9 10.6a3 3 0 106 0 3 3 0 00-6 0zM6.2 19.7a6.6 6.6 0 0111.6 0",
  waveform: "M3 12h2M7 8v8M11 5v14M15 9v6M19 7v10M23 12h-2",
  /* 窗口控制（无框标题栏，Windows 风格） */
  "win-min": "M5 12h14",
  "win-max": "M6 6h12v12H6z",
  "win-restore": "M8.5 5.5h10v10h-10zM5.5 8.5v10h10M15.5 15.5v3",
  "win-close": "M6 6l12 12M18 6L6 18",
  /* 移动端参考布局用得到的图标（桌面组件不会引用，纯新增） */
  home:
    "M15 21v-7a1 1 0 00-1-1h-4a1 1 0 00-1 1v7M3 10.2a2 2 0 01.72-1.54l7-5.9a2 2 0 012.56 0l7 5.9A2 2 0 0121 10.2V19a2 2 0 01-2 2H5a2 2 0 01-2-2z",
  clock: "M12 21a9 9 0 100-18 9 9 0 000 18zM12 7v5l3 2",
  download: "M12 3v12M7 11l5 5 5-5M4 20h16",
  comment: "M4 5h16v11H9l-5 4z",
  more: "M6 12a1.1 1.1 0 100 2.2 1.1 1.1 0 000-2.2zM12 12a1.1 1.1 0 100 2.2 1.1 1.1 0 000-2.2zM18 12a1.1 1.1 0 100 2.2 1.1 1.1 0 000-2.2z",
  radio: "M12 13a2 2 0 100-4 2 2 0 000 4zM8 9a5.5 5.5 0 000 7M16 9a5.5 5.5 0 010 7M5 6a10 10 0 000 13M19 6a10 10 0 010 13",
  image: "M4 5h16a1 1 0 011 1v12a1 1 0 01-1 1H4a1 1 0 01-1-1V6a1 1 0 011-1zM3 15.5l4.5-4.5 3.5 3.5 4-4 6 6M15.6 9.4a1.1 1.1 0 100-2.2 1.1 1.1 0 000 2.2z",
  sequence: "M4 6h13M4 11h13M4 16h6M16.5 13.5L20.5 17l-4 3.5M20.5 17h-8.5",
  caption: "M4 6.5h16M9 12h11M4 17.5h8",
  lock: "M6 11h12a1 1 0 011 1v7a1 1 0 01-1 1H6a1 1 0 01-1-1v-7a1 1 0 011-1zM8.5 11V7.5a3.5 3.5 0 017 0V11M12 14.5v2",
  /* 实心暂停双条（移动端播放页大按钮专用，需 filled 渲染） */
  "pause-f":
    // 双条整体向右 0.5 单位，让图形几何中心落在 viewBox 的 12 上
    // （原坐标 6.8–16.2，中心 11.5，30px 图标下肉眼可见偏左）
    "M8.4 5h1a1.1 1.1 0 011.1 1.1v11.8a1.1 1.1 0 01-1.1 1.1h-1a1.1 1.1 0 01-1.1-1.1V6.1A1.1 1.1 0 018.4 5zm6.2 0h1a1.1 1.1 0 011.1 1.1v11.8a1.1 1.1 0 01-1.1 1.1h-1a1.1 1.1 0 01-1.1-1.1V6.1A1.1 1.1 0 0114.6 5z",
  /* 双闪星：设置页「智能音质」用（实心渲染，与参考稿的蓝色星形一致） */
  sparkle:
    "M11 3.2l1.6 4.4 4.4 1.6-4.4 1.6L11 15.2l-1.6-4.4L5 9.2l4.4-1.6zM18.2 14.6l.9 2.3 2.3.9-2.3.9-.9 2.3-.9-2.3-2.3-.9 2.3-.9z",
};

const path = computed(() => PATHS[props.name] ?? PATHS.music);
const sizePx = computed(() =>
  typeof props.size === "number" ? `${props.size}px` : props.size,
);
</script>

<template>
  <svg
    :width="sizePx"
    :height="sizePx"
    viewBox="0 0 24 24"
    :fill="filled ? 'currentColor' : 'none'"
    :stroke="filled ? 'none' : 'currentColor'"
    :stroke-width="stroke"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    class="icon"
  >
    <!-- 「词」图标：方框 + 汉字字形（桌面歌词专用，需 text 元素渲染汉字） -->
    <template v-if="name === 'lyrics'">
      <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
      <text
        x="12"
        y="12.6"
        text-anchor="middle"
        dominant-baseline="central"
        font-size="11.5"
        font-weight="600"
        font-family="'PingFang SC', 'Microsoft YaHei', system-ui, sans-serif"
        fill="currentColor"
        stroke="none"
      >
        词
      </text>
    </template>
    <path v-else :d="path" />
  </svg>
</template>

<style scoped>
.icon {
  display: block;
  flex: none;
}
</style>
