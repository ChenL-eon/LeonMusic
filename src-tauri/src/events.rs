//! 前后端事件名常量。
//!
//! 前端只监听这些事件，**不轮询**、不自己算播放进度。

/// 完整播放器快照（含当前曲目、状态、进度、音量、速度、音调）
pub const EVT_PLAYER_STATE: &str = "leon://player/state";
/// 队列变化（增删、排序、当前项变化）
pub const EVT_QUEUE: &str = "leon://player/queue";
/// 媒体库变化（扫描完成、导入/删除）
pub const EVT_LIBRARY_CHANGED: &str = "leon://library/changed";
/// 扫描进度
pub const EVT_SCAN_PROGRESS: &str = "leon://library/scan-progress";
/// 开始为在线曲目拉流（前端显示「缓冲中」）
pub const EVT_BUFFERING: &str = "leon://player/buffering";
/// 拉流进度 `{ trackId, downloaded, total }`
///
/// 流式版后暂无发送方（边下边播不等整曲），常量保留给未来的
/// 「下载进度可见化」（后台预热的进度条）。
#[allow(dead_code)]
pub const EVT_BUFFER_PROGRESS: &str = "leon://player/buffer-progress";
/// 播放结束（自然结束，用于统计/UI）
pub const EVT_TRACK_ENDED: &str = "leon://player/track-ended";
/// 引擎遇到「在线曲目还没落盘」，请求上层异步解析并回灌。
///
/// 为什么要在引擎外解析：解析要发 HTTP、要写文件，全是阻塞 IO；
/// 而引擎线程必须保持 8ms 节拍，不能等网络。
pub const EVT_RESOLVE_REQUEST: &str = "leon://player/resolve-request";
/// 在线曲目开播失败，交给上层按「智能音质」降档重试。
///
/// payload = Track（`qualityHint` 已经是下一档）。引擎不自己做网络，
/// 所以它只负责报告失败；真正的重解析在 `resolve_and_play` 里。
pub const EVT_PLAY_FAILED: &str = "leon://player/play-failed";
/// 设置已保存（payload = 完整 Settings）。
///
/// 主窗口与桌面歌词窗各自持有独立的 Pinia 实例，任何一窗改设置
/// （包括歌词窗工具条上的字号/锁定/置顶），另一窗靠这条事件实时跟随。
pub const EVT_SETTINGS_CHANGED: &str = "leon://settings/changed";
/// 应用内更新的下载进度 `{ downloaded, total }`（字节；total=0 表示服务端没给长度）。
pub const EVT_UPDATE_PROGRESS: &str = "leon://update/progress";

// 注：歌词不走事件推送。前端在 trackId 变化时主动 invoke `player_lyrics`，
// 因为歌词是「按需加载」的资源，推事件会白白解析每一首路过但不看的歌。
