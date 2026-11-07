package com.leonmusic.player

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.support.v4.media.MediaMetadataCompat
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import android.util.Log
import androidx.core.app.NotificationCompat
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLDecoder
import java.util.concurrent.Executors
import androidx.media.app.NotificationCompat as MediaNotificationCompat

/**
 * 前台播放服务。
 *
 * ## 它为什么存在（而不是让 Activity 管播放）
 *
 * Tauri 的 WebView 在 Android 上会被系统回收：切后台、内存压力、分屏切换都会触发。
 * 如果播放依赖 WebView 存活，Activity 一销毁音乐就断。所以：
 *
 * * **音频引擎跑在 Rust 线程里**，与 WebView 完全无关（`audio/engine.rs`）
 * * 这个 Service 只做三件事：把进程钉在前台、持有 `MediaSession`、显示通知
 * * WebView 只是遥控器，销毁重建后从 Rust 拉快照即可恢复 UI
 *
 * 因此 `onDestroy` 里**绝不能**停掉音频——那由 Rust 引擎自己决定。
 * 这里的销毁只意味着「通知栏和锁屏控制没了」。
 *
 * ## 通知为什么用 MediaStyle
 *
 * Android 10+ 的 SystemUI 会直接读 `MediaSession` 的 `PlaybackState` 渲染进度条、
 * 播放/暂停按钮，而不是读通知里的 action。所以 `pushState()` 的准确性比
 * 「频繁 notify」重要：这里只在状态/曲目变化时 notify，进度靠系统按 speed 插值。
 */
class PlaybackService : Service() {

    companion object {
        private const val TAG = "LeonPlayback"
        const val CHANNEL_ID = "leonmusic.playback"
        private const val NOTIF_ID = 0x6201

        private const val ACTION_PREV = "com.leonmusic.player.action.PREV"
        private const val ACTION_TOGGLE = "com.leonmusic.player.action.TOGGLE"
        private const val ACTION_NEXT = "com.leonmusic.player.action.NEXT"
        private const val ACTION_STOP = "com.leonmusic.player.action.STOP"

        /** 封面解码目标边长：通知大图 512px 足够，再大纯属浪费内存 */
        private const val COVER_TARGET = 512

        @Volatile
        var instance: PlaybackService? = null
            private set
    }

    private lateinit var session: MediaSessionCompat
    private lateinit var focus: AudioFocusHelper
    private val main = Handler(Looper.getMainLooper())
    private val coverExecutor = Executors.newSingleThreadExecutor { r ->
        Thread(r, "leon-cover").apply { isDaemon = true }
    }

    @Volatile private var playing = false
    @Volatile private var positionMs = 0L
    @Volatile private var durationMs = 0L
    @Volatile private var foreground = false

    private var title = "未在播放"
    private var artist = ""
    private var album = ""
    private var trackId = ""
    private var coverBitmap: Bitmap? = null

    // ----------------------------------------------------------------------
    // 生命周期
    // ----------------------------------------------------------------------

    override fun onCreate() {
        super.onCreate()
        instance = this
        createChannel()

        session = MediaSessionCompat(this, "LeonMusic").apply {
            setFlags(
                MediaSessionCompat.FLAG_HANDLES_MEDIA_BUTTONS or
                    MediaSessionCompat.FLAG_HANDLES_TRANSPORT_CONTROLS
            )
            setCallback(sessionCallback)
            isActive = true
        }

        focus = AudioFocusHelper(this, focusCallback)
        Log.i(TAG, "service created")

        // 补上「startService 之后、本服务真正就绪之前」那段窗口里丢掉的推送
        LeonBridge.replayInto(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_PREV -> command("prev")

            ACTION_TOGGLE -> {
                // 用户自己按的暂停，不能因为之后来了个电话就被"自动恢复"
                if (playing) focus.noteUserPaused()
                command("toggle")
            }

            ACTION_NEXT -> command("next")

            ACTION_STOP -> {
                focus.noteUserPaused()
                command("stop")
            }
        }
        // 只有真的在播才进前台。
        //
        // 冷启动时 [LeonBridge.attach] 用 startService 拉起本服务（不带
        // startForeground 的 5 秒契约），此时若强行 startForeground 会先弹一条
        // 「未在播放」的通知再立刻撤掉——用户看到的是通知栏闪一下。
        if (playing) enterForeground()
        // 进程被杀说明 Rust 引擎也没了，重启服务毫无意义（真正的续播靠下次启动恢复进度）
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        instance = null
        focus.abandon()
        session.isActive = false
        session.release()
        coverExecutor.shutdownNow()
        Log.i(TAG, "service destroyed")
        super.onDestroy()
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        // 用户从最近任务里划掉应用：如果还在播，让服务继续（这是音乐应用的正确行为）
        if (!playing) stopAll()
        super.onTaskRemoved(rootIntent)
    }

