# LeonMusic

一个**把音频核心完全放进 Rust** 的桌面 / 安卓音乐播放器。

前端只做三件事：画界面、收状态、发指令。解码、重采样、变速变调、队列、进度、
缓存、歌词解析全部在 Rust 里跑；WebView 被销毁也不会影响播放（Android 上尤其重要）。

技术栈：**Tauri v2 + Rust 2021 + Vue 3 + Vite + TypeScript + Pinia**，目标平台
**Windows / Android**（不做 iOS，也不复用任何 Electron 旧代码）。

---

## 1. 它长什么样

```
┌──────────────────────────────────────────────────────────────┐
│  Vue 3 (WebView)                                             │
│    views / components / stores  ← 只读镜像 PlayerSnapshot       │
│         │  invoke(player_play / seek / set_speed …)           │
│         │  listen(leon://player/state)                        │
└─────────┼────────────────────────────────────────────────────┘
          │ Tauri IPC
┌─────────▼────────────────────────────────────────────────────┐
│  Rust                                                        │
│  commands  ──►  EngineCommand（crossbeam channel）             │
│                        │                                     │
│                 ┌──────▼───────────────────────────┐          │
│                 │ engine thread (8ms tick)         │          │
│                 │  decode → WSOLA → resample → ring │          │
│                 └──────┬───────────────────────────┘          │
│                        │ rtrb (SPSC, lock-free)              │
│                 ┌──────▼───────────────────────────┐          │
│                 │ audio callback (OS thread)       │          │
│                 │  pop + 音量平滑 → 声卡            │          │
│                 └──────────────────────────────────┘          │
│                                                              │
│  ticker thread ──emit(120ms)──► 前端                           │
│                └─► SMTC(Windows) / MediaSession(Android)      │
└──────────────────────────────────────────────────────────────┘
```

三条铁律：

1. **Rust 是唯一可信的播放状态源**。前端 `PlayerSnapshot` 是只读镜像，
   连「当前播放到第几毫秒」都是推过来的；前端只用 `requestAnimationFrame`
   做显示层插值，不回写状态。
2. **音频输出流常驻**。整个进程只开一次 `cpal::Stream`，切歌只换解码源。
   这样 Android 不会反复申请 AudioTrack / 音频焦点，切歌听不出接缝。
3. **进度由 `played_frames` 推导**（设备真实消费的帧数），不是计时器。
   暂停、seek、变速都不会漂移。

---

## 2. 目录结构

```
LeonMusic/
├── index.html                  # 主窗口入口
├── lyric.html                  # 桌面悬浮歌词窗（独立 WebView）
├── vite.config.ts              # 双入口 + 端口 1420
├── scripts/gen-icon.py         # 生成 1024 图标（PIL）
│
├── src/                        # ---- 前端（只做 UI）----
│   ├── api/ipc.ts              # 唯一 IPC 出口：命令 + 事件订阅
│   ├── types/index.ts          # 与 Rust models.rs 一一对应
│   ├── stores/
│   │   ├── player.ts           # 只读镜像 + rAF 显示插值 + 歌词行定位
│   │   ├── library.ts / settings.ts / ui.ts
│   ├── views/                  # NowPlaying / Library / Search / Account / Settings / Playlist / Favorites
│   ├── components/             # PlayerBar / TrackList / LyricView / Slider / CoverArt / QueueDrawer …
│   ├── lyric/                  # 悬浮歌词窗（独立入口）
│   └── styles/main.css         # CSS 变量设计系统（暗色 / 亮色 / 自定义配色）
│
├── src-tauri/                  # ---- 后端（全部核心逻辑）----
│   ├── migrations/001_init.sql # 8 张表（tracks/playlists/history/cache/plugins/lyrics…）
│   └── src/
│       ├── lib.rs              # 启动装配 + 命令注册
│       ├── models.rs           # 跨端数据契约（camelCase）
│       ├── error.rs            # AppError → {code, message}
│       ├── events.rs           # 事件名常量
│       ├── paths.rs            # data / cache / covers / plugins
│       ├── state.rs            # AppState（不持有播放状态）
│       ├── audio/
│       │   ├── decoder.rs      # symphonia + opus-pure 统一 DecodeSource
│       │   ├── dsp.rs          # WSOLA 变速变调 + Catmull-Rom 重采样
│       │   ├── sink.rs         # AudioOutput trait + 实时安全 render
│       │   ├── sink_cpal.rs    # Windows: WASAPI
│       │   ├── sink_android.rs # Android: Oboe → AAudio（48k/2ch 固定）
│       │   ├── queue.rs        # 队列 / 循环 / 随机（含单测）
│       │   └── engine.rs       # 引擎线程 + 共享原子状态 + 状态推送
│       ├── library/            # 扫描 / 元数据 / 封面 / LRC（逐字）解析
│       ├── net/                # reqwest 客户端 + 整曲缓存 + LRU 淘汰
│       ├── plugin/             # 音源插件协议 + boa JS 沙箱 + 4 个内置插件
│       ├── platform/
│       │   ├── mod.rs          # PlatformHost trait（唯一的平台分叉点）
│       │   ├── windows/        # 托盘 / 全局快捷键 / SMTC / 悬浮歌词窗
│       │   ├── android/        # JNI 桥（Kotlin 侧在 android-overlay/）
│       │   └── fallback.rs     # 非 Windows/Android 的兜底（CI 用）
│       ├── db/                 # rusqlite（WAL）+ 全部 CRUD
│       └── commands/           # Tauri 命令（player/library/playlist/plugin/settings）
│
└── android-overlay/            # ---- Android 原生层（Kotlin）----
    ├── apply.py                # 把覆盖层合并进 gen/android
    ├── app/src/main/java/com/leonmusic/player/
    │   ├── MainActivity.kt     # 权限申请 + 桥接 attach + 电池白名单引导
    │   ├── LeonBridge.kt       # Rust ↔ Kotlin 唯一通道
    │   ├── PlaybackService.kt  # 前台服务 + MediaSession + MediaStyle 通知
    │   ├── AudioFocusHelper.kt # 焦点四种情况的处理
    │   ├── Permissions.kt
    │   └── BatteryOptimization.kt
    └── manifest/               # 权限与 service 片段（脚本合并）
```

---

## 3. 桌面（Windows）

### 3.1 环境

| 依赖 | 版本 | 备注 |
|---|---|---|
| Node.js | ≥ 20 | |
| Rust | ≥ 1.77（实测 1.9x） | `rustup default stable` |
| MSVC 生成工具 | VS 2022 Build Tools | 勾选「使用 C++ 的桌面开发」 |
| WebView2 | Win10+ 自带 | Win7 需单独装 |

> **没有 C++ 依赖需要手装**：Opus 用纯 Rust 的 `opus-pure`，
> 变速变调是自研 WSOLA，SQLite 用 `rusqlite/bundled` 编译进去。

### 3.2 开发

```bash
npm install
npm run desktop:dev          # = tauri dev，热重载
```

只想调前端（不启 Rust）：

```bash
npm run dev                  # vite，端口 1420
```

### 3.3 打包

```bash
npm run desktop:build        # = tauri build
```

产物：

```
src-tauri/target/release/bundle/nsis/LeonMusic_0.1.0_x64-setup.exe
src-tauri/target/release/bundle/msi/LeonMusic_0.1.0_x64_en-US.msi
```

首次打包会自动下载 NSIS / WiX，需要网络。只想验证编译：

```bash
npm run build:only                          # 前端
cargo build --release --manifest-path src-tauri/Cargo.toml --features custom-protocol
```

> **注意：裸 `cargo build --release` 编出来的 exe 是「dev 版」**——
> Tauri 的 dev/prod 判定是 `dev = !custom_protocol`（tauri crate build.rs），
> 不带 `custom-protocol` feature 时 release 产物也会把 devUrl
> （`http://localhost:1420`）编进 exe，WebView 启动就直连 vite dev server；
> server 不在的话窗口直接「localhost 拒绝连接」白屏。2026-11 实测踩坑：
> 之前交付的 exe 一直「能跑」是因为机器上恰好有遗留的 dev server 撑着。
> 现在两种等价做法：
>
> ```bash
> # 方式一：裸 cargo（已把 custom-protocol 写进 Cargo.toml 的 [features]，
> #         构建命令必须显式带上）
> cargo build --release --features custom-protocol
>
> # 方式二：走 Tauri CLI（CLI 会自动传 feature；要 NSIS/WiX 安装包时用）
> npx tauri build --no-bundle
> # 产物 src-tauri/target/release/leonmusic.exe（约 15MB，strip + LTO）
> ```
>
> 这个 exe 自包含前端资源（`https://tauri.localhost/`，useHttpsScheme
> 规避系统代理对 `tauri.localhost` 的劫持），单文件拷走即可当
> 便携版用；运行时只依赖系统自带的 WebView2 Runtime，无需安装。

---

## 4. Android

### 4.1 一次性准备

```bash
npm run android:init         # 生成 src-tauri/gen/android（生成物，可删可重建）
python android-overlay/apply.py
```

`apply.py` 做的事（全部幂等）：

1. 把 `android-overlay/app/src/main/java/**` 同步到生成工程（已存在的先备份 `.orig`）
2. 把播放相关权限插进 `AndroidManifest.xml`
3. 插入 `PlaybackService` 声明（`foregroundServiceType="mediaPlayback"`）
4. 给 `<application>` 补 `android:usesCleartextTraffic="true"`
5. 给 `app/build.gradle.kts` 加 `androidx.media:media:1.7.0`
6. 把 `minSdk` 抬到 **26**（见下方「为什么必须 26」）
7. 存在 `android-overlay/keystore.properties` 时注入 release 签名配置（见 4.3）

为什么要有这一层：`gen/android` 是 `tauri android init` 的生成物，删了重跑就没了。
把 Kotlin 代码放在 `android-overlay/` 才能真正进版本控制。

**为什么必须 minSdk 26**：cpal 的 Android 后端是 Oboe → AAudio，
而 NDK 里 `libaaudio.so` 从 API 26 才开始提供。留在 24 会在链接阶段直接失败：

