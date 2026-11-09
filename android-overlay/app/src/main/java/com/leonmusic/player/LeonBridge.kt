package com.leonmusic.player

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log
import androidx.core.content.FileProvider
import org.json.JSONObject
import java.io.File

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

        /** 通知栏 / 锁屏开关存在原生侧：前台服务重启后不依赖 WebView 也能读到 */
        internal const val PREFS_MEDIA = "leon.media"
        internal const val KEY_NOTIFICATION = "notification"
        internal const val KEY_LOCKSCREEN = "lockscreen"

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

                // 设置页的「通知栏播放控制 / 锁屏播放状态」开关。
                // 存 SharedPreferences 而不是只留在 WebView：Activity 被回收、
                // 前台服务重建时，PlaybackService 自己读得到同一份值。
                "mediaPrefs" -> {
                    val json = parse(payload) ?: return
                    val ctx = appContext ?: return
                    ctx.getSharedPreferences(PREFS_MEDIA, Context.MODE_PRIVATE)
                        .edit()
                        .putBoolean(KEY_NOTIFICATION, json.optBoolean(KEY_NOTIFICATION, true))
                        .putBoolean(KEY_LOCKSCREEN, json.optBoolean(KEY_LOCKSCREEN, true))
                        .apply()
                    PlaybackService.instance?.onMediaPrefsChanged()
                }

                // 用户拒绝过 POST_NOTIFICATIONS：从设置页跳系统通知设置自行开启
                "notificationSettings" -> openNotificationSettings()

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
        // Rust 同步查询（设置页展示真实状态，而不是"应该支持"的静态文案）
        // ------------------------------------------------------------------

        /**
         * `bridge.rs::query_inner` 调的就是这个静态方法，签名必须是
         * `(Ljava/lang/String;)Ljava/lang/String;`。
         *
         * 只支持 `"media"`：返回通知权限、MediaSession、前台服务与两个开关。
         * 拿不到 Application context（Tauri setup 之前）时返回 null，
         * Rust 侧会退化成全 false，前端照常渲染。
         */
        @JvmStatic
        fun query(action: String): String? {
            if (action != "media") return null
            val ctx = appContext ?: return null
            val prefs = ctx.getSharedPreferences(PREFS_MEDIA, Context.MODE_PRIVATE)
            val svc = PlaybackService.instance
            return JSONObject()
                .put("notificationGranted", Permissions.canPostNotifications(ctx))
                .put("sessionActive", svc?.isSessionActive == true)
                .put("foreground", svc?.isForeground == true)
                .put("notificationControl", prefs.getBoolean(KEY_NOTIFICATION, true))
                .put("lockscreenControl", prefs.getBoolean(KEY_LOCKSCREEN, true))
                .toString()
        }

        /**
         * 把下载好的 APK 交给系统安装器（应用内更新）。
         *
         * `bridge.rs::install_apk_inner` 调的就是这个静态方法，签名必须是
         * `(Ljava/lang/String;)Ljava/lang/String;`。
         *
         * 返回空串 = 已拉起安装器；非空 = 给用户看的原因
         * （最常见的是还没授予「安装未知应用」）。**不抛异常**：调用方在
         * Rust 里，异常跨 JNI 传回去只会变成一句看不出原因的失败。
         *
         * 为什么必须走 FileProvider：下载目录是应用私有缓存，安装器是另一个
         * 进程，Android 7 起直接用 `file://` 会抛 FileUriExposedException。
         */
        @JvmStatic
        fun installApk(path: String): String {
            val ctx = appContext ?: return "应用还没有完全启动，请稍后重试"
            val file = File(path)
            if (!file.isFile) return "安装包不存在，请重新下载"

            // Android 8 起「安装未知应用」是按应用授权的：没授权直接拉起安装器
            // 会被系统静默拦掉，用户只会看到「什么都没发生」。
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
                !ctx.packageManager.canRequestPackageInstalls()
            ) {
                try {
                    ctx.startActivity(
                        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES)
                            .setData(Uri.parse("package:${ctx.packageName}"))
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                } catch (t: Throwable) {
                    Log.w(TAG, "open unknown-sources settings failed: ${t.message}")
                }
                return "请先允许「LeonMusic 安装应用」，返回后再点一次「立即更新」"
            }

            return try {
                val uri = FileProvider.getUriForFile(ctx, "${ctx.packageName}.fileprovider", file)
                val intent = Intent(Intent.ACTION_VIEW).apply {
                    setDataAndType(uri, "application/vnd.android.package-archive")
                    addFlags(
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK,
                    )
                }
                ctx.startActivity(intent)
                ""
            } catch (t: Throwable) {
                Log.e(TAG, "install apk failed: ${t.message}")
                "无法打开系统安装器：${t.message ?: "未知错误"}"
            }
        }

        /**
         * 打开系统通知设置。部分 OEM 没有 `ACTION_APP_NOTIFICATION_SETTINGS`，
         * 退回应用详情页，两条路都带 `FLAG_ACTIVITY_NEW_TASK`（从 Service 里跳）。
         */
        private fun openNotificationSettings() {
            val ctx = appContext ?: return
            val pkg = ctx.packageName
            val candidates = listOf(
                Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                    .putExtra(Settings.EXTRA_APP_PACKAGE, pkg),
                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
                    .setData(Uri.parse("package:$pkg")),
            )
            for (intent in candidates) {
                try {
                    intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    ctx.startActivity(intent)
                    return
                } catch (t: Throwable) {
                    Log.w(TAG, "open notification settings failed: ${t.message}")
                }
            }
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