    // ----------------------------------------------------------------------
    // Rust → 这里
    // ----------------------------------------------------------------------

    fun updateMetadata(json: JSONObject) {
        title = json.optString("title").ifBlank { "未知曲目" }
        artist = json.optString("artist").ifBlank { "未知艺术家" }
        album = json.optString("album")
        durationMs = json.optLong("durationMs")
        positionMs = json.optLong("positionMs")
        playing = json.optBoolean("playing")
        trackId = json.optString("trackId")

        // 换歌先清封面，避免新歌配旧图
        coverBitmap = null

        session.setMetadata(
            MediaMetadataCompat.Builder()
                .putString(MediaMetadataCompat.METADATA_KEY_TITLE, title)
                .putString(MediaMetadataCompat.METADATA_KEY_ARTIST, artist)
                .putString(MediaMetadataCompat.METADATA_KEY_ALBUM, album)
                .putLong(MediaMetadataCompat.METADATA_KEY_DURATION, durationMs)
                .putString(MediaMetadataCompat.METADATA_KEY_MEDIA_ID, trackId)
                .build()
        )

        pushState()
        notifyNow()

        val cover = json.optString("coverUrl")
        if (cover.isNotBlank()) loadCover(cover, trackId)

        if (playing) {
            if (!focus.hasFocus) focus.request()
            enterForeground()
        }
    }

    /**
     * 播放/暂停变化。Rust 每次状态切换都会推一次。
     */
    fun updateState(playingNow: Boolean) {
        playing = playingNow
        main.post {
            if (playing && !focus.hasFocus) focus.request()
            pushState()
            notifyNow()
            if (playing) enterForeground()
        }
    }

    /**
     * 进度。Rust 侧约 900ms 推一次，这里再做一次去抖，
     * 避免拖动进度条时把 `setPlaybackState` 打成风暴。
     */
    fun updatePosition(ms: Long, playingNow: Boolean) {
        val jumped = kotlin.math.abs(ms - positionMs) >= 500
        if (!jumped && playingNow == playing) return
        positionMs = ms.coerceAtLeast(0)
        playing = playingNow
        main.post { pushState() }
    }

    /**
     * 队列清空 / 停止：撤掉通知里的曲目信息，**但服务本身留着**。
     *
     * 不能在这里 `stopSelf()`。Rust 的 ticker 在「当前没有曲目」时就会推 `clear`，
     * 冷启动阶段也会推一次；一旦 stopSelf，刚由 `attach` 拉起来的服务立刻被杀，
     * 紧接着用户点播放又要重新 start，MediaSession 也会闪断。
     *
     * 这里只做「退出前台 + 清空元数据」，进程不再被钉住，系统可以正常回收。
     * 真正销毁服务的是 [stopAll]（划掉任务且未在播）与 `onDestroy`。
     */
    fun clearMetadata() {
        main.post {
            resetMetadata()
            leaveForegroundNow()
        }
    }