```
ld.lld: error: unable to find library -laaudio
clang: error: linker command failed with exit code 1
```

`tauri.conf.json` 的 `bundle.android.minSdkVersion` 只在 `android init` 时读一次，
生成物已存在时不会跟着变，所以 `apply.py` 里做了兜底纠正。

### 4.2 跑起来

```bash
npm run android:dev          # 需要先连真机或开模拟器
```

要求：JDK 17+（实测 21 可用）、Android SDK Platform 34+、NDK 26+、`ANDROID_HOME` 已设置。
四个 target 的 Rust 标准库要装好：

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi \
                  i686-linux-android x86_64-linux-android
```

### 4.3 签名（正式包必须）

签名信息放在 `android-overlay/`，由 `apply.py` 注入生成工程——这样 `android init`
重建后不会丢。三份文件各司其职：

| 文件 | 进版本库 | 作用 |
|---|---|---|
| `android-overlay/leonmusic-release.jks` | ❌ `.gitignore` 已排除 | 私钥 |
| `android-overlay/keystore.properties` | ❌ 同上 | 口令与别名 |
| `android-overlay/keystore.properties.example` | ✅ | 格式模板 |

**第一步：生成 keystore（一次就好，务必备份）**

```bash
keytool -genkeypair -v -keystore android-overlay/leonmusic-release.jks \
  -alias leonmusic -keyalg RSA -keysize 2048 -validity 10000 \
  -storepass <口令> -keypass <口令> \
  -dname "CN=LeonMusic, OU=Dev, O=LeonMusic, L=Shanghai, ST=Shanghai, C=CN"
```

> PKCS12 要求 `storepass` 与 `keypass` 一致，不一致 keytool 会直接拒绝。

**第二步：写 `android-overlay/keystore.properties`**

```properties
storeFile=leonmusic-release.jks
storePassword=<口令>
keyAlias=leonmusic
keyPassword=<口令>
```

`storeFile` 相对 `gen/android/`；同目录的 `.jks` 会被 `apply.py` 一起复制过去。

**第三步：注入并打包**

```bash
python android-overlay/apply.py    # 注入 signingConfigs
npm run android:build              # APK
npm run android:build:aab          # AAB（上架 Google Play 用）
```

产物：`src-tauri/gen/android/app/build/outputs/apk/universal/release/`。

验证配置是否真的生效（只解析 Gradle，不编译 Rust）：

```bash
cd src-tauri/gen/android && ./gradlew :app:signingReport -q | grep -A2 Release
```

> 同一个包名 + 同一份签名才能覆盖安装。换机器一定要把 `.jks` 拷过去，
> 丢了就只能换包名重新上架。

**踩过的坑**：`build.gradle.kts` 里不要写 `java.util.Properties()`。
Kotlin DSL 中 `java` 会先命中 Project 的 `java {}` 扩展，`java.util` 就被解析成
「在 `Action<JavaPluginExtension>` 上找 `util`」，报一串
`Unresolved reference 'util' / 'getProperty'`。文件顶部本来就有
`import java.util.Properties`，直接写 `Properties()` 即可。

### 4.4 为什么必须有前台服务

Tauri 的 Activity / WebView 会被系统回收（切后台、内存压力、分屏）。
播放**不能**依赖 WebView 存活，所以：

- 音频引擎在 Rust 进程的独立线程里
- `PlaybackService` 用 `foregroundServiceType="mediaPlayback"` 把进程钉在前台
- 通知 / 锁屏 / 耳机线控 → `MediaSession` → `LeonBridge.nativeCommand()` → Rust
- WebView 重建后从 Rust 拉一次 `PlayerSnapshot` 即可恢复 UI

`onTaskRemoved` 里**不会**停服务：用户从最近任务划掉应用，只要还在播就继续播。
Rust 推 `clear`（当前没有曲目）时服务只退出前台、**不自毁**——
自毁会让冷启动时刚起来的服务被杀，MediaSession 跟着闪断（细节见 6.1）。

服务生命周期只有两种情况会结束：从最近任务划掉且没在播，或系统回收。

### 4.5 权限与后台限制

| 权限 | 用途 | 被拒的后果 |
|---|---|---|
| `READ_MEDIA_AUDIO`（33+） | 读本地音乐 | 无法扫描本地文件，在线音源不受影响 |
| `READ_EXTERNAL_STORAGE`（≤32） | 同上 | 同上 |
| `POST_NOTIFICATIONS`（33+） | 通知栏控制 | 播放正常，但没有通知/锁屏按钮 |
| `FOREGROUND_SERVICE_MEDIA_PLAYBACK` | 前台服务类型 | Android 14 起缺失会直接崩 |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | 电池白名单 | 熄屏久了可能被冻结 |

电池白名单引导**只在首次启动弹一次**（`SharedPreferences` 记标记），
因为 Google Play 要求这类敏感权限必须由用户主动触发。

### 4.6 手机上的布局是怎么做的

同一套 Vue 代码同时跑桌面和手机，靠一个断点（**860px**）在 `App.vue` 里分流到
两套外壳，不是「把桌面缩小」：桌面仍是 `Sidebar + 播放条` 的侧栏布局；手机走
`src/mobile/MobileShell.vue`，布局参考 [KA Music](https://github.com/umr-xiaomai/kgka_Music_hl)
（Flutter / Material You）。

| | 桌面 | 手机（≤860px） |
|---|---|---|
| 导航 | 左侧栏 4 项：首页 / 本地音乐 / 我喜欢的 / 账号 | 悬浮胶囊三项：首页 / 中间唱片 / 我的（300px 宽、3 列 grid） |
| 默认页 | 首页（搜索框 + 推荐歌单 + 每日推荐歌曲） | 首页（卡片流） |
| 当前播放 | 底部播放条，点左侧歌曲打开**全屏覆盖层**（右上角收起） | 胶囊中央旋转唱片（带进度环），点开是全屏覆盖播放页 |
| 在线搜索 | **首页内联搜索**：搜索框输入（防抖 400ms）直接在下方出结果，清空即回推荐态；不再有独立搜索页/覆盖层 | 底栏没有搜索，从首页大搜索框进入 |
| 一级页面 | 首页 / 本地音乐 / 我喜欢的 / 账号；播放详情是覆盖层 | 首页、我的；搜索 / 我喜欢不在底栏，分别从首页大搜索框、「我的 → 收藏」进入；其余收成二级页 |
| 播放详情 | 封面左、歌词右（覆盖层化，`ui.page === 'now'` 只是开关信号，`lastPage` 记录来源页） | 全屏覆盖层：封面模糊背景 + 封面/歌词分页 + 5 键控制栏，下滑关闭 |
| 队列 | 右侧抽屉 | 底部 sheet；`.mobile-shell` 用 `isolation: isolate` 建独立层叠上下文，保证抽屉盖在全屏播放页之上 |

底栏只留三项（2026-11-06 调整）：`MobileNav.vue` 移除「搜索」「我喜欢」两个按钮，
`grid-template-columns` 由 5 列改 3 列、胶囊 `max-width` 360 → 300px。两个页面本身
**没有删**，仍在 `MobileShell` 的 `TABS` 里，靠 `navActive` 把搜索映射成高亮「首页」、
我喜欢映射成高亮「我的」，底栏不会失去选中态；`favoritesReturn` 记录来源分区，
于是「我的 → 收藏 → 返回」回到「我的」而不是首页。

`mobile.css` 定义圆角 12–28 / 柔和阴影 / 彩色图标块等 token；主色仍跟随用户在设置里
选的 `--accent`，不覆盖主题设置。类名统一 `m-` 前缀且桌面组件不引用，所以桌面布局零改动。
`MobileHome` / `MobileMe` / `MobileSearch` / `MobilePlayer` / `MobileListPage` /
`MobileTrackRow` / `MobileNav` 都只读现有 store，没有新增 IPC。

二级页（本地音乐 / 歌单 / 账号 / 设置 / 最近 / 已缓存）复用桌面视图，套一层统一返回头；
`ui.go('now')` 在移动端被翻译成「打开播放覆盖层并留在当前 tab」。初始化时 ui store 的
默认页 `now` 先回首页，避免一启动就弹播放页。

断点取 860 而不是常见的 768：桌面窗口 `minWidth` 是 940，不会误伤；
手机横屏（844×390）和折叠屏内屏也要走移动布局，760 会漏掉。

**浏览器走查**：`build/mock_tauri.js` 是 Tauri IPC 替身。用无头 Chrome 打开 `dist/`，
通过 CDP 的 `Page.addScriptToEvaluateOnNewDocument` 注入后 reload，再把视口设成
412×915 即可逐页截图，不需要真机或正在运行的 Tauri 实例。截图见 `build/mobile-*.png`。

#### 安全区：为什么 CSS 的 `env()` 不够

Activity 调了 `enableEdgeToEdge()`，WebView 铺满整块屏幕，状态栏和手势条
直接压在内容上。CSS 有 `env(safe-area-inset-*)`，但**Chrome for Android
只上报刘海（display cutout），导航栏/手势条恒为 0**：

```
# 模拟器，navigation_mode=2（手势导航）
env(safe-area-inset-bottom) = 0px     ← 手势条实际高 24px
```

真机反而报 20px（厂商 WebView 的额外扩展），所以不能依赖。做法是双保险：

1. **原生注入**（主力）：`WindowInsetsBridge.kt` 监听 `WindowInsets`，
   换算成 CSS px 后通过 `evaluateJavascript` 写进 `--sat` / `--sab`：
   ```js
   window.__leonInsets(49.45, 24)   // 返回 true 表示 JS 已就绪
   ```
   页面是异步加载的，原生侧用**有限次重试**（250ms × 24）而不是猜一个延迟——
   猜短了白推一次，猜长了底部会先闪一下没让开的样子。
2. **CSS 兜底**：`--sat: env(safe-area-inset-top, 0px)`，iOS/旧环境仍可用。

`.shell` 用 `padding-top: var(--sat); padding-bottom: var(--sab)` 而不是把行撑高，
这样背景（body 的渐变）依然铺到状态栏下面，看起来是沉浸式但内容不钻进去。

#### 状态栏图标

深色主题下状态栏图标也得变浅。`enableEdgeToEdge` 之后图标颜色由系统控制，
CSS 管不着，所以切主题时要过一遍原生：

```
settings.applyTheme() → app_set_dark_appearance(dark)
  → PlatformHost::set_dark_appearance → LeonBridge.dispatch("theme")
  → WindowInsetsBridge.setSystemBarAppearance → isAppearanceLightStatusBars = !dark
