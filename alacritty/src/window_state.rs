//! 窗体位置/大小状态的持久化与消抖保存。
//!
//! 通过 `-i <win_id>` 指定窗体标识后，窗体移动或缩放时会将位置和大小写入
//! `$CONFIG_DIR/alacritty/window_state.json`，下次以相同 `win_id` 启动时
//! 自动恢复位置和大小。保存动作由 `fns::debounce` 在事件停止 500ms 后
//! 在独立线程中执行，避免阻塞主线程。

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

pub struct Debounce<T> {
    f: Arc<dyn Fn(T) + Send + Sync + 'static>,
    delay: Duration,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
    latest_arg: Arc<Mutex<Option<T>>>,
    _phantom: PhantomData<T>,
}

impl<T> Debounce<T>
where
    T: Send + Sync + Clone + 'static,
{
    pub fn new<F>(f: F, delay: Duration) -> Self
    where
        F: Fn(T) + Send + Sync + 'static,
    {
        Self {
            f: Arc::new(f),
            delay,
            task: Arc::new(Mutex::new(None)),
            latest_arg: Arc::new(Mutex::new(None)),
            _phantom: PhantomData,
        }
    }

    /// 触发防抖
    pub fn call(&self, arg: T) {
        // 更新最新参数
        *self.latest_arg.lock().unwrap() = Some(arg.clone());

        let f = self.f.clone();
        let delay = self.delay;
        let task = self.task.clone();
        let latest_arg = self.latest_arg.clone();

        // 终止上一轮等待任务
        if let Some(handle) = task.lock().unwrap().take() {
            handle.abort();
        }

        // 新建延时任务
        let new_handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // 取出最后一次传入的参数
            let maybe_arg = latest_arg.lock().unwrap().take();
            if let Some(val) = maybe_arg {
                f(val);
            }
        });

        *task.lock().unwrap() = Some(new_handle);
    }
}

/// 消抖延迟：移动/缩放事件停止 500ms 后才真正写入文件。
const SAVE_DEBOUNCE: Duration = Duration::from_millis(2000);

/// 单个窗体的位置和大小（屏幕物理像素）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StatePayload {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub id: String,
}

impl StatePayload {
    pub fn is_empty(&self) -> bool {
        self.x == 0 && self.y == 0 && self.width == 0
            && self.height == 0 && self.scale_factor == 0f64
    }
}

pub struct WindowState {
    pub payload: Arc<Mutex<StatePayload>>,
    deb: Debounce<()>,
}

impl WindowState {
    pub fn new(id: String) -> Self {
        let p = if let Some(mut pl) = load_state(&id) {
            pl.id = id;
            pl
        } else {
            StatePayload { x: 0, y: 0, width: 0, height: 0, scale_factor: 0f64, id }
        };
        let payload = Arc::new(Mutex::new(p));
        let payload_clone = payload.clone();
        let deb = Debounce::new(
            move |_| {
                let pd = payload_clone.lock().unwrap();
                save_state(&pd);
            },
            SAVE_DEBOUNCE,
        );

        Self { payload, deb }
    }

    pub fn update(&mut self, x: i32, y: i32, width: u32, height: u32,scale_factor: f64) {
        let mut pd = self.payload.lock().unwrap();
        pd.x = x;
        pd.y = y;
        pd.width = width;
        pd.height = height;
        pd.scale_factor = scale_factor;
        self.deb.call(());
    }

}

#[inline]
fn file_name(id:&str) -> String {
    format!("state.{}.json", id)
}

/// 窗体状态文件的路径。
///
/// - Windows: `%APPDATA%\alacritty\window_state.json`
/// - 其他: `$XDG_CONFIG_HOME/alacritty/window_state.json`
///   或 `$HOME/.config/alacritty/window_state.json`
pub fn state_file_path(id: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        dirs::config_dir().map(|p| p.join("alacritty").join(file_name(id)))
    }

    #[cfg(not(windows))]
    {
        Some(xdg::BaseDirectories::with_prefix("alacritty").get_config_file(file_name(id)))
    }
}

/// 读取并解析指定 id 的窗体状态。文件不存在或解析失败均返回 `None`。
pub fn load_state(id: &String) -> Option<StatePayload> {
    let path = state_file_path(id)?;
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<StatePayload>(&data).ok()
}

/// 将 payload 写入其 win_id 对应的独立文件 `state.<id>.json`。
///
/// 每个 win_id 独占一个文件，无需在读取时合并 HashMap。
/// 在消抖任务中调用，文件 I/O 不阻塞主线程。
pub fn save_state(payload: &StatePayload) {
    let id = payload.id.as_ref();
    let Some(path) = state_file_path(id) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(payload) {
        let _ = std::fs::write(&path, json);
    }
}
