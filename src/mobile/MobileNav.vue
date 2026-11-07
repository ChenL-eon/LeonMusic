<script setup lang="ts">
/** 悬浮胶囊导航：参考 KA Music 的 liquid-glass bottom bar。
 *  中间唱片兼任「当前播放」入口，不再有独立的迷你播放条。
 *  底栏只保留「首页 / 唱片 / 我的」三项：搜索由首页大搜索框进入，
 *  「我喜欢」由「我的 → 收藏」快捷卡进入，两个页面本身仍保留在 MobileShell。 */
import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";

defineProps<{
  active: string;
  progress: number;
  coverPath?: string | null;
  coverUrl?: string | null;
  isPlaying: boolean;
}>();

const emit = defineEmits<{
  (e: "navigate", page: "home" | "me"): void;
  (e: "open-player"): void;
}>();

const left = [{ id: "home", label: "首页", icon: "home" }] as const;
const right = [{ id: "me", label: "我的", icon: "user" }] as const;
</script>

<template>
  <nav class="nav">
    <button
      v-for="it in left"
      :key="it.id"
      class="item"
      :class="{ on: active === it.id }"
      @click="emit('navigate', it.id)"
    >
      <Icon :name="it.icon" :size="21" />
      <span>{{ it.label }}</span>
    </button>

    <button
      class="disc-wrap"
      :style="{ '--p': (progress * 100).toFixed(1) + '%' }"
      aria-label="打开播放页"
      @click="emit('open-player')"
    >
      <span class="ring" />
      <CoverArt
        :cover-path="coverPath"
        :cover-url="coverUrl"
        size="46px"
        radius="50%"
        :spinning="isPlaying"
        class="disc"
      />
    </button>

    <button
      v-for="it in right"
      :key="it.id"
      class="item"
      :class="{ on: active === it.id }"
      @click="emit('navigate', it.id)"
    >
      <Icon :name="it.icon" :size="21" />
      <span>{{ it.label }}</span>
    </button>
  </nav>
</template>

<style scoped>
.nav {
  position: absolute;
  left: 50%;
  bottom: calc(14px + var(--sab));
  transform: translateX(-50%);
  width: calc(100% - 40px);
  max-width: 300px;
  height: 62px;
  border-radius: 999px;
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  align-items: center;
  padding: 0 6px;
  box-sizing: border-box;
  background: color-mix(in srgb, var(--panel) 78%, transparent);
  border: 1px solid color-mix(in srgb, #fff 55%, var(--border));
  box-shadow: var(--m-shadow-float);
  backdrop-filter: blur(24px);
  -webkit-backdrop-filter: blur(24px);
  z-index: 40;
}
.item {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 3px;
  height: 56px;
  color: var(--text-3);
  font-size: 10px;
  line-height: 1.1;
  white-space: nowrap;
}
.item.on {
  color: var(--accent);
  font-weight: 600;
}
.item:active {
  transform: scale(0.93);
}
.disc-wrap {
  position: relative;
  display: grid;
  place-items: center;
  width: 54px;
  height: 54px;
  justify-self: center;
}
.ring {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  background: conic-gradient(var(--accent) var(--p), transparent var(--p));
  -webkit-mask: radial-gradient(farthest-side, transparent calc(100% - 3px), #000 calc(100% - 3px));
  mask: radial-gradient(farthest-side, transparent calc(100% - 3px), #000 calc(100% - 3px));
  opacity: 0.9;
}
.disc {
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.18);
}
.disc-wrap:active .disc {
  transform: scale(0.92);
}
</style>
