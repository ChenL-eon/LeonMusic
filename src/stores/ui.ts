/** UI 状态：路由、抽屉、Toast。 */

import { defineStore } from "pinia";
import { ref } from "vue";

export type Page =
  | "home" // 首页（桌面与移动端共用：搜索 + 推荐内容）
  | "me" // 移动端「我的」（桌面不使用）
  | "now" // 播放详情：桌面是覆盖层（点击播放条歌曲进入），移动端是全屏覆盖层
  | "library"
  | "playlist"
  | "search" // 搜索结果：桌面是覆盖层（从首页搜索框进入），移动端仍是一级 tab
  | "favorites"
  | "account"
  | "settings";

/** 设置页的分页。放在 store 里，「去配置」才能从别的页面直达指定页签。 */
export type SettingsTab = "look" | "lyric" | "play" | "source" | "stream" | "about";

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error" | "success";
}

export const useUiStore = defineStore("ui", () => {
  const page = ref<Page>("home");
  const queueOpen = ref(false);
  const toasts = ref<Toast[]>([]);
  const lyricWindowOn = ref(false);
  const settingsTab = ref<SettingsTab>("look");
  let seq = 0;

  function go(p: Page) {
    page.value = p;
  }

  /** 跳到设置页，可指定落到哪个页签（例如「去配置音源」→ source）。 */
  function goSettings(tab?: SettingsTab) {
    if (tab) settingsTab.value = tab;
    page.value = "settings";
  }

  function toast(text: string, kind: Toast["kind"] = "info") {
    const id = ++seq;
    toasts.value.push({ id, text, kind });
    setTimeout(() => {
      toasts.value = toasts.value.filter((t) => t.id !== id);
    }, 3600);
  }

  function toggleQueue() {
    queueOpen.value = !queueOpen.value;
  }

  return {
    page,
    queueOpen,
    toasts,
    lyricWindowOn,
    settingsTab,
    go,
    goSettings,
    toast,
    toggleQueue,
  };
});
