<script setup lang="ts">
/** 歌曲 VIP 标：按来源平台显示各自的样式。
 *  网易云（含「我喜欢的音乐」）→ 金色 VIP 胶囊；
 *  QQ 音乐（qq 插件，或 netease 插件里前缀 qq: 的外部歌单）→ 皇冠 VIP 标。
 *  只做展示，track.vip 为 false 时父级用 v-if 控制不渲染。 */
import { computed } from "vue";
import type { Track } from "@/types";

const props = defineProps<{ track: Track; small?: boolean }>();

const platform = computed<"qq" | "netease">(() => {
  const fromId = String(props.track.id || "").startsWith("qq:");
  const pluginId =
    props.track.source.kind === "plugin" ? props.track.source.pluginId : "";
  return pluginId === "qq" || fromId ? "qq" : "netease";
});

const label = computed(() =>
  platform.value === "qq" ? "QQ 音乐会员专享" : "网易云会员专享",
);
</script>

<template>
  <span
    class="vip-badge"
    :class="[platform, { small }]"
    :title="label"
    :aria-label="label"
  >
    <!-- QQ 音乐：皇冠 + VIP -->
    <template v-if="platform === 'qq'">
      <svg class="crown" viewBox="0 0 16 16" aria-hidden="true">
        <path d="M2 5l2.4 2L8 3l3.6 4L14 5l-1.2 7H3.2z" />
      </svg>
      <span>VIP</span>
    </template>
    <!-- 网易云：金色 VIP 胶囊 -->
    <template v-else>VIP</template>
  </span>
</template>

<style scoped>
.vip-badge {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  height: 14px;
  padding: 0 4px;
  border-radius: 4px;
  font-size: 9px;
  font-weight: 800;
  line-height: 1;
  letter-spacing: 0.2px;
  vertical-align: 1px;
  white-space: nowrap;
  flex: none;
}

.vip-badge.small {
  height: 12px;
  padding: 0 3px;
  font-size: 8px;
}

/* 网易云：金色渐变底 + 深褐字，接近网易云会员标的观感 */
.vip-badge.netease {
  color: #6b3b00;
  background: linear-gradient(135deg, #ffd76a, #ffb437);
  box-shadow: 0 0 0 0.5px rgba(180, 110, 0, 0.35) inset;
}

/* QQ 音乐：皇冠用青蓝渐变，文字沿用金色，和网易云明显区分 */
.vip-badge.qq {
  color: #fff;
  background: linear-gradient(135deg, #23c2c2, #1a8fe3);
  box-shadow: 0 0 0 0.5px rgba(0, 80, 130, 0.3) inset;
}

.crown {
  width: 9px;
  height: 9px;
  fill: #ffd76a;
}
</style>
