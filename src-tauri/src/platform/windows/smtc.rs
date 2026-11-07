//! Windows 系统媒体会话（SMTC）。
//!
//! Win32 桌面程序要拿到 `SystemMediaTransportControls` 必须走
//! `ISystemMediaTransportControlsInterop::GetForWindow`（UWP 的
//! `GetForCurrentView` 在桌面会直接失败）。
//!
//! 注册后可获得：
//! * 键盘媒体键（播放/暂停/上一首/下一首）
//! * 音量键弹出的媒体浮层里的曲目信息
//! * 锁屏 / 蓝牙耳机 / 部分笔记本功能键的控制入口

use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use windows::core::{factory, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
    SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
};
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

use crate::audio::engine::EngineCommand;
use crate::error::{AppError, AppResult};
use crate::models::{PlaybackStatus, PlayerSnapshot};

type ButtonHandler =
    TypedEventHandler<SystemMediaTransportControls, SystemMediaTransportControlsButtonPressedEventArgs>;

pub struct Smtc {
    controls: SystemMediaTransportControls,
    /// 事件 handler 必须在整个生命周期内保持存活，否则回调会被回收
    _handler: ButtonHandler,
}

// WinRT 的 SMTC 是 agile 对象，跨线程使用是安全的（COM 内部做 marshal）
unsafe impl Send for Smtc {}
unsafe impl Sync for Smtc {}

impl Smtc {
    pub fn new(raw_hwnd: isize, tx: Arc<Mutex<Option<Sender<EngineCommand>>>>) -> AppResult<Self> {
        let hwnd = HWND(raw_hwnd as *mut core::ffi::c_void);

        let interop: ISystemMediaTransportControlsInterop =
            factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()
                .map_err(|e| AppError::invalid(format!("SMTC factory: {e}")))?;

        let controls: SystemMediaTransportControls = unsafe {
            interop
                .GetForWindow(hwnd)
                .map_err(|e| AppError::invalid(format!("GetForWindow: {e}")))?
        };

        let _ = controls.SetIsEnabled(true);
        let _ = controls.SetIsPlayEnabled(true);
        let _ = controls.SetIsPauseEnabled(true);
        let _ = controls.SetIsStopEnabled(true);
        let _ = controls.SetIsNextEnabled(true);
        let _ = controls.SetIsPreviousEnabled(true);

        let handler = ButtonHandler::new(
            move |_sender: windows::core::Ref<'_, SystemMediaTransportControls>,
                  args: windows::core::Ref<'_, SystemMediaTransportControlsButtonPressedEventArgs>| {
                let Ok(args) = args.ok() else { return Ok(()) };
                let Ok(button) = args.Button() else { return Ok(()) };

                let cmd = if button == SystemMediaTransportControlsButton::Play {
                    Some(EngineCommand::Resume)
                } else if button == SystemMediaTransportControlsButton::Pause {
                    Some(EngineCommand::Pause)
                } else if button == SystemMediaTransportControlsButton::Next {
                    Some(EngineCommand::Next)
                } else if button == SystemMediaTransportControlsButton::Previous {
                    Some(EngineCommand::Prev)
                } else if button == SystemMediaTransportControlsButton::Stop {
                    Some(EngineCommand::Stop)
                } else {
                    None
                };

                if let Some(cmd) = cmd {
                    if let Some(tx) = tx.lock().as_ref() {
                        let _ = tx.send(cmd);
                    }
                }
                Ok(())
            },
        );

        controls
            .ButtonPressed(&handler)
            .map_err(|e| AppError::invalid(format!("ButtonPressed: {e}")))?;

        Ok(Self {
            controls,
            _handler: handler,
        })
    }

    pub fn update(&self, snap: &PlayerSnapshot) -> windows::core::Result<()> {
        let updater = self.controls.DisplayUpdater()?;
        updater.SetType(MediaPlaybackType::Music)?;

        if let Some(t) = &snap.track {
            let music = updater.MusicProperties()?;
            music.SetTitle(&HSTRING::from(t.title.as_str()))?;
            music.SetArtist(&HSTRING::from(t.artist.as_str()))?;
            music.SetAlbumTitle(&HSTRING::from(t.album.as_str()))?;

            // 封面：远程用 http(s) URI，本地用 file URI
            if let Some(cover) = t.cover_url.as_ref().filter(|c| c.starts_with("http")) {
                if let Ok(uri) = windows::Foundation::Uri::CreateUri(&HSTRING::from(cover.as_str())) {
                    if let Ok(stream) = RandomAccessStreamReference::CreateFromUri(&uri) {
                        let _ = updater.SetThumbnail(&stream);
                    }
                }
            } else if let Some(cover) = &t.cover_path {
                if let Ok(url) = url::Url::from_file_path(Path::new(cover)) {
                    if let Ok(uri) = windows::Foundation::Uri::CreateUri(&HSTRING::from(url.as_str()))
                    {
                        if let Ok(stream) = RandomAccessStreamReference::CreateFromUri(&uri) {
                            let _ = updater.SetThumbnail(&stream);
                        }
                    }
                }
            }
        } else {
            let music = updater.MusicProperties()?;
            music.SetTitle(&HSTRING::from("LeonMusic"))?;
        }

        updater.Update()?;

        let status = match snap.status {
            PlaybackStatus::Playing => MediaPlaybackStatus::Playing,
            PlaybackStatus::Paused => MediaPlaybackStatus::Paused,
            PlaybackStatus::Stopped | PlaybackStatus::Idle => MediaPlaybackStatus::Stopped,
            _ => MediaPlaybackStatus::Changing,
        };
        self.controls.SetPlaybackStatus(status)?;
        Ok(())
    }

    pub fn set_playing(&self, playing: bool) -> windows::core::Result<()> {
        let status = if playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        };
        self.controls.SetPlaybackStatus(status)
    }
}
