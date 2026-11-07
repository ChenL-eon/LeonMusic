package com.leonmusic.player

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * 音频焦点管理。
 *
 * Android 的焦点是「协作式」的：想要出声就得先申请，被别人抢占时要按类型让位。
 * 这里把系统的五种回调翻译成播放器该做的事：
 *
 * | 系统事件 | 真实场景 | 播放器行为 |
 * |---|---|---|
 * | `AUDIOFOCUS_GAIN` | 来电结束 / 其他应用播完 | 恢复播放（仅当是**我们**暂停的） |
 * | `AUDIOFOCUS_LOSS_TRANSIENT` | 来电、微信语音 | 暂停，记住待恢复 |
 * | `AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK` | 导航播报、通知提示音 | **只压低音量**，不打断 |
 * | `AUDIOFOCUS_LOSS` | 其他播放器抢走 | 暂停并放弃焦点 |
 *
 * 关键点：`resumeWhenGain` 只在「因焦点丢失而暂停」时置位。
 * 用户自己按的暂停，不能被一次来电恢复成播放——这是很多播放器的经典 bug。
 */
class AudioFocusHelper(
    context: Context,
    private val events: Callback,
) {

    interface Callback {
        /** 焦点回来了，且上次是「被动暂停」→ 可以继续播 */
        fun onFocusGain()

        /** 永久失去焦点：暂停，不自动恢复 */
        fun onFocusLoss()

        /** 短暂失去（来电）：暂停，可恢复 */
        fun onFocusLossTransient()

        /** 可 duck 的短暂失去（导航播报）：压低音量 */
        fun onFocusDuck()

        /** duck 结束，恢复音量 */
        fun onFocusUnduck()
    }

    private val appContext = context.applicationContext
    private val audioManager =
        appContext.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val main = Handler(Looper.getMainLooper())

    private var focusRequest: AudioFocusRequest? = null

    @Volatile
    private var focused = false

    /** 被动暂停标记：只有它为 true 时，GAIN 才自动续播 */
    @Volatile
    private var resumeWhenGain = false

    /** 当前是否处于 duck 状态，避免重复下发 */
    @Volatile
    private var ducked = false

    val hasFocus: Boolean get() = focused

    private val listener = AudioManager.OnAudioFocusChangeListener { change ->
        main.post { handleChange(change) }
    }

    private fun handleChange(change: Int) {
        Log.d(TAG, "focus change: ${nameOf(change)}")
        when (change) {
            AudioManager.AUDIOFOCUS_GAIN -> {
                focused = true
                if (ducked) {
                    ducked = false
                    events.onFocusUnduck()
                }
                if (resumeWhenGain) {
                    resumeWhenGain = false
                    events.onFocusGain()
                }
            }

            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                focused = false
                // 只有正在播放才需要记「待恢复」，否则会把用户的暂停改写成播放
                resumeWhenGain = true
                events.onFocusLossTransient()
            }

            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> {
                // 不放弃焦点，只让声音
                if (!ducked) {
                    ducked = true
                    events.onFocusDuck()
                }
            }

            AudioManager.AUDIOFOCUS_LOSS -> {
                focused = false
                ducked = false
                resumeWhenGain = false
                events.onFocusLoss()
            }
        }
    }

    /**
     * 申请焦点。返回是否拿到。
     *
     * 注意 `AUDIOFOCUS_GAIN`（而非 GAIN_TRANSIENT）：音乐播放要长期持有，
     * 这样切到别的应用时对方才会请求我们让位，而不是两边同时出声。
     */
    fun request(): Boolean {
        if (focused) return true
        val result = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val attrs = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build()
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(attrs)
                .setAcceptsDelayedFocusGain(false)
                // true 表示「系统帮我 duck」，但我们要自己做平滑淡出，所以设 false
                .setWillPauseWhenDucked(false)
                .setOnAudioFocusChangeListener(listener)
                .build()
            focusRequest = request
            audioManager.requestAudioFocus(request)
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(
                listener,
                AudioManager.STREAM_MUSIC,
                AudioManager.AUDIOFOCUS_GAIN,
            )
        }
        focused = result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        if (!focused) Log.w(TAG, "focus request denied")
        return focused
    }

    /** 暂停/停止时放弃焦点，让别人能正常出声（尤其是通话）。 */
    fun abandon() {
        if (!focused && !ducked) return
        focused = false
        ducked = false
        resumeWhenGain = false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            focusRequest?.let { audioManager.abandonAudioFocusRequest(it) }
        } else {
            @Suppress("DEPRECATION")
            audioManager.abandonAudioFocus(listener)
        }
    }

    /** 用户主动暂停：清掉待恢复标记，避免来电结束后自己响起来。 */
    fun noteUserPaused() {
        resumeWhenGain = false
    }

    private fun nameOf(change: Int): String = when (change) {
        AudioManager.AUDIOFOCUS_GAIN -> "GAIN"
        AudioManager.AUDIOFOCUS_LOSS -> "LOSS"
        AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> "LOSS_TRANSIENT"
        AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> "LOSS_TRANSIENT_CAN_DUCK"
        else -> "UNKNOWN($change)"
    }

    companion object {
        private const val TAG = "LeonFocus"
    }
}