    fun enterForeground() {
        if (foreground) return
        foreground = true
        main.post {
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    startForeground(
                        NOTIF_ID,
                        buildNotification(),
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK,
                    )
                } else {
                    startForeground(NOTIF_ID, buildNotification())
                }
            } catch (t: Throwable) {
                // Android 14 起缺 FOREGROUND_SERVICE_MEDIA_PLAYBACK 会抛 SecurityException
                foreground = false
                Log.w(TAG, "startForeground failed: ${t.message}")
            }
        }
    }

    fun leaveForeground() {
        if (!foreground) return
        foreground = false
        main.post {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                // 保留通知（用户可能马上又播），但不再钉住进程
                stopForeground(STOP_FOREGROUND_DETACH)
            } else {
                @Suppress("DEPRECATION")
                stopForeground(false)
            }
        }
    }

    // ----------------------------------------------------------------------
    // 内部
    // ----------------------------------------------------------------------

    private fun command(action: String) = LeonBridge.sendCommand(action)

    private val sessionCallback = object : MediaSessionCompat.Callback() {
        override fun onPlay() = command("play")

        override fun onPause() {
            focus.noteUserPaused()
            command("pause")
        }

        override fun onStop() {
            focus.noteUserPaused()
            command("stop")
        }

        override fun onSkipToNext() = command("next")

        override fun onSkipToPrevious() = command("prev")

        override fun onSeekTo(pos: Long) {
            val target = pos.coerceAtLeast(0)
            // 先本地更新，避免锁屏进度条在 Rust 回报前弹回旧位置
            positionMs = target
            pushState()
            command("seek:$target")
        }
    }

    private val focusCallback = object : AudioFocusHelper.Callback {
        override fun onFocusGain() = command("play")

        override fun onFocusLoss() = command("pause")

        override fun onFocusLossTransient() {
            // 不调 noteUserPaused：Rust 回报的暂停是「被动」的，焦点回来要继续
            command("pause")
        }

        override fun onFocusDuck() = command("duck")

        override fun onFocusUnduck() = command("unduck")
    }

    private fun pushState() {
        val state = when {
            playing -> PlaybackStateCompat.STATE_PLAYING
            trackId.isNotEmpty() -> PlaybackStateCompat.STATE_PAUSED
            else -> PlaybackStateCompat.STATE_NONE
        }
        val builder = PlaybackStateCompat.Builder()
            .setActions(ACTIONS)
            .setState(
                state,
                positionMs,
                if (playing) 1.0f else 0.0f,
                SystemClock.elapsedRealtime(),
            )
        session.setPlaybackState(builder.build())
    }

    /** 清空曲目信息并把 PlaybackState 归零（必须在主线程调用） */
    private fun resetMetadata() {
        playing = false
        positionMs = 0
        durationMs = 0
        trackId = ""
        coverBitmap = null
        title = "未在播放"
        artist = ""
        album = ""
        session.setMetadata(MediaMetadataCompat.Builder().build())
        session.setPlaybackState(
            PlaybackStateCompat.Builder()
                .setActions(ACTIONS)
                .setState(PlaybackStateCompat.STATE_NONE, 0, 0f)
                .build()
        )
    }

    /** 退出前台并撤掉通知（必须在主线程调用） */
    private fun leaveForegroundNow() {
        if (!foreground) return
        foreground = false
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                stopForeground(STOP_FOREGROUND_REMOVE)
            } else {
                @Suppress("DEPRECATION")
                stopForeground(true)
            }
        } catch (t: Throwable) {
            Log.w(TAG, "stopForeground failed: ${t.message}")
        }
    }

    private fun stopAll() {
        resetMetadata()
        leaveForegroundNow()
        stopSelf()
    }

    // ----------------------------------------------------------------------
    // 通知
    // ----------------------------------------------------------------------

    private fun createChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (nm.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "播放控制",
            NotificationManager.IMPORTANCE_LOW, // LOW：不出横幅、不发声，只在通知栏常驻
        ).apply {
            description = "显示当前播放的歌曲与播放控制"
            setShowBadge(false)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        nm.createNotificationChannel(channel)
    }

    private fun buildNotification(): Notification {
        val toggleIcon =
            if (playing) android.R.drawable.ic_media_pause else android.R.drawable.ic_media_play
        val toggleLabel = if (playing) "暂停" else "播放"
        val subtitle = if (album.isBlank()) artist else "$artist · $album"

        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(smallIcon())
            .setContentTitle(title)
            .setContentText(subtitle)
            .setLargeIcon(coverBitmap)
            .setContentIntent(openIntent())
            .setDeleteIntent(serviceIntent(ACTION_STOP))
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setSilent(true)
            .addAction(android.R.drawable.ic_media_previous, "上一首", serviceIntent(ACTION_PREV))
            .addAction(toggleIcon, toggleLabel, serviceIntent(ACTION_TOGGLE))
            .addAction(android.R.drawable.ic_media_next, "下一首", serviceIntent(ACTION_NEXT))
            .setStyle(
                MediaNotificationCompat.MediaStyle()
                    .setMediaSession(session.sessionToken)
                    .setShowActionsInCompactView(0, 1, 2)
            )
            .build()
    }

    /** 通知小图标必须是单色 alpha 图：直接复用应用图标，避免额外资源依赖 */
    private fun smallIcon(): Int {
        val icon = applicationInfo.icon
        return if (icon != 0) icon else android.R.drawable.ic_media_play
    }

    private fun notifyNow() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        try {
            nm.notify(NOTIF_ID, buildNotification())
        } catch (t: Throwable) {
            // Android 13+ 没授 POST_NOTIFICATIONS 时这里会静默失败，播放不受影响
            Log.w(TAG, "notify failed: ${t.message}")
        }
    }

    private fun serviceIntent(action: String): PendingIntent {
        val intent = Intent(this, PlaybackService::class.java).setAction(action)
        return PendingIntent.getService(
            this,
            action.hashCode(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun openIntent(): PendingIntent? {
        val launch = packageManager.getLaunchIntentForPackage(packageName) ?: return null
        launch.flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
        return PendingIntent.getActivity(
            this,
            0,
            launch,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    // ----------------------------------------------------------------------
    // 封面
    // ----------------------------------------------------------------------

    /**
     * 封面来源有三种形态，都要能处理：
     * * 本地绝对路径（`/data/user/0/.../covers/xx.jpg`）
     * * Tauri asset 协议（`http://asset.localhost/...` 或 `asset://localhost/...`）
     * * 在线 URL（网易云等音源给的 `coverUrl`）
     */
    private fun loadCover(raw: String, forTrackId: String) {
        coverExecutor.execute {
            val bitmap = decodeCover(raw)
            if (bitmap == null) return@execute
            // 解码期间用户可能已经切歌，别把旧图贴上去
            if (forTrackId != trackId) return@execute
            coverBitmap = bitmap
            main.post { notifyNow() }
        }
    }

    private fun decodeCover(raw: String): Bitmap? = try {
        val lower = raw.lowercase()
        when {
            lower.startsWith("asset://") -> assetPath(raw)?.let(::decodeFile)

            lower.startsWith("http://") || lower.startsWith("https://") ->
                if (lower.contains("asset.localhost")) {
                    assetPath(raw)?.let(::decodeFile)
                } else {
                    decodeRemote(raw)
                }

            else -> decodeFile(File(raw.removePrefix("file://")))
        }
    } catch (t: Throwable) {
        Log.w(TAG, "decode cover failed: ${t.message}")
        null
    }

    private fun assetPath(raw: String): File? = try {
        var path = Uri.parse(raw).path ?: return null
        path = URLDecoder.decode(path, "UTF-8")
        // 某些平台会给出 /C:/... 形式，去掉多余的前导斜杠
        if (path.length >= 3 && path[0] == '/' && path[2] == ':') path = path.substring(1)
        File(path)
    } catch (t: Throwable) {
        null
    }

    private fun decodeFile(file: File): Bitmap? {
        if (!file.isFile) return null
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        val opts = BitmapFactory.Options().apply {
            inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight)
        }
        return BitmapFactory.decodeFile(file.absolutePath, opts)
    }

    private fun decodeRemote(url: String): Bitmap? {
        var conn: HttpURLConnection? = null
        return try {
            val c = (URL(url).openConnection() as HttpURLConnection).apply {
                connectTimeout = 8000
                readTimeout = 8000
                instanceFollowRedirects = true
            }
            conn = c
            val bytes = c.inputStream.use { it.readBytes() }
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
            val opts = BitmapFactory.Options().apply {
                inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight)
            }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
        } catch (t: Throwable) {
            Log.w(TAG, "cover download failed: ${t.message}")
            null
        } finally {
            conn?.disconnect()
        }
    }

    private fun sampleSize(width: Int, height: Int): Int {
        if (width <= 0 || height <= 0) return 1
        var sample = 1
        var longest = maxOf(width, height)
        while (longest / 2 >= COVER_TARGET) {
            sample *= 2
            longest /= 2
        }
        return sample
    }
}

/** MediaSession 声明支持的传输控制能力，SystemUI 据此决定显示哪些按钮 */
private const val ACTIONS: Long =
    PlaybackStateCompat.ACTION_PLAY or
        PlaybackStateCompat.ACTION_PAUSE or
        PlaybackStateCompat.ACTION_PLAY_PAUSE or
        PlaybackStateCompat.ACTION_STOP or
        PlaybackStateCompat.ACTION_SKIP_TO_NEXT or
        PlaybackStateCompat.ACTION_SKIP_TO_PREVIOUS or
        PlaybackStateCompat.ACTION_SEEK_TO