```

不做这一步的表现是「深色背景 + 深色图标」，等于看不见。

#### 触摸端的几处必要偏离

- **列表单击播放**：原来靠 `@dblclick`，手机上根本点不动。改成单击播放
  （`.row` 上本来就没有其他点击语义，桌面也一样用）。
- **输入框字号 ≥16px**：Android WebView 在聚焦字号小于 16px 的输入框时
  会自动放大整个页面且不缩回，底部播放条会被顶出屏幕。
- **`@media (hover: hover)` 门控**：触摸设备 tap 一次会让 `:hover` 粘住，
  整行一直亮着像被选中了。
- **`-webkit-tap-highlight-color: transparent`**：去掉安卓点击时的青色方块，
  改用 `:active` 的缩放/透明度反馈。

---

## 5. 音源插件

### 5.1 协议（API v1）

内置四个：`netease`（网易云，开箱即用、支持扫码/验证码/Cookie 登录与受限歌曲解析）、`navidrome`、`jellyfin`、`emby`。
插件目录：`<appData>/plugins/<id>/{manifest.json, index.js}`。

`manifest.json`：

```json
{
  "id": "navidrome",
  "name": "Navidrome",
  "version": "1.0.0",
  "api": 1,
  "main": "index.js",
  "capabilities": ["search", "stream", "lyric"],
  "config": [
    { "key": "baseUrl", "label": "服务器地址", "fieldType": "string", "required": true },
    { "key": "password", "label": "密码", "fieldType": "password", "required": true }
  ]
}
```

`index.js` 需要定义全局函数，宿主按名字调用：

```js
function search(keyword, page, limit) {
  // 必须返回 [{ id, title, artist, album, durationMs, coverUrl }]
}

function getStreamUrl(track) {
  // track: { id, title, artist, album, durationMs, coverUrl }
  // 返回可直接 GET 的音频地址
}

function getLyric(track) { return "[00:01.00]..."; }   // 可选
```

宿主注入 `leon`：

| API | 说明 |
|---|---|
| `leon.config.get(key)` | 读配置（字符串） |
| `leon.http.get(url)` | 同步 GET，返回文本 |
| `leon.http.post(url, body)` | 同步 POST（JSON） |
| `leon.http.postForm(url, body, headers?)` | 同步 POST（form-urlencoded），第三个参数是普通对象 |
| `leon.http.postFormEx(url, body, headers?)` | 同上，但返回 `{ status, headers, cookies, body }`（登录要读 `Set-Cookie`） |
| `leon.crypto.aesCbcB64(text, key, iv)` | AES-128-CBC/PKCS7 → base64（key/iv 必须 16 字节） |
| `leon.crypto.aesEcbHex(text, key)` | AES-128-ECB/PKCS7 → hex |
| `leon.crypto.rsaNoPadHex(text, pem)` | RSA 裸模幂（无填充）→ hex，左补零到模长 |
| `leon.crypto.randomB64(n)` | n 字节随机数 → base64 |
| `leon.md5(s)` / `leon.sha1(s)` / `leon.base64(s)` | 摘要 |
| `leon.log(msg)` | 打日志 |

**鉴权**：`leon.http.postForm` 可以带自定义请求头，其他接口仍只支持 query。
Jellyfin / Emby / Subsonic 的 token 继续放 query（`?api_key=…`、`?u=..&t=..&s=..`），
它们都原生支持；网易云则必须带头（`Referer` / `Cookie`），所以单独开了 `postForm`。

**为什么加密原语在宿主侧**：沙箱里没有 Node 的 `crypto`，boa 也不提供。
把 AES/RSA 做成 `leon.crypto.*`，插件只写协议、不写算法——
既避免了在 JS 里手搓大数运算，也让 Android 与桌面共用同一份实现。
这四个原语的输出有单测和 Node `crypto` 的结果逐字节对齐。

**沙箱边界**：没有 `require` / `fs` / `process`；单次调用有指令数上限
（2000 万条）与递归深度上限（400），`while(true)` 会被掐断而不是卡死引擎；
每次调用新建一个 `boa` Context（因为 `Context` 不是 `Send`），跑在
`spawn_blocking` 线程里，不阻塞音频线程。

### 5.2 内置网易云是怎么接的

**不需要自建任何服务**：插件直接打网易云 web 端接口，加密方案与
[NeteaseCloudMusicApi](https://github.com/Binaryify/NeteaseCloudMusicApi)
（也就是 VutronMusic 内置的那个）一致。

| 能力 | 接口 | 加密 |
|---|---|---|
| 搜索 | `POST /weapi/cloudsearch/pc` | weapi |
| 播放直链 | `POST /eapi/song/enhance/player/url/v1` | eapi |
| 歌词（含翻译） | `POST /weapi/song/lyric` | weapi |

两种加密的差别：

```
weapi：JSON → AES-128-CBC(presetKey, iv) → AES-128-CBC(随机 16 字符 secretKey, iv)
       → base64；secretKey 反转后左补零到 128 字节，做 RSA 裸模幂 → encSecKey(hex)

eapi：  "nobody" + path + "use" + JSON + "md5forencrypt" 取 md5
       → "path-36cd479b6b5-JSON-36cd479b6b5-md5" → AES-128-ECB(固定 key) → hex
```

`secretKey` 是每次请求随机的，所以同一份参数每次密文都不同——这也是
为什么单测只能测原语（固定 key/iv/明文），不能测整个 weapi 请求。

**音质**（插件 2.4.0 起；2.5.0 改为单一决策链 + 真实规格回显）：设置 → 播放 →「在线音质」。
**选一个档位会同时写 `level`（官方请求档位）与 `unblockQuality`（解析码率）**，改完无需重启，
**从下一首开始生效**（当前曲目的直链已经取过了）。播放页用单个下拉框列出六档（低 → 高，
Hi-Res / 高清臻音为 disabled 项），保存失败时控件回退到实际生效档位；原先的六宫格卡片已移除。

| 档位 | `level` | 解析 `br` | 实际能拿到什么 |
|---|---|---|---|
| 标准 | `standard` | 128 | 128 kbps MP3（官方） |
| 较高 | `higher` | 192 | 192 kbps MP3 |
| 极高（默认） | `exhigh` | 320 | 320 kbps MP3 |
| 无损 | `lossless` | 999 | 官方只给试听时经同平台解析拿 FLAC 16bit/44.1kHz 全曲；官方已给全曲时可能被静默降成 320k MP3 |
| Hi-Res / 高清臻音 | `hires` / `jymaster` | — | 当前链路无法保证，UI 禁止选择 |

**不确保有效**（对标 AlgerMusicPlayer 的设置文案）：官方 eapi 对 `lossless/hires/jymaster`
一律 `code 200` 静默返回 320k MP3；老接口 `song/url` 的 `br=999000` 同样降 320k
（VutronMusic 的 `songUrl` 固定这个值，而它的 `musicQuality` 设置其实没接进播放链路）。
实测同平台解析 `br=999` 对 VIP 曲真下发无损：《渡口》18.6 MB / 《海阔天空》29.9 MB，
Range 取头解出 `fLaC` STREAMINFO = 44.1kHz / 2ch / 16bit、227.3s / 255.5s 全曲；
`br=320` 是 ID3 MP3。Alger 的 `/song/music/detail`、`/song/download/url/v1` 在本环境直连 404，
不能照抄。本机没有会员凭据，因此不承诺会员走官方接口一定能拿到 FLAC。

真实规格的透传（2.5.0）：

- `getStreamUrl` 返回 `{url, source, bitrate, format, trial, requested, downgraded}`，
  宿主 `host.rs::stream_url` **仍兼容旧插件的字符串返回**（按「规格未知」处理）。
- 规格经 `engine.shared().set_quality()` 进 `PlayerSnapshot.quality`（带 `trackId`，切歌自动失效，
  缓存命中重复播放仍显示）。播放条常驻显示「FLAC 无损 · 解析」/「320k MP3 · 官方」，
  降级用琥珀色标签并弹一次提示；悬浮说明含所选档位、来源和 48 kHz 输出上限。
- `qualityFromOfficial()` 同时识别「逐档降级」与「官方静默忽略 level」——后者以前完全看不出来。
- 高级折叠区里的「受限歌曲解析音质」只用于临时手动覆盖，下次换档会重新自动同步。
  `level` / `unblockQuality` / `cookie` 都不再暴露为插件页的自由文本输入框。

`LEVELS` 从高到低为 `['jymaster', 'hires', 'lossless', 'exhigh', 'higher', 'standard']`；
`levelFallback(want)` 只从选中档**向下**逐档重试——选了「标准」不会再白请求一次高清臻音，
配置里出现旧版本或非法值则回落 `exhigh`。匿名用户遇到 `fee=1` 的曲目会被服务端降到
128k 试听（`code:200` 但 `br` 很低）；整档拿不到时返回 `url: null`，插件自动降一档重试，
全空才报错。Hi-Res / 高清臻音不可保证：解析源最高 44.1kHz / 16bit，
`audio/engine.rs::OUTPUT_RATE` 固定 48 kHz 且重采样器无抗混叠低通，高采样率与 24bit
位深无法原样呈现。沉浸环绕声（`sky`）/ 杜比全景声（`dolby`）需要专门解码设备，暂不列入。

**歌词翻译**：`/weapi/song/lyric` 的 `tlyric` 是独立的一份同时间轴 LRC。
插件把它直接接在原文后面，宿主解析时把「时间戳相同的紧邻两行」合成
`text` + `translation`（文本完全相同的不合并，避免把合唱段落的重复原文吃掉）。

**封面必须转 https**：接口返回的 `picUrl` 是 `http://p3.music.126.net/...`，
WebView 在 Android 的明文流量策略下会拒绝加载，插件统一改写协议头。
音频直链仍是 http，但下载走 Rust 的 reqwest（自有 socket），不受该策略约束。

