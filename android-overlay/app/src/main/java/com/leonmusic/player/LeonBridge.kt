package com.leonmusic.player

import android.content.Context
import android.content.Intent
import android.util.Log
import org.json.JSONObject

/**
 * Rust(JNI) ↔ Kotlin 的唯一通道。
 *
 * ## 为什么需要这一层
 *
 * Rust 是播放状态的唯一可信源，但它不知道 Android 的通知、MediaSession、
 * 音频焦点这些概念；反过来，通知栏按钮、锁屏、耳机线控的事件必须回到 Rust。
 * 两边都不该知道对方的实现，于是用这个类做「协议边界」：
 *
 * ```
 *  Rust ──dispatch(action, json)──▶ LeonBridge ──▶ PlaybackService
 *  Rust ◀──nativeCommand(action)── LeonBridge ◀── 通知/锁屏/线控/焦点
 * ```
 *
 * ## 线程约定
 *
 * `dispatch` 由 Rust 的引擎线程 / ticker 线程调用（**不是主线程**），
 * 因此这里不直接碰 UI，只把工作转交给 [PlaybackService]，后者自己 post 到主线程。
 *
 * ## 冷启动竞态
 *
 * Tauri 的 `setup` 可能早于 `MainActivity.onCreate` 里的 `attach`。
 * 此时收到的推送先缓存在 [pending] 里，attach 后按顺序重放，
 * 保证通知栏不会因为「第一条 metadata 丢了」而空白。
 */
class LeonBridge private constructor() {

