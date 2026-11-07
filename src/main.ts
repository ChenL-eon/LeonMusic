import { createApp } from "vue";
import { createPinia } from "pinia";

import App from "./App.vue";
import "./styles/main.css";

// 屏蔽浏览器默认右键菜单（Tauri 里没有「刷新」「另存为」的需求）
if (import.meta.env.PROD) {
  window.addEventListener("contextmenu", (e) => e.preventDefault());
}

/**
 * 系统栏高度入口，由 Android 原生侧调用：
 *
 *   WindowInsetsBridge.kt → webView.evaluateJavascript("window.__leonInsets(t, b)")
 *
 * CSS 的 `env(safe-area-inset-*)` 在 Chrome for Android 上只上报刘海，
 * 导航栏 / 手势条恒为 0（见 WindowInsetsBridge 的注释），所以底栏必须靠
 * 这条路拿到真实高度。函数返回 true 是给原生侧的「JS 已就绪」信号，
 * 没有它原生侧只能靠猜延迟重推。
 *
 * 桌面端没有调用方，变量保持 main.css 里的 env() 兜底值。
 */
declare global {
  interface Window {
    __leonInsets?: (top: number, bottom: number) => boolean;
  }
}

window.__leonInsets = (top: number, bottom: number): boolean => {
  const root = document.documentElement;
  root.style.setProperty("--sat", `${Math.max(0, top)}px`);
  root.style.setProperty("--sab", `${Math.max(0, bottom)}px`);
  return true;
};

createApp(App).use(createPinia()).mount("#app");
