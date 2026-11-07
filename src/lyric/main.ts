import { createApp } from "vue";
import { createPinia } from "pinia";

import LyricWindow from "./LyricWindow.vue";

createApp(LyricWindow).use(createPinia()).mount("#lyric-root");