    companion object {
        private const val TAG = "LeonBridge"

        /** 缓存上限：只需要最后几次状态，多了没意义 */
        private const val MAX_PENDING = 8

        @Volatile
        private var appContext: Context? = null

        private val pending = ArrayDeque<Pair<String, String>>()

        /**
         * 最近一次「文档型」状态（metadata / state / position）。
         *
         * ## 为什么需要它
         *
         * `startService` 是异步的：`ensureService()` 返回后 [PlaybackService.instance]
         * 仍可能为 null，紧接着的 `instance?.updateMetadata(json)` 就被安全调用符
         * 静默吞掉了——表现为「通知栏一直是空的，直到下一次换歌」。
         *
         * 这里把最近一次状态留档，服务 `onCreate` 时回调 [replayInto] 重放，
         * 把这段 100~200ms 的窗口补上。
         */
        private val lastDoc = LinkedHashMap<String, String>()

        private const val MAX_DOC = 3

        /**
         * 由 [MainActivity] 在 `onCreate` 调用。
         *
         * 用 applicationContext 而不是 Activity：Activity 会被销毁重建，
         * 而通知/服务必须活得更久。
         */
        @JvmStatic
        fun attach(context: Context) {
            appContext = context.applicationContext
            // 先起服务，再重放缓存，避免 PlaybackService 还是 null 时丢掉状态
            ensureService()
            val replay = synchronized(pending) {
                val copy = pending.toList()
                pending.clear()
                copy
            }
            replay.forEach { (action, payload) -> route(action, payload) }
            Log.i(TAG, "attached, replayed ${replay.size} pending message(s)")
        }

        /**
         * Rust → Kotlin。`payload` 一律是 JSON 字符串，空状态用 `{}`。
         *
         * 这个方法被 JNI 以静态方法调用，签名 `(Ljava/lang/String;Ljava/lang/String;)V`
         * 必须与 `src-tauri/src/platform/android/bridge.rs` 里的 `call_static_method` 一致。
         */
        @JvmStatic
        fun dispatch(action: String, payload: String) {
            if (appContext == null) {
                synchronized(pending) {
                    if (pending.size >= MAX_PENDING) pending.removeFirst()
                    pending.addLast(action to payload)
                }
                return
            }
            route(action, payload)
        }

        private fun route(action: String, payload: String) {
            when (action) {
                // Rust 启动完成：确保前台服务在跑（通知渠道也在这里建）
                "init" -> ensureService()

                "metadata" -> {
                    remember(action, payload)
                    val json = parse(payload) ?: return
                    ensureService()
                    PlaybackService.instance?.updateMetadata(json)
                }

                "state" -> {
                    remember(action, payload)
                    val json = parse(payload) ?: return
                    PlaybackService.instance?.updateState(json.optBoolean("playing"))
                }

                "position" -> {
                    remember(action, payload)
                    val json = parse(payload) ?: return
                    PlaybackService.instance?.updatePosition(
                        json.optLong("positionMs"),
                        json.optBoolean("playing"),
                    )
                }

                // 队列清空 / 停止：撤掉通知里的曲目信息，但服务先留着（可能马上播下一首）
                "clear" -> {
                    synchronized(lastDoc) { lastDoc.clear() }
                    PlaybackService.instance?.clearMetadata()
                }

                // 用户在前端点「后台播放」开关
                "foreground" -> {
                    val json = parse(payload) ?: return
                    if (json.optBoolean("enabled")) {
                        ensureService()
                        PlaybackService.instance?.enterForeground()
                    } else {
                        PlaybackService.instance?.leaveForeground()
                    }
                }

                // 用户在前端切换深色/浅色主题
                "theme" -> {
                    val json = parse(payload) ?: return
                    WindowInsetsBridge.setSystemBarAppearance(json.optBoolean("dark"))
                }

                else -> Log.w(TAG, "unknown action: $action")
            }
        }

        /**
         * 启动（或复用）播放服务。重复调用是安全的。
         *
         * ## 为什么用 `startService` 而不是 `startForegroundService`
         *
         * `startForegroundService` 带一个硬契约：5 秒内必须 `startForeground()`，
         * 否则系统抛 `ForegroundServiceDidNotStartInTimeException` 并杀掉服务。
         * 但冷启动时我们**不该**显示通知（还没有歌），为了满足契约就只能先弹一条
         * 「未在播放」再撤掉——用户会看到通知栏闪一下。
         *
         * 本服务的所有启动来源都在前台（`MainActivity.onCreate` 的 attach、
         * Rust setup 的 init、用户点播放），`startService` 完全够用；
         * 真正需要前台的时机是「开始播放」，那时由
         * [PlaybackService.enterForeground] 提升。
         */
        private fun ensureService() {
            val ctx = appContext ?: return
            if (PlaybackService.instance != null) return
            val intent = Intent(ctx, PlaybackService::class.java)
            try {
                ctx.startService(intent)
            } catch (t: Throwable) {
                // Android 8+ 在后台调用 startService 会抛 IllegalStateException。
                // 此时应用必然在后台，服务通常已经在跑，忽略即可。
                Log.w(TAG, "start PlaybackService failed: ${t.message}")
            }
        }

        /** 记住最近一次文档型状态，供服务重建后重放 */
        private fun remember(action: String, payload: String) {
            synchronized(lastDoc) {
                lastDoc.remove(action)
                lastDoc[action] = payload
                while (lastDoc.size > MAX_DOC) {
                    val oldest = lastDoc.keys.firstOrNull() ?: break
                    lastDoc.remove(oldest)
                }
            }
        }

        /**
         * 服务刚创建时由 [PlaybackService.onCreate] 调用，把最近一次状态补上。
         *
         * 调用发生在主线程，且此时 `instance` 已经赋值。
         */
        internal fun replayInto(service: PlaybackService) {
            val snapshot = synchronized(lastDoc) { lastDoc.toMap() }
            snapshot["metadata"]?.let { raw ->
                parse(raw)?.let { service.updateMetadata(it) }
            }
            snapshot["state"]?.let { raw ->
                parse(raw)?.let { service.updateState(it.optBoolean("playing")) }
            }
            snapshot["position"]?.let { raw ->
                parse(raw)?.let {
                    service.updatePosition(it.optLong("positionMs"), it.optBoolean("playing"))
                }
            }
            Log.i(TAG, "replayed ${snapshot.size} cached state(s) into service")
        }

        private fun parse(payload: String): JSONObject? = try {
            JSONObject(payload)
        } catch (t: Throwable) {
            Log.w(TAG, "bad payload: $payload")
            null
        }

        // ------------------------------------------------------------------
        // Kotlin → Rust
        // ------------------------------------------------------------------

        /**
         * 通知按钮 / MediaSession / 耳机线控 / 音频焦点 的统一出口。
         *
         * 支持的 action：`play` `pause` `toggle` `next` `prev` `stop`
         * `focus-loss` `duck` `unduck` `seek:<毫秒>`
         */
        internal fun sendCommand(action: String) {
            try {
                nativeCommand(action)
            } catch (t: Throwable) {
                // 库没加载（比如热重载期间）不应该让通知点击崩掉
                Log.e(TAG, "nativeCommand($action) failed: ${t.message}")
            }
        }

        /** 通知栏、锁屏、耳机线控 → Rust，由 `bridge.rs` 导出同名 JNI 符号 */
        @JvmStatic
        external fun nativeCommand(action: String)

        /** 拉一次完整播放状态（WebView 重建后恢复 UI 用）。当前版本仅用于诊断。 */
        @JvmStatic
        external fun nativeSnapshot(): String
    }
}