**扫码登录**（插件 2.1.0 起，2.3.0 补风控处理）：桌面侧栏 / 手机「我的」→「账号」→「扫码登录」。
登录相关 UI 全部收在独立的 `Account.vue`（账号卡片 + 扫码弹层 + Cookie 弹层 + 风控说明），
设置 → 音源插件 只保留一个「前往账号页」跳转条和插件参数，不再暴露
`cookie` / `level` / `unblockQuality` 这些原始输入框（音质改在「播放」页）。
manifest 里声明 `capabilities: ["login"]` 才会出现这一块，宿主按名字调用
`loginQrCreate` / `loginQrPoll` / `loginInfo`，插件脚本只管拼协议：

| 接口 | 作用 |
|---|---|
| `POST /weapi/login/qrcode/unikey` | 拿 `unikey`，二维码内容 = `https://music.163.com/login?codekey=<unikey>` |
| `GET https://music.163.com/`（`getEx`） | 预热浏览器会话，收集 `_ntes_nuid` / `NMTID` 等 `Set-Cookie` |
| `POST /weapi/login/qrcode/client/login` | 每 3 秒轮询：`801` 等待扫码 / `802` 已扫待确认 / `803` 成功 / `800` 过期 / `8821` 风控拦截 |
| `POST /weapi/nuser/account/get` | 昵称、头像、`vipType`（11 = 黑胶 VIP） |

二维码由宿主用 `qrcode` crate 渲染成 SVG（强制白底黑块，跟随主题反色会让相机扫不出来），
前端不需要引二维码库。`803` 时响应体只有一句「授权登陆成功」，
`MUSIC_U` / `__csrf` 等凭据全在**多条** `Set-Cookie` 响应头里，
所以这一步必须走 `leon.http.postFormEx`——宿主从 `HeaderMap::get_all("set-cookie")`
逐条解析成 `{ name: value }`，少了任何一条都会让登录功亏一篑。

`state: success` 时宿主把 cookie 合并进插件配置的 `cookie` 项。
每次 `call()` 都从 SQLite 重新读配置，插件的 `cookie()` / `csrfToken()`
下一帧就用上新凭据，不需要重启或重新加载插件；退出登录只删这一项。
「账号」页会显示昵称、头像和会员状态，未登录时给出「扫码登录」「验证码登录」
与「用 Cookie 登录」；扫码被风控时弹层直接显示原因并引导改用其它方式。

**短信验证码登录**（插件 2.8.0 起）：同一弹层体系的第三种登录方式，
参数形态对齐 `NeteaseCloudMusicApiEnhanced/api-enhanced` 的
`captcha_sent` / `login_cellphone` 实现，直连网易官方 weapi——
**手机号与验证码只发往 `music.163.com` 本身，不经过任何第三方服务器**，
本软件也不存储手机号（只在本机 SQLite 存登录后的 cookie）。流程：

| 接口 | 作用 |
|---|---|
| `POST /weapi/sms/captcha/sent` | 发码：`{ ctcode:'86', secrete:'music_middleuser_pclogin', cellphone }`；成功后进入 60s 重发倒计时 |
| `POST /weapi/w/login/cellphone` | 登录：`{ type:'1', https:'true', phone, countrycode:'86', captcha, remember:'true', secureCaptcha:'' }`（新版字段名是 `phone` 不是 `cellphone`） |
| `POST /weapi/nuser/account/get` | 登录成功后刷新账号卡片 |

发码时收集一次浏览器会话指纹（session），登录时原样带回，保证
「发码 → 校验」在服务端看来是同一设备（与扫码的 session 同构）。
成功凭据同样在 `Set-Cookie` 响应头里，与扫码共用同一条宿主落库路径。
实测错验证码返回 `code 503 + message「验证码错误」`，弹层原样透出；
验证码 10 分钟内有效。离线协议形状锁 + 真机探针（错码登录，不发短信）
在 `js_runtime.rs` 的 `netease_captcha_*` 测试里。

> **登录 ≠ 一定能听全曲**：只有会员账号、且曲目在该账号权限内才下发完整时长。
> 2026-11 真机实测（fee=1 曲目，150418）：匿名下发 **45 秒**试听
> （`freeTrialInfo {end:45}`），**普通账号登录后反而只有 20 秒**
> （`freeTrialInfo {fragmentType:1018, end:20}`，standard 档 128k MP3）——
> 试听权益绑定**账号是否会员**，不是「登录没登录」。播放器 store 的试听提示
> 因此按登录态分流（`vipTrialHint`）：未登录建议登录会员账号；已登录无会员
> 直接说明「登录了也还是试听」。数字专辑需单独购买，下架曲 `code:404` 谁都不行。
> 另：受限歌曲解析走的是第三方服务（GD Studio），服务本身也可能 503 宕机
> 导致「未命中」，与本机设置无关。
> 凭据落在本机 SQLite、**没有本地过期时间，也不会自动续期**：重启、升级都还在，
> 只有点「退出登录」、清除应用数据或换设备才会丢。真正决定能撑多久的是服务端：
> `MUSIC_U` 是长效 token（验证码/扫码登录下发的也是有效凭据，仅形态较长），
> 更常见的失效是**被吊销**——改密码、手机 App 退出登录、异地登录、账号风控。
> 掉线后官方接口退回试听片段，但默认开启的受限歌曲解析仍可能拿到全曲，
> 所以播放未必中断；应用不会主动弹提示，进「账号」页刷新一次才看得出未登录。
> 恢复方式：重新粘贴浏览器 Cookie（或再试扫码）；手填与扫码写的是同一个配置字段。

#### 扫码被风控拦截怎么办（插件 2.3.0 起）

手机 App 扫完弹出「**检测到当前设备环境异常，本次操作已拦截**」、
或服务端在 `802` 之后回 `8821`（请切换其他登录方式或升级新版本再试），
是网易云对**第三方客户端扫码登录的账号级风控**，不是二维码生成失败、
也不是本地解析或网络故障。同类项目（NeteaseCloudMusicApi 系列、
Obsidian / HA 音乐插件）都有相同记录：判定依据是客户端指纹与账号历史行为，
换 UA、换协议、换 `type`、加设备指纹参数都绕不过；`8821` 是**签发
`MUSIC_U` 这一步被拒**，而扫码、确认链路其实是通的。

插件侧能做的三件事（2.3.0 已做）：

1. **建码前先访问首页**，把 `_ntes_nuid` / `NMTID` / `__csrf` 等会话 Cookie
   从建码一直串到轮询（`loginQrCreate` 返回 `session`，前端轮询原样带回）；
   旧实现每次都是没有指纹的裸请求，最容易被判异常设备。
2. **不吞错误码**：`8821`、含「设备环境异常」的消息、以及其它未知码
   统一映射成 `state: blocked`，前端停止轮询并显示原因，不再永远停在
   「打开手机 App 扫一扫」。
3. **降低风控面**：UA 去掉 `LeonMusic/0.1` 这类第三方后缀，`deviceId`
   改成合规的 32 位十六进制；轮询 2s → 3s；二维码 4 分钟内复用，
   反复开关弹层不再重复建码。

用户侧仍可能触发风控（官方给的四条建议）：切换网络或更换设备、
移除手机上的越狱 / 解锁 / 外挂类插件、把网易云 App 升到最新版、
不要频繁操作并等待 24 小时。

被拦时的**可靠路径**：短信验证码登录（2.8.0 起，走官方短信通道、
不依赖扫码链路）或 Cookie 登录（真实浏览器登录一次复制凭据）。

**8810「网络环境存在安全风险」**（验证码/登录场景的网络级风控，与出口 IP
相关，重试基本没用）：App 内网易域名**一律强制直连**——reqwest 会读
`HTTP_PROXY`/`HTTPS_PROXY` 环境变量，从带代理变量的终端启动 App 也会被
兜住；若仍遇到（ Clash TUN / 全局模式在系统层接管了流量），关代理或换
网络（如手机热点）即可。登录链路同时改为**纯 web 指纹**：浏览器 UA 只配
浏览器会话 Cookie，不再混入 `os=osx` 客户端身份（UA 与 os 不自洽会被
按异常环境处理）。8810 / -462（要滑块人机验证，App 内做不了）/ 406 / 509
都有各自的行动建议文案。

**可靠路径是 Cookie 登录**（不依赖扫码）：桌面侧栏 / 手机「我的」→「账号」→
「用 Cookie 登录」→ 在真实浏览器登录 `music.163.com` → `F12` →
`Network` → 刷新 → 点任一 `music.163.com` 请求 → 复制 Request Headers 里的
整段 `Cookie`（**必须含 `MUSIC_U`**）→ 粘贴并「保存并验证」。
`MUSIC_U` 是 HttpOnly，控制台 `document.cookie` 读不到，只能从请求头复制。
保存后宿主用 `/api/nuser/account/get` 校验并显示昵称 / 会员状态；
凭据只落在本机 SQLite，点「退出」即清除。注意网易云的凭证是「被吊销」
而非「过期」：改密码、异地登录、账号风控都会让旧 Cookie 立即失效，只能重取。

**云端歌单**（登录后自动出现）：账号页列出账号全部歌单与「我喜欢的音乐」，
每项带网易云的封面图；普通歌单可直接播放或**导入为本地歌单**（连同封面一起
记下来，侧栏缩略图 /「我的」页都用它），「我喜欢的音乐」走「同步」——只把
本地还没有的曲目补进收藏。歌单封面优先用歌单自己记的图（云端导入时写入），
没有就回退到歌单内第一首曲目的封面，所以手动建的本地歌单也有图可看。

导入时会往本地歌单记一条 `remote_key`（`插件id:云端歌单id`），「已导入过」
就是靠它判断的，重启软件也不会丢；对已导入的歌单再点「导入」会先弹确认
（再导会新建一个重复的歌单）。账号页还有**歌单链接导入**：把网易云分享的
完整链接（`music.163.com/playlist?id=…`）或 `163cn.tv` 短链粘进去即可，
短链由宿主命令 `resolve_redirect` 跟随重定向解析出歌单 id，导入免登录；
QQ 音乐 / 酷狗 / 酷我 / 咪咕 / B 站的分享链接也能识别平台并给出明确提示，
只是还没有对应音源插件、暂不能导入。

