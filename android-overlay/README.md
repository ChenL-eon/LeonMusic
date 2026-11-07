# android-overlay

Tauri 的 Android 原生层覆盖代码。

## 为什么单独放一个目录

`npm run android:init` 会把整个 Android 工程生成到 `src-tauri/gen/android/`，
那是个**生成物**：删了重跑就没了，改它等于白改。

所以原生代码放在这里（进版本控制），用 `apply.py` 同步过去：

```bash
npm run android:init              # 首次
python android-overlay/apply.py    # 每次改完 Kotlin 都跑一下
npm run android:dev
```

`apply.py` 是完全幂等的，重复跑不会产生重复节点；`--dry-run` 可以先看会改什么。
被覆盖的既有文件（比如 Tauri 生成的 `MainActivity.kt`）会先备份成 `*.orig`。

## 文件职责

| 文件 | 职责 | 关键点 |
|---|---|---|
| `MainActivity.kt` | 唯一 Activity | 只做三件事：把 context 交给桥、拉起前台服务、申请权限 |
| `LeonBridge.kt` | Rust ↔ Kotlin 唯一通道 | `dispatch()` 由 Rust 调用；`nativeCommand()` / `nativeSnapshot()` 由 JNI 回调 |
| `PlaybackService.kt` | 前台服务 + MediaSession | `mediaPlayback` 类型钉住进程；通知用 MediaStyle，进度由 SystemUI 按 speed 插值 |
| `AudioFocusHelper.kt` | 音频焦点 | 四种系统事件 → 四种播放器行为；`resumeWhenGain` 只在「被动暂停」时置位 |
| `Permissions.kt` | 运行时权限 | 33+ 用 `READ_MEDIA_AUDIO`，32- 用 `READ_EXTERNAL_STORAGE` |
| `BatteryOptimization.kt` | 电池白名单引导 | 只在首次启动弹一次，符合 Play 的「用户主动」要求 |

## 与 Rust 的协议

### Rust → Kotlin

`bridge.rs` 里：

```rust
env.call_static_method(class, "dispatch", "(Ljava/lang/String;Ljava/lang/String;)V", …)
```

`action` 与 `payload`（JSON）的对应关系：

| action | payload | 行为 |
|---|---|---|
| `init` | `{}` | 建通知渠道、启动前台服务 |
| `metadata` | `{title, artist, album, durationMs, positionMs, playing, coverUrl, trackId}` | 更新 MediaSession 元数据 + 通知 + 加载封面 |
| `state` | `{playing}` | 更新播放状态；开始播时申请音频焦点 |
| `position` | `{positionMs, playing}` | 更新 `PlaybackState` 的进度 |
| `clear` | `{}` | 撤掉通知、`stopSelf()` |
| `foreground` | `{enabled}` | 进/退前台 |

### Kotlin → Rust

```kotlin
@JvmStatic external fun nativeCommand(action: String)
```

| action | 来源 | Rust 侧映射 |
|---|---|---|
| `play` / `pause` / `toggle` | 通知、锁屏、耳机、焦点恢复 | `Resume` / `Pause` / `Toggle` |
| `next` / `prev` / `stop` | 通知、锁屏 | `Next` / `Prev` / `Stop` |
| `seek:<毫秒>` | 锁屏拖动进度条 | `Seek(ms)` |
| `focus-loss` | 来电、其他应用抢占 | `Pause` |
| `duck` / `unduck` | 导航播报 | `Duck(true/false)`（只压音量，不打断） |

> `nativeCommand` 的 JNI 符号是 `Java_com_leonmusic_player_LeonBridge_nativeCommand`，
> 由 `src-tauri/src/platform/android/bridge.rs` 导出。**改包名/类名必须两边一起改**。

## 调试提示

```bash
# 看原生层日志
adb logcat -s LeonBridge LeonPlayback LeonFocus LeonMain LeonBattery

# 看 Rust 日志（tracing 默认 info）
adb logcat -s leonmusic_lib

# 确认前台服务活着
adb shell dumpsys activity services com.leonmusic.player
```

常见故障：

- **通知不出现** → Android 13+ 没给 `POST_NOTIFICATIONS`；或 `smallIcon` 为 0。
  后者当前用 `applicationInfo.icon` 兜底，正常不会为 0。
- **播放几秒就断** → 电池优化把进程冻了，去设置里把它加成「不受限制」。
- **`startForeground` 抛 SecurityException** → manifest 里缺
  `FOREGROUND_SERVICE_MEDIA_PLAYBACK`，重跑 `apply.py` 检查。
- **锁屏进度条不动** → `PlaybackState` 的 `setState(state, position, speed, updateTime)`
  四个参数都要给对，少了 `updateTime` 系统就不插值。