**受限歌曲解析**（插件 2.2.0 起，默认开启）：官方只给试听时，把「取一条完整直链」
交给第三方解析服务 GD 音乐台——AlgerMusicPlayer 与 UnblockNeteaseMusic 的
`pyncmd` 用的都是它。

| 步骤 | 行为 |
|---|---|
| 判定 | eapi 返回 `freeTrialInfo:{start:0,end:30}` / `time:30040` / `br:128012` 即试听；免费曲这几项是 `null` / `0` / `320000`，直接返回，不打扰第三方 |
| 主路径 | `GET api.php?types=url&source=netease&id=<id>&br=320` → `music.126.net` 上带时效签名的正版全曲 |
| 兜底 | 同平台搜「歌名 + 歌手」，歌名归一化且歌手对得上才采用；不跨平台换源，避免翻唱 / Live 错配 |
| 失败 | 第三方限流或关停 → 保留官方 30 秒试听，前端照旧弹提示，不静默失败 |

只用到已有的 `leon.http.get`，宿主侧零改动——因为链路本来就是
「插件返回 URL → Rust 用 reqwest 下载」，不需要像 Electron 客户端那样
起本地服务或自定义协议代理。

**听歌打卡**（插件 2.7.0 起，默认开启）：登录后，播放累计满 30 秒的曲目
会在切歌或播到曲尾时自动上报网易云，计入**最近播放 / 听歌排行 / 听歌时长 /
歌曲播放次数**（App 里 10–30 分钟后可见）。网易云的这些数字没有独立的
同步接口，完全由客户端上报的播放日志产生：

- 老 weapi 打卡接口（`/weapi/1.0/scrobble`、`music.163.com/api/feedback/weblog`）
  2026 年起被服务端**静默丢弃**——返回 code 200 但不计入排行（多人实测，
  见 hana-music-api#31）；
- 实测能计入的是官方桌面客户端路径：`clientlog.music.163.com/eapi/feedback/weblog`
  （eapi 加密），一次上报两条日志——`startplay` 让歌进「最近播放」，
  `play`（`time` 为整数秒）驱动排行计数；请求 Cookie 带 mac 客户端指纹
  （`os=osx; appver=3.1.10.5100; osver=15.5; channel=netease`），只覆盖打卡请求；
- 时长按播放位置累计（与官方客户端一致），单曲循环/手动重播各算一次；
  未登录直接跳过；打卡被拒（Cookie 失效/风控）后本会话不再尝试；
- 插件配置里有「听歌打卡」开关，关掉就完全不上报。

设置 → 音源插件 → 网易云音乐里的**「受限歌曲解析」开关**可一键关闭
（`unblock=off`），关掉后完全只走官方链路；`解析音质` 跟随「在线音质」档位自动写入
（无损档 = 999，实测可拿到 FLAC），也可在播放页的「高级」折叠区手动覆盖。
`fieldType: "boolean"` 会渲染成开关，草稿初始化时合并 manifest 的 `default`，
所以「数据库里没存过配置」和 UI 显示的开启状态是一致的。

> **这不是破解 VIP**：客户端没有绕过任何鉴权，只是把取链请求转给一个持有
> 播放权限的第三方服务，音频仍是网易云自己 CDN 上的文件。代价是歌曲 ID
> 会发给第三方、服务可能随时限流或关停，版权上属于灰色地带。
> 仅供个人使用，别做分发服务。

### 5.3 自己写一个

```bash
# 目录：%APPDATA%/com.leonmusic.player/plugins/my-source/
# 放 manifest.json + index.js，然后「设置 → 音源插件 → 重新加载」
```

改完即时生效。内置插件会随版本升级覆盖到用户目录（比较 manifest 的
`version`，内置更高才写），但**用户配置存在 SQLite，不会被覆盖**。

---

## 6. 已知风险与限制

诚实清单，按影响排序：

1. **SAF 目录导入尚未接线**：本地扫描走文件路径（`/sdcard/Music/...`），
   需要 `READ_MEDIA_AUDIO`。Android 11+ 想导入 `Android/data` 之类受限目录
   只能走 SAF，当前版本没做——扫描的是「系统媒体库能直接读到」的路径。
2. **真机 arm64 已装并跑通**（HyperOS / API 37）：扫描（含中文文件名）、
   播放/暂停/seek/倍速、队列 next/prev/自动续播、系统媒体键、MediaStyle
   通知、布局几何都有实测记录。尚未在 armv7 / x86 真机上验证。
3. **音频焦点未实测**：`AudioFocusHelper` 的四种回调（来电暂停、
   导航 duck、恢复播放）只做了代码审查，没有真的打进电话验证。
   如果出现「来电后不恢复」或「导航播报后音量回不来」，先看这里。
4. **Opus 用 `opus-pure`**：纯 Rust 实现，没有 C 依赖、交叉编译零负担，
   但解码速度不如 libopus。低端安卓机上极长 Opus 曲目可能出现欠载（underrun）。
5. **流媒体缓存是整曲落盘**，不是边下边播。原因是 symphonia 需要 `Seek`。
   代价：一首 30MB 的 FLAC 要下完才出声；收益：seek 完全无延迟、离线可播。
6. **插件沙箱没有域名白名单**，靠用户自己填的 `baseUrl`。
   装来路不明的插件等于给它访问网络的能力，请只装自己看得懂的脚本。
7. **网易云走的是非官方 web 接口**，没有官方授权，随时可能被改签名规则或封禁。
   匿名可搜索、取歌词；`fee=1`（VIP / 版权曲）官方只下发 128k / 30 秒试听。
   插件默认把这类曲目交给第三方解析服务取完整直链（见 5.2），该服务可能
   限流、关停或改协议；关掉「受限歌曲解析」即退回纯官方链路。
   扫码登录自己的账号（或手填 Cookie）后，**只有会员且在权限内**的曲目
   才由官方下发完整时长与更高码率：普通账号登录不会解除限制，数字专辑
   仍需购买，下架曲 `code:404` 谁都不行。插件保留 `baseUrl` 作为兜底：
   内置接口失效时填一个 Meting 地址就切回旧路径。仅供个人学习，别做分发服务。
8. **桌面歌词窗的点击穿透**在 Windows 11 + 某些显卡驱动下可能失效；
   真机验证时如果挡住操作，先用托盘菜单关掉。
9. **前端没有音频可视化**。因为禁止用 Web Audio 作为播放引擎，
   波形/频谱必须由 Rust 算好推给前端——当前版本没做。
10. `panic = "abort"`：release 下任何 panic 都会直接退出进程，
    换来做体积更小（`opt-level = "s"` + LTO + strip）。
    调试期请用 dev profile。
11. **debug APK 约 352 MiB**：拆包实测，APK 里其实只有**一个** ABI 的
   `lib/arm64-v8a/libleonmusic_lib.so`（361,070,560 B，STORED 未压缩），
   并不是「三个 target 全打进 universal 包」。体积几乎全来自未 strip 的
   调试符号：`.debug_*` DWARF 约 257 MiB、`.symtab + .strtab` 约 36 MiB，
   真正的 `.text` 只有约 33 MiB。原因是 `[profile.dev]` 没有 `strip` / LTO /
   `opt-level = "s"`，而 AGP 的 debug 配置还主动
   `jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")`，`.so` 在 APK 里又不压缩。
   实测 `llvm-strip --strip-debug` 可把 `.so` 降到约 85 MiB，再加
   `--strip-all` 约 51 MiB。Windows 之所以只有十几 MB，是因为它走
   `[profile.release]`（`strip = true` + LTO + `opt-level = "s"`）且 zip 会再压一次，
   两边不是同一种构建。release APK 按体积比推算约 20–30 MB（尚未实测编译）。
12. **Android 增量打包会让 APK 翻倍**：只要 `libleonmusic_lib.so` 有变化，
    AGP 就在**已存在**的 APK 文件上追加写入，旧内容变成 central directory
    未引用的残留（369 MB 包变 730 MB；zip 仍能打开、安装大概率也正常，
    但体积翻倍）。这与构建目录脏不脏无关——干净的 `app/build` 也会复发。
    标准做法：**构建前删掉
    `app/build/outputs/apk/universal/debug/app-universal-debug.apk`**
    （单文件即可，不必清整个 build 目录），实测删后重跑立即恢复 369 MB。

### 6.1 只有真跑才会撞上的坑（都已修，留档备查）

**JNI 从原生线程 `FindClass` 找不到应用类**

```
java.lang.ClassNotFoundException: Didn't find class "com.leonmusic.player.LeonBridge"
  on path: DexPathList[[directory "."]]
```

`FindClass` 的行为取决于当前线程：Java 线程用它定义类的 classloader，
而 Rust 自己的线程（引擎线程 / ticker）只用 bootstrap classloader，永远找不到
应用类。正确做法是从 Application context 取 `ClassLoader` 再 `loadClass`：

```rust
let loader = env.call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?;
let class = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", ...)?;
```

**`startForegroundService` 的 5 秒契约 vs「不该有通知」**

冷启动还没歌，不需要通知；但只要用了 `startForegroundService`，
就必须在 5 秒内 `startForeground()`，否则 `ForegroundServiceDidNotStartInTimeException`。
为了满足契约先弹「未在播放」再撤掉，用户会看到通知栏闪一下。
所以冷启动走 `startService`，真正开始播放时才 `enterForeground()` 提升。

**`clear` 不能顺手 `stopSelf()`**

Rust 的 ticker 在「当前没有曲目」时会推 `clear`，冷启动也会推一次。
早期实现里 `clear → stopAll → stopSelf`，结果服务刚起来就被杀，
MediaSession 闪断、点播放要重新拉起。现在 `clear` 只退出前台 + 清元数据，
真正销毁留给「划掉任务且没在播」。

**「服务刚起来 6 秒就死了」是系统行为，不是 bug**

真机（Android 17 / HyperOS）冷启动后 logcat 会出现：

```
W ActivityManager: Stopping service due to app idle: u0a179 -6s129ms com.leonmusic.player/.PlaybackService
```

这是 Android 8+ 的后台服务限制：**非前台**的 started service 在应用离开前台
约 6 秒后被回收。冷启动还没歌、服务没进前台，被回收是正常的。
真正开始播放时 `enterForeground()` 会把它钉住，那时系统不会再动它。

**`env(safe-area-inset-bottom)` 在 Android 上是 0**

手势导航的设备上底部会顶到手势条。原因和处理见 [4.6](#46-手机上的布局是怎么做的)。
一句话：Chrome for Android 只把刘海算 safe area，导航栏一律报 0，
必须在原生侧读 `WindowInsets` 再注入 CSS 变量。

**Android 上 boa 的 nan-boxing 直接崩：所有音源插件都不可用**

在真机上点搜索，前端只拿到一句毫无信息量的：

```
plugin: join: task 23 panicked with message
"this platform is not compatible with a nan-boxed `JsValueInner`
enable the `jsvalue-enum` feature to use the enum-based `JsValueInner`"
```

根因在 boa 的 `nan_boxed` 实现：它把指针塞进 f64 的尾数位，只保留低 48 位
（`MASK_POINTER_VALUE`），并在 `tag_pointer` 里断言「掩码后必须等于原值」。
Android aarch64 的 scudo 分配器 + HyperOS 的 top-byte-ignore 会让堆指针带上
高位 tag，断言必然失败。**这不是配置问题，也不是插件写错了**——搜索、
解析播放地址、歌词全部走同一条路，所以表现成「搜不了、也播不了」。

官方给的出口就是 `jsvalue-enum`：用 enum 代替位打包。只在 Android 上付这个
代价，桌面继续用更快的 nan-boxing：

```toml
[target.'cfg(target_os = "android")'.dependencies]
boa_engine = { version = "0.22", features = ["jsvalue-enum"] }
```

同一份代码在两个 target 段声明同一个 crate 是允许的，feature 按 target 取并集，
`cargo tree -e features --target aarch64-linux-android -i boa_engine` 可确认。

顺带把 `spawn_blocking` 的 join 失败改成可读文案：tokio 的 `JoinError` 被 tauri
包成了 `tauri::Error::JoinError`，`into_panic()` 里的载荷只有第一行有用
（boa 的平台断言、数组越界之类），后面全是 unwinding 栈噪声。
现在提示是「插件执行崩溃: <第一行>」，而不是一整屏 backtrace。

**在线曲目「点了没反应」：id 查询不到还没入库的歌**

`player_play_queue(track_ids)` 走 `db.tracks_by_ids`，而搜索结果里的曲目
**还没入库**，一个都查不到，直接 `not found: no playable tracks in queue`。
更隐蔽的是 `tracks_by_ids` 会静默跳过不存在的 id——混着本地和在线曲目时，
`start_index` 会错位，表现成「点了第 5 首，播的是第 2 首」。

两条一起修：数量不一致直接报错（宁可失败也不静默错位），
并新增 `player_play_tracks(tracks, start_index)` 走对象路径。引擎收到没有
`file_path` 的曲目会自动发 `resolve-request`，由上层解析直链、落盘、再回来播，
所以这条路不需要先入库。

**播放失败时用户什么都看不到**

`shared.error` 一直在写，`ticker` 每 120ms 也一直在推快照，但前端从来没用过
`snap.error` 这个字段。设备打不开、直链解析失败、格式不支持——全都静默。
现在 `player` store 监听状态流时会 toast 出来，并按文本去重（否则同一条错误
会连着弹几十次；error 清空后要重置，不然重试同一首不再提示）。

---

## 7. 设计决策速查

**为什么不用 Web Audio？**
硬性要求，也是 Android 上的正确答案：Web Audio 的音量/进度与系统媒体会话
对不上，切后台还容易被 WebView 冻结。Rust 持有 `cpal::Stream` 后，
WebView 只是个遥控器。

**为什么自己写 WSOLA？**
`signalsmith-stretch` 要 C++ 工具链（cmake / cl），Windows 和 Android
交叉编译都要额外配置。自研 200 行的 WSOLA 换来零依赖 + 可读性。

**为什么设置页要有「音频输出自检」？**
「进度条在走但没声音」用 `PlayerSnapshot` 判不出来：位置由
`played_frames` 推导，ring buffer 空转补零时它照样增长。曾有一次排查中，
VIP 曲的解码/解析/输出全链路实测都正常（会话峰值 0.19–0.61），
真正原因只是**那个进程的输出状态**——输出流在启动时绑定一次系统默认设备，
运行中插耳机/切设备不会自动跟随；另外播放条改音量原先只改引擎、不写设置，
设置页仍显示旧值、重启又「自己好了」。所以补了 `player_audio_diag`
（设备名 / 音量 / 已输出帧 / 欠载 / 设备错误 + 中文判读）与
`player_audio_reset`（重开输出流并从当前位置续播），并把播放条音量写回数据库。
设置 → 播放 里点「刷新」即可自查，音量 0 时播放条也会显示「已静音」。

**为什么变速变调用「WSOLA + 重采样」两级？**
WSOLA 拉伸比 `T = P / S`（保持音高只改时长），再按 `R = P` 重采样
（同时改音高与时长），合成后音高 ×P、速度 ×S，两者解耦。
见 `audio/dsp.rs` 头部注释的推导。

**歌词为什么不推事件？**
歌词是「按需加载」的资源。推事件意味着每首路过的歌都要解析一遍 LRC；
现在前端在 `trackId` 变化时主动 `invoke("player_lyrics")`。

**为什么 `played_frames` 而不是计时器？**
设备真实消费的帧数不会因为缓冲区大小、暂停、seek 而漂移。
`Instant::now()` 差值在 Android 上会随 Doze 变得不可靠。

**为什么插件每次调用新建 Context？**
`boa_engine::Context` 是 `!Send`。要跨 `spawn_blocking` 线程复用只能加锁，
反而更慢。每次新建 Context + 预编译脚本，实测足够快（插件调用本身有网络 IO）。

---

## 8. 验证状态

| 检查项 | 命令 | 结果 |
|---|---|---|
| Rust 编译（Windows） | `cargo check --lib` | ✅ 0 warning |
| Rust 编译（Android arm64） | `cargo check --lib --target aarch64-linux-android` | ✅ 0 warning |
| Rust 单测 | `cargo test --lib` | ✅ 29 passed（另有 3 个联网用例默认 `#[ignore]`） |
| Rust 联网回归 | `cargo test --lib -- --ignored --nocapture` | ✅ 3 passed：搜索/直链/歌词、扫码建码/轮询/匿名账号、VIP 曲解析到完整直链（`Content-Range` 9,584,893 字节） |
| 前端类型 | `npx vue-tsc --noEmit` | ✅ |
| 前端构建 | `npx vite build` | ✅ 双入口 |
| Windows 便携版（单文件 exe） | `npx tauri build --no-bundle` | ✅ 加载 `http://tauri.localhost/`，CDP 验证登录弹层 |
| Windows 便携版 VIP 全曲 | CDP 驱动 `plugin_search` → `player_play_tracks` → `player_snapshot` | ✅ 2.2.0 开关默认开启；`fee=1` 海阔天空 `durationMs=239560`、`playing`、`error=null`、无试听提示 |
| VIP 曲真实音频输出 | `pycaw` 读 Windows 音频会话 `IAudioMeterInformation` | ✅ 本地 WAV 峰值 0.38；在线 VIP 0.19；连播 6 首周杰伦 VIP 曲峰值 0.19–0.61，全部非静音 |
| 音频自检 / 重连 | 设置 → 播放 → 音频输出自检 | ✅ `player_audio_diag` 返回设备名、音量、已输出帧、欠载、设备错误与判读提示；`player_audio_reset` 重绑系统默认设备并保持播放位置 |
| 在线音质 / 真实规格回显（插件 2.5.0） | Rust 联网回归 + 产物静态核验 | ✅ `netease_vip_unblock_live` 实测：默认档 `source=resolved / format=mp3 / bitrate=320000`（9.58 MB 全曲）；无损档 `source=resolved / format=flac / bitrate=934000 / downgraded=false`；官方试听 `source=trial / downgraded=true`。dist / exe / APK 均含 2.5.0 新代码。本次**未做 CDP 点验**：用户进程占用单实例，新 exe 被转发到旧窗口，未强制关闭 |
| 在线音质改为下拉（本次） | `npx tauri build --no-bundle` + 便携版 CDP 9333 | ✅ 六档 option 渲染、Hi-Res / 高清臻音 disabled、切「无损」写配置并 toast、切回「标准」恢复原值；`.quality-option` 残留 0 个；最长选项文本（无损 · … · 解析可得）收起态完整显示 |
| 移动端布局全面对齐（KA Music 参考） | 无头 Chrome 412×915 + `build/mock_tauri.js` 注入 IPC | ✅ 首页 / 我的 / 搜索 / 我喜欢 / 全屏播放页 / 歌词分页 / 二级设置页逐页截图；搜索结果 8 行、队列抽屉盖在播放页之上（`isolation:isolate`）、1280px 桌面回归侧栏与播放条完好。截图 `build/mobile-*.png` |
| 桌面端联调 | `npm run desktop:dev` | ✅ 见下 |
| Android debug APK 重打（移动端全面对齐） | `mv app/build` 隔离旧目录 → `npx tauri android build --apk --debug --target aarch64` | ✅ 369,197,003 B，sha256 `c56204e60fd272620e0bed53f88e471819f491beb15509133ec918150be61d1c`；972 条目、`testzip()` 全通过、EOCD 落在文件末尾、APK v2 签名块存在；`.so` 内嵌本次 `dist/assets/main-B8TmUgVH.js` 等资源名，确认新移动端 UI 已进包。首次增量构建产出的 730 MB 尾部残留包已隔离，不再作为交付物 |
| 底栏隐藏「搜索」「我喜欢」（本次） | `npm run build` + 无头 Chrome 412×915 + `build/mock_tauri.js` | ✅ 胶囊 3 列 300px：`items=['首页','我的']` + 中央唱片；首页搜索框 → 搜索页（底栏高亮首页）→ 返回；我的 → 收藏 → 我喜欢的（高亮我的）→ `.m-back` 返回回「我的」；唱片打开全屏播放页；运行时异常 0（仅 favicon 404）。截图 `build/nav-*.png` |
| 插件 2.6.0：推荐聚合 / 账号歌单 / 云端歌单 | `cargo test --lib -- --ignored --nocapture` | ✅ 6 passed：新增 3 条 —— `getRecommendFeed` 未登录返回个性推荐歌单 6 个 + `loggedIn=false`；`getPlaylist('3778678')` 热歌榜 200 首全量、字段齐全；`getUserPlaylists` 未登录返回 `loggedIn=false` 不抛错。原有 roundtrip / 登录 / VIP 解析继续通过（无损档仍 `resolved/flac/934000`） |
| 插件 2.7.0：听歌打卡（clientlog eapi） | 离线协议测试 + 真账号 live | ✅ `netease_scrobble_logs` 锁 startplay/play 字段形态（`end=playend`、`time` 整数秒）、`netease_scrobble_requires_login` 未登录 301 且不联网；`netease_scrobble_live` 用本机真实登录 Cookie 端到端打卡成功（clientlog 返回 code 200，日志记入的是真实播放一次）。排行/听歌时长变化有 10–30 分钟服务端延迟，需在网易云 App 里人工复核 |
| 桌面壳改造（本次） | 无头 Chrome 1280×800 + `build/mock_tauri.js` | ✅ 默认落首页：侧栏只剩 首页/媒体库/我喜欢的/账号；首页 = 搜索框 + 推荐歌单网格 + 每日推荐歌曲流（登录态标题「首页，Leon」）；搜索框 → 全屏搜索覆盖层（✕ 关闭）；播放条点歌曲 → 播放覆盖层（`close-now` 收起、回来源页）；账号页登录后自动出现「云端歌单」：我喜欢的音乐（同步）+ 账号歌单（播放 / 导入）。截图 `build/dt-*.png` |
| 五连改造（本地音乐改名 / 新图标 / 歌单封面 / KeepAlive / 首页内联搜索） | 无头 Chrome + `build/walk-ui.py`（mock 计数断言） | ✅ 侧栏=「本地音乐」+ 歌单封面缩略图（回退第一首曲目封面）；导航图标重绘（房子 / 文件夹音符 / 心形 / 圆形头像）；账号页云端歌单三项全带封面（含「我喜欢的音乐」）；切换页面 `recommend_feed` / `login_info` 调用次数不涨（KeepAlive + `viewKey` 修复播放覆盖层开关的重挂载）；首页搜索框输入即出结果（防抖 400ms、过期响应丢弃、清空回推荐），无独立搜索页。移动端外壳同步 KeepAlive（`:max=4`）。截图 `build/dt-inline-search.png` / `dt-account-covers.png` / `dt-mobile-regression2.png` |
| 导入防重复 / 歌单链接导入 / 首页滚动修复 | 无头 Chrome + `build/walk-ui.py`（mock 有状态歌单库） | ✅ `playlists` 表加 `remote_key` 列（幂等迁移）+ `playlist_set_remote_key` / `resolve_redirect` 命令；已导入的歌单显示「再导入」，点击弹确认层（取消 / 仍然导入），确认后照常执行；链接导入：网易云完整链直接导、`163cn.tv` 短链经 resolve_redirect 解析（mock 返回 `playlist?id=8810448888`）、QQ 音乐链提示「暂无对应音源插件」、单曲链提示「没有歌单 ID」；导入后 remoteKey 落库、新歌单出现在侧栏。首页内容区接管滚动（`.home .body`，页头搜索框固定），滚到底不再被裁切。走查 12 项断言全过、0 运行时异常。截图 `build/dt-home-scrolled.png` / `dt-import-confirm.png` |
| 短信验证码登录（插件 2.8.0） | `cargo test`（协议形状锁 + 真机探针）+ 无头 Chrome 走查 | ✅ 发码 `/weapi/sms/captcha/sent` + 登录 `/weapi/w/login/cellphone`（对齐 api-enhanced 参数形态，`phone` 字段），直连网易、手机号不经第三方；账号卡片新增「验证码登录」按钮与弹层（+86 前缀、60s 重发倒计时、错码报错、成功后自动刷新账号卡片）；真机探针实测错验证码 = `503 验证码错误`（登录请求不发短信）；成功凭据与扫码共用宿主落库路径。走查 13 项断言全过、0 运行时异常。截图 `build/dt-captcha-sent.png` |
| 8810 风控修复（插件 2.8.1） | `cargo test`（直连判定 / 指纹锁 / 失败码翻译）+ 真机探针 + 走查 | ✅ 真机验证码登录报 8810（网络环境风险）：① `net/client.rs` 按域名选客户端——网易系（163.com / 126.net / 163cn.tv / netease.cn）强制 `no_proxy` 直连，插件全部 HTTP（postForm/postFormEx/getEx/缓存下载/短链解析）与异步 HttpClient 同步接入，防环境变量代理把登录带出境；② 登录链（扫码 + 验证码）改纯 web 指纹，`anonCookie` 不再混入 `os=osx`；③ 8810 / -462 / 406 / 509 翻译成行动建议（关代理换网络 / 改扫码或 Cookie / 等冷却）。探针（挂代理环境变量直连网易）= 503 正常错误，直连路径生效。cargo 37 条全过、走查 PASS 0 异常 |
| Android APK 重打（2.6.0 / 桌面改造同包） | 构建前 `rm` 旧 APK → `npx tauri android build --apk --debug --target aarch64` | ✅ 369,890,275 B，sha256 `30ebd4e8bd30da8f3dacafb6849dda8b74b424944e4fda8e8285f1847f0c80d4`；972 条目、`testzip()` 通过、v2 签名块、EOCD 正常；`.so` 内嵌本次 `main-J9mZ9Hle.js` / `main-sMAXnL7b.js` / `main-BdfbToIL.css`。Windows 便携 zip 内 exe sha `e6951193…`（14,849,024 B）；`E:\Test` 目录内 exe 因被运行中进程占用暂为上一版，退出程序后覆盖。增量追加残留坑与对策见 6.1 第 12 条 |
| Android 构建（x86_64 / arm64） | `npx tauri android build --apk --debug --target …` | ✅ |
| Android 播放链路（模拟器 API 35） | 见下 | ✅ |
| Android 真机（arm64 / API 37 / HyperOS） | `adb install -r` | ✅ 同一套链路复现通过 |
| Android 布局（竖屏 + 横屏，模拟器与真机） | 见下 | ✅ |
| 移动端播放页标题对齐封面（本次） | `npm run build` + 无头 Chrome 390×844 + `build/mock_tauri.js` | ✅ 顶部歌名/歌手左边缘 = 大封面左边缘：390 / 360 / 412 / 320 四档实测 58.5 / 54 / 61.8 / 48 px 两组数值完全相同；封面宽与左偏移共用一个 `--cover-w`，改一处两边同步；封面下重复歌名改为两行歌词预览（左对齐同宽）；收起箭头去圆形底、心形/更多改描边圆；歌词分页复用同一左基线（`.line` left 58.5 = 标题 58.5）；长标题 `scrollWidth > clientWidth` 省略生效、右边缘 236 < 按钮左 246。截图 `build/tmp/mobile-player-*.png` |
| 探测失败可读化（本次） | `cargo test --lib` + 本地 mock HTTP | ✅ 三种非音频响应的错误文案：HTML → 「音源返回的不是音频（网页/XML，Content-Type: text/html; charset=utf-8）：「<!DOCTYPE html>…」——链接可能已失效或需要登录」；JSON → 同类 JSON 结论；空体 → `probe failed …（来源没有返回任何数据）`。本地加密头（`CTENFDAM…`）→ 错误附 `文件开头字节 43 54 45 4e…`；0 字节 → `（文件是空的：0 字节）`。`HttpStream` 新增 `content_type()` / `sniff()`（不改读游标）；49 passed / 0 failed |
| 起播耗时归因（本次） | 真实联网基准（Boa 宿主 + 真实 CDN，同一首连测） | ✅ 解析链是 4 个串行请求：酷我搜索 199–217ms / 广告解锁 87–143ms / 取链 84–91ms / Range 长度探测 23–24ms，`resolveTrack` 内部 494–523ms，另加每次新建 Context 的 eval 85–96ms。起播段（开流 + probe + 首个解码块）原本要再冷握一次 CDN 证书 ~200ms，共享连接池 + 探测预热后降到 17–34ms；冷连接首请求 208ms vs 复用 25ms |
| 坏缓存防护（本次） | `cargo test --lib` + 桌面缓存实扫 | ✅ 落盘前按魔数校验（ID3 / fLaC / OggS / RIFF / ftyp / MP3 帧同步 / ASF）：`.part` 大小等于 Content-Length 但内容不是音频时**只跳过缓存**，播放不受影响；打开缓存失败时只删除 media 缓存目录内的文件并清库记录，用户本地音乐不碰。测试：HTML 正文（长度精确匹配）不落盘且 `.part` 被清理、魔数表 flac/id3/帧同步/m4a/wav/html/json/空。桌面缓存目录现存 4 文件 154.9 MB 全为音频，无历史坏文件。51 passed / 0 failed |
| 启动不再等曲库（本次） | `npm run build` + `build/mock_tauri.js`（可延迟指定 IPC）+ 无头 Edge 390×844 / 1280×800 | ✅ 曲库 IPC 延迟 2000ms：300ms 时首页已渲染、6 行骨架（52px 封面与真实行同尺寸，填充不跳动）、全程无启动遮罩；2s 后骨架消失、8 行真实曲目。延迟 0：`bootSpinner` 从未出现。播放器快照延迟 600ms：320ms 时显示 spinner「正在启动…」，1300ms 后首页就绪。桌面 1280 宽：侧栏/播放条正常，仅主区居中 spinner。首屏只等 `settings`+`player`（4 个 IPC），曲库与播放器并行且不阻塞渲染 |

单测覆盖：WSOLA 拉伸比、重采样倍率、队列三种循环行为、LRC 五种解析
（普通 / 多时间标签 / 逐字 / 偏移 / 同时间戳译文合并）、文件名解析、
AES-CBC / AES-ECB / RSA-NoPadding 与 Node `crypto` 逐字节对齐、
`Set-Cookie` 单条解析与**多条同名响应头**收集、二维码 SVG 结构、
内置插件脚本入口、试听片段判定与解析开关语义、内置插件版本比较、
扫码风控状态码映射（801/802/800/8821/设备环境异常 → blocked）、
会话 Cookie 合并去重、音质档位降级顺序（六档 + 非法值回落 exhigh）、
档位→解析码率映射、直链格式推断、官方静默降级与逐档降级的 `downgraded` 判定。

**桌面端实测**（CDP 真实驱动 UI + IPC，非纸面推演）：启动无 panic、SMTC ready、
托盘与隐藏歌词窗就位；扫描 3 首 WAV → 标签解析正确；点行播放、暂停、上一首、
下一首、seek、音量、循环、随机全部正确；进度 1.2s/1.21s 无漂移；
逐字 LRC 解析为 `kind: wordByWord` 且歌词窗高亮当前字；深浅色主题均正常。

**Android 实测**（模拟器 x86_64 / API 35，通过 WebView DevTools 远程调试驱动）：

| 场景 | 结果 |
|---|---|
| 扫描 `/sdcard/Music/LeonTest` | ✅ 3 首，含中文文件名与音轨号剥离 |
| 播放 / 进度 | ✅ 1.5s 墙钟推进 1520ms |
| 暂停 | ✅ 漂移 0ms |
| seek 8000ms | ✅ |
| 2.0× 变速 | ✅ 2s 墙钟推进 4000ms |
| 队列 next / prev / 自动续播 | ✅ 游标与高亮同步 |
| 系统媒体按键（87/85/88） | ✅ MediaSession → JNI → Rust 全链路 |
| 通知栏 | ✅ MediaStyle + 上一首/暂停/下一首 |
| **前端卸载（页面导航到 `about:blank`）** | ✅ 期间 MediaSession 仍 `PLAYING`，进度 5200→9200ms；导航回来 UI 从快照恢复，进度连续（Δ11760ms） |

> 关于「WebView 被回收后继续播放」，我只验证到**前端页面被卸载**这一层：
> 把页面导航到 `about:blank` 后 Vue 应用完全消失，播放不受影响，说明前端确实只是遥控器。
>
> 更极端的两种模拟都不成立，别被误导：
> * `adb shell kill -9 <renderer>`：非 root 下直接 `Operation not permitted`（会静默失败！）；
>   root 下杀掉渲染进程会让**整个应用进程一起死**（Chromium 的渲染进程崩溃策略），
>   logcat 里能看到 `Process com.leonmusic.player has died: fg TOP`。
> * 开发者选项「不保留活动」（`always_finish_activities=1`）：在该模拟器镜像上
>   实测未触发 Activity 销毁，CDP 目标一直存在，因此不能当证据。

> 交叉编译能过，说明 **Rust 侧的平台分叉是干净的**：Windows 模块
> （HWND / SMTC / 托盘）在 Android 目标上完全不参与编译，
> Android 专属的 JNI 桥也只在 `cfg(target_os = "android")` 下生效。

**布局实测**（几何断言 + 逐页截图，模拟器 393×851 与真机 384×853 各跑一遍）：

| 断言 | 结果 |
|---|---|
| 无横向溢出（`shell.scrollWidth - clientWidth`） | ✅ 0 |
| 标题在状态栏下方（`title.y >= --sat`） | ✅ |
| 底栏不压手势条（`sidebar.bottom <= vh - --sab`） | ✅ 真机 833 = 853 − 20 |
| 播放条内容未被裁切（`scrollHeight == clientHeight`） | ✅ 70 = 70 |
| 5 个 tab 均分且触摸高度 ≥48px | ✅ 75×52 |
| 播放条进度条随时间推进 | ✅ `--p` 11.18% → 29.50%（2s） |
| 歌词空状态垂直居中 | ✅ 偏差 <12px |
| 列表单击播放 | ✅ 点「中文歌名」→ `playing`，并跳转「正在播放」 |
| 横屏（851×393）内容区可用 | ✅ 头部压缩成一行，回两栏歌词 |

**在线音源全链路实测**（真机 arm64 / API 37 / HyperOS，直连 `music.163.com`，
不用 mock、不用自建服务）：

| 场景 | 结果 |
|---|---|
| 插件搜索「海阔天空」 | ✅ 415ms 返回 5 条，中文标题/艺术家/专辑无损，`source.kind = plugin` |
| 列表点行播放 | ✅ `playing`，跳转「正在播放」，封面/歌词/标题全部就位 |
| 免费曲（`fee=8`） | ✅ 起风了 325.9 秒完整播放，`durationMs` 与元数据完全一致 |
| VIP 曲（`fee=1`），解析开启 | ✅ 海阔天空拿到 320k / 9.58MB 全曲，与元数据 240s 一致 |
| VIP 曲（`fee=1`），解析关闭 | ✅ 回落 128k / 30 秒试听，UI 弹提示说明原因与解决办法 |
| 直链解析 + 落盘 | ✅ `cache/media/<sha256>.mp3`，播过一次即可离线重播 |
| 歌词 + 翻译 | ✅ 点播后自动拉取，逐行高亮；同时间轴译文并入 `translation` |
| 封面加载 | ✅ `http://p3.music.126.net/...` 改写成 https 后 WebView 正常显示 |
| 插件热升级 | ✅ 磁盘上的 1.0.0 被 2.0.0 覆盖，SQLite 里的旧配置保留 |

> **`fee` 分级是服务端行为，不是链路坏了**：
> `fee=8` 给 320kbps 全曲（起风了 12.7MB / 326s），
> `fee=1` 匿名只给 128kbps × 30 秒（470KB），两者都是 `code: 200`。
> 2.2.0 起 eapi 响应里的 `freeTrialInfo` 可以直接判定试听（不必再靠
> `size / br` 反推）：命中就交给第三方解析取 126.net 全曲。解析也失败时，
> 前端仍保留「解码出来的时长明显短于元数据」这道兜底，弹一次提示。
> 扫码登录**会员**账号后，权限内的曲目由官方恢复完整时长；
> 普通账号登录仍是 30 秒，这是服务端权限而非客户端缺陷。

**扫码登录实测**（真机 arm64 / API 37 / HyperOS，CDP 驱动 UI + 截屏）：

| 场景 | 结果 |
|---|---|
| 设置 → 音源插件 → 网易云账号区（旧入口） | ✅ 显示「未登录 / 扫码登录后可播放账号权限内的完整曲目」与按钮；该入口现已迁到独立「账号」页 |
| 点「扫码登录」 | ✅ 弹层出现，标题「扫码登录 · 网易云音乐」 |
| 二维码 | ✅ 宿主渲染 220×220 黑白 SVG（viewBox 225×225），定位角清晰、截图可辨 |
| 状态轮询 | ✅ 稳定停在「打开手机 App 扫一扫」（801 waiting），连续多轮无报错 |
| 联网协议回归 | ✅ 建码拿到 unikey、轮询 801、匿名账号 `loggedIn:false` |
| 2.3.0 会话预热 | ✅ 首页 `getEx` 拿到 `NMTID=...`，建码与轮询全程带同一份会话 Cookie，轮询仍回 801 |
| 独立「账号」页（Windows 便携版 CDP） | ✅ 侧栏「账号」直达、标题「账号与登录」、渲染 1 张插件卡片、风控说明卡；设置 → 音源插件 只剩「前往账号页」跳转条，cookie 输入框已隐藏 |

> **没有会员账号，所以没验到 803**：登录成功后写 cookie、会员曲目恢复完整时长
> 这两步只能靠协议对齐保证——`type=3`、二维码 URL、账号接口路径均与
> `@neteasecloudmusicapienhanced/api`（VutronMusic 用的那套）一致，
> 凭据取自多条 `Set-Cookie`（`build/mock/netease_qr_probe.cjs` 可复核接口响应）。
> 有会员账号扫一次即可确认；若 803 后仍是 30 秒，先看 `MUSIC_U` 有没有写进配置。
> 2.3.0 之后若手机弹「检测到当前设备环境异常」或服务端回 `8821`，界面会
> 直接显示 `blocked` 原因并引导「用 Cookie 登录」——详见 5.2 的风控小节。

> 离线验证协议仍可用 `build/mock/meting_server.py`（实现 `search` / `url` /
> `lrc` 三种 type，音频回一段 20 秒 WAV）+ `adb reverse`。
> 协议本身的探针在 `build/mock/netease_probe.cjs`（Node，一次性脚本），
> 它按 NeteaseCloudMusicApi 的算法独立实现了一遍加密，用来交叉验证 Rust 侧。

> 顺带记一个排查陷阱：模拟器上 `adb exec-out screencap` 有时抓不到 WebView
> 的最新合成帧（截出来整页空白，但 DOM 量出来是正常的）。
> 用 CDP 的 `Page.captureScreenshot` 才是浏览器自己渲染的结果。

**复现方法**（Android 端不需要装额外工具，WebView 自带 DevTools 协议）：

```bash
adb forward tcp:9333 localabstract:webview_devtools_remote_$(adb shell pidof com.leonmusic.player)
curl -s http://127.0.0.1:9333/json      # 拿到 webSocketDebuggerUrl
```

然后用任意 CDP 客户端 `Runtime.evaluate` 调
`window.__TAURI_INTERNALS__.invoke("player_play_track", {...})`。
注意 Chromium 会拒绝带 `Origin` 的 WebSocket 握手（403），
客户端要带 `suppress_origin`。

---

## 9. License

仅供个人学习与技术研究。音源插件连接的是**你自己的服务器**或第三方接口，
曲库版权与合规责任由使用者承担。
