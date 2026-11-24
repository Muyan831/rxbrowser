//! P0: WebEngine — ONE servo + ONE offscreen surface, many WebViews (tab-aligned).
//! Servo's Opts are a process-wide singleton → multiple views on one Servo.
//!
//! 三档分级休眠（老师定稿）：
//!   S1 媒体保护：view 缩 1×1（GPU 画面渲染≈0）但音频流不断 → 切回秒恢复
//!   S2 快照休眠：存 URL+scrollY 后整页释放 → 切回按快照原地恢复
//!   S3 深度冬眠：>5min 未激活或内存告警，同 S2 释放

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use servo::{
    DeviceIntRect, DevicePoint, InputEvent, JSValue, MediaSessionEvent, MediaSessionPlaybackState,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, RenderingContext, Servo,
    ServoBuilder, SoftwareRenderingContext, WebView, WebViewBuilder, WebViewDelegate, WheelDelta,
    WheelEvent, WindowRenderingContext, WheelMode,
};
use keyboard_types;
use url::Url;
use winit::dpi::PhysicalSize;

/// Bridges servo wake-ups into egui repaints (thread-safe: Context is Arc'd).
pub struct EguiWaker(pub egui::Context);

impl servo::EventLoopWaker for EguiWaker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(Self(self.0.clone()))
    }
    fn wake(&self) {
        self.0.request_repaint();
    }
}

struct FrameDelegate {
    title: Rc<RefCell<String>>,
    url: Rc<RefCell<String>>,
    fresh: Rc<Cell<bool>>,
    media: Rc<Cell<bool>>, // S1 判定
    favicon: Rc<RefCell<Option<(u32, u32, Vec<u8>, u64)>>>, // (w, h, rgba, 版本号)
    frame_ready: Rc<Cell<bool>>, // 有新帧才 read_to_image（切 tab 不读回=不卡）
}

impl WebViewDelegate for FrameDelegate {
    fn notify_favicon_changed(&self, view: WebView) {
        if let Some(img) = view.favicon() {
            let mut slot = self.favicon.borrow_mut();
            let ver = slot.as_ref().map(|f| f.3).unwrap_or(0) + 1;
            *slot = Some((img.width, img.height, img.data().to_vec(), ver));
        }
    }
    fn notify_new_frame_ready(&self, _view: WebView) {
        self.frame_ready.set(true);
    }
    fn notify_page_title_changed(&self, _view: WebView, title: Option<String>) {
        if let Some(t) = title {
            *self.title.borrow_mut() = t;
            self.fresh.set(true);
        }
    }
    fn notify_url_changed(&self, _view: WebView, url: Url) {
        *self.url.borrow_mut() = url.to_string();
        self.fresh.set(true);
    }
        fn notify_load_status_changed(&self, _view: WebView, _status: servo::LoadStatus) {
        self.fresh.set(true);
    }
    fn notify_media_session_event(&self, _view: WebView, event: MediaSessionEvent) {
        // W3C MediaSession —— S1 媒体检测的官方通道
        if let MediaSessionEvent::PlaybackStateChange(state) = event {
            // S1 保护只给真正出声的页（Playing）——Paused/None 不算
            self.media
                .set(matches!(state, MediaSessionPlaybackState::Playing));
        }
    }
}

/// S2/S3 快照：URL + 滚动位置（切回原地恢复）。
#[derive(Clone)]
pub struct Snapshot {
    pub url: String,
    pub scroll_y: f64,
}

struct Slot {
    view: WebView,
    delegate: Rc<FrameDelegate>,
    texture: Option<egui::TextureHandle>,
    /// S1：view 已缩 1×1（音频活、渲染≈0）
    shrunk: bool,
    last_paint: Instant,
}

pub struct WebEngine {
    servo: Servo,
    /// GPU Offscreen 优先，Software 兜底（统一 trait object）
    ctx: Rc<dyn RenderingContext>,
    /// GPU 锚点：HiddenWindow + winit event loop（保生命周期，永不显示）
    _event_loop: Option<winit::event_loop::EventLoop<()>>,
    _hidden_window: Option<winit::window::Window>,
    slots: Vec<Option<Slot>>,
    /// tab 索引对齐的快照（park 后存在，供原地恢复）
    pub snapshots: HashMap<usize, Snapshot>,
    size: (u32, u32),
    /// P0-2：resize 后等一帧再 read
    pending_resize: bool,
}

impl WebEngine {
    pub fn new(egui_ctx: &egui::Context, w: u32, h: u32) -> Result<Self, String> {
        // P0: GPU Offscreen 优先（显存纹理，系统 RAM 省），失败回退 Software。
        // eframe 不暴露窗口 handle → 自建 HiddenWindow 做 surfman GPU 锚点。
        let mut hidden_window = None;
        let mut event_loop = None;
        let ctx: Rc<dyn RenderingContext> = (|| -> Result<Rc<dyn RenderingContext>, String> {
            let el = winit::event_loop::EventLoop::new()
                .map_err(|e| format!("winit event loop: {e:?}"))?;
            let win = el
                .create_window(
                    winit::window::Window::default_attributes()
                        .with_visible(false)
                        .with_inner_size(PhysicalSize::new(w, h))
                        .with_title("rxb-gpu-anchor"),
                )
                .map_err(|e| format!("hidden window: {e:?}"))?;
            let (dh, wh) = unsafe {
                // SAFETY: handles 借自 win，win 存入 self 与 WebEngine 同生命周期
                (
                    std::mem::transmute::<_, raw_window_handle::DisplayHandle<'static>>(
                        win.display_handle().map_err(|e| format!("dh: {e:?}"))?,
                    ),
                    std::mem::transmute::<_, raw_window_handle::WindowHandle<'static>>(
                        win.window_handle().map_err(|e| format!("wh: {e:?}"))?,
                    ),
                )
            };
            event_loop = Some(el);
            hidden_window = Some(win);
            let wctx = WindowRenderingContext::new(dh, wh, PhysicalSize::new(w, h))
                .map_err(|e| format!("gpu wctx: {e:?}"))?;
            let rc_wctx = Rc::new(wctx);
            let offscreen = rc_wctx.offscreen_context(PhysicalSize::new(w, h));
            eprintln!("rxb: GPU offscreen path active");
            Ok(Rc::new(offscreen))
        })()
        .unwrap_or_else(|gpu_err| {
            eprintln!("rxb: GPU path failed ({gpu_err}), fallback software");
            Rc::new(
                SoftwareRenderingContext::new(PhysicalSize::new(w, h))
                    .expect("software context failed"),
            ) as Rc<dyn RenderingContext>
        });

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(EguiWaker(egui_ctx.clone())))
            .build();

        // RenderingContext must be current before servo paints.
        let _ = ctx.make_current();

        Ok(Self {
            servo,
            ctx,
            _event_loop: event_loop,
            _hidden_window: hidden_window,
            slots: Vec::new(),
            snapshots: HashMap::new(),
            size: (w, h),
            pending_resize: false,
        })
    }

    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }

    fn slot_mut(&mut self, idx: usize) -> Option<&mut Slot> {
        self.slots.get_mut(idx).and_then(|s| s.as_mut())
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w, h) == self.size {
            return;
        }
        self.ctx.resize(PhysicalSize::new(w, h)); // RenderingContext::resize（trait 方法）
        for slot in self.slots.iter().flatten() {
            if !slot.shrunk {
                slot.view.resize(PhysicalSize::new(w, h));
                slot.delegate.fresh.set(true);
            }
        }
        self.size = (w, h);
        self.pending_resize = true;
    }

    /// Ensure tab `idx` has a view; S2 恢复 = 重建后按快照 scroll。
    pub fn ensure_view(&mut self, idx: usize, first_url: &str) -> Result<(), String> {
        while self.slots.len() <= idx {
            self.slots.push(None);
        }
        if let Some(slot) = self.slots[idx].as_mut() {
            // S1 恢复：把缩过的 view 拉回原尺寸
            if slot.shrunk {
                let (w, h) = self.size;
                slot.view.resize(PhysicalSize::new(w, h));
                slot.shrunk = false;
                slot.delegate.fresh.set(true);
                self.pending_resize = true;
            }
            return Ok(());
        }

        let start = servo::ServoUrl::parse(first_url)
            .map_err(|e| format!("bad url {first_url}: {e:?}"))?;
        let delegate = Rc::new(FrameDelegate {
            title: Rc::new(RefCell::new(String::new())),
            url: Rc::new(RefCell::new(first_url.to_string())),
            fresh: Rc::new(Cell::new(true)),
            favicon: Rc::new(RefCell::new(None)),
            frame_ready: Rc::new(Cell::new(true)),
            media: Rc::new(Cell::new(false)),
        });
        let view = WebViewBuilder::new(&self.servo, self.ctx.clone())
            .url(start.into_url())
            .delegate(delegate.clone())
            .build();
        self.slots[idx] = Some(Slot {
            view,
            delegate,
            texture: None,
            shrunk: false,
            last_paint: Instant::now() - Duration::from_secs(1),
        });
        self.pending_resize = true;

        // S2 原地恢复：快照里有 scroll → load 完成后滚回
        let scroll_y = self.snapshots.get(&idx).map(|s| s.scroll_y).unwrap_or(0.0);
        if scroll_y > 0.0 {
            if let Some(slot) = self.slots[idx].as_ref() {
                let script = format!("window.scrollTo(0, {scroll_y});");
                slot.view.evaluate_javascript(script, |_| {});
            }
        }
        Ok(())
    }

    /// P0 三档分级休眠 —— tab 离开时调用。
    pub fn park(&mut self, idx: usize) {
        let media_playing = self.probe_media(idx);

        if media_playing {
            // S1 媒体保护：砍画面（1×1）保音频
            if let Some(slot) = self.slot_mut(idx) {
                if !slot.shrunk {
                    slot.view.resize(PhysicalSize::new(1, 1));
                    slot.shrunk = true;
                }
                slot.texture = None;
                slot.delegate.fresh.set(false);
            }
            self.pending_resize = true;
            return;
        }

        // S2 快照休眠：URL+scrollY 落快照后整页释放
        self.snapshot_now(idx);
        if idx < self.slots.len() {
            self.slots[idx] = None;
        }
    }

    /// 同步读 scrollY 并存快照（park 前调用）。
    fn snapshot_now(&mut self, idx: usize) {
        let url_now = self
            .slots
            .get(idx)
            .and_then(|s| s.as_ref())
            .map(|s| s.delegate.url.borrow().clone())
            .filter(|u| !u.is_empty())
            .or_else(|| self.snapshots.get(&idx).map(|s| s.url.clone()))
            .unwrap_or_default();

        let scroll_y = self.read_scroll_y(idx);

        self.snapshots.insert(
            idx,
            Snapshot {
                url: url_now,
                scroll_y,
            },
        );
    }

    /// 读当前 scrollY（JS 同步桥：evaluate 回调 + 短暂自旋等结果）。
    fn read_scroll_y(&mut self, idx: usize) -> f64 {
        let sink: Rc<RefCell<Option<f64>>> = Rc::new(RefCell::new(None));
        if let Some(slot) = self.slot_mut(idx) {
            let sink2 = sink.clone();
            slot.view.evaluate_javascript(
                "window.scrollY || document.documentElement.scrollTop || 0",
                move |res| {
                    if let Ok(v) = res {
                        if let JSValue::Number(n) = v {
                            *sink2.borrow_mut() = Some(n);
                        }
                    }
                },
            );
        }
        // servo 的 evaluate 是异步 callback；spin 几帧等它回来
        for _ in 0..20 {
            if sink.borrow().is_some() {
                break;
            }
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(2));
        }
        { let b = sink.borrow(); (*b).unwrap_or(0.0) }
    }

    /// 深度冬眠：全部非媒体页快照释放，媒体页 S1 缩身。
    /// `keep` = 永远不休眠的 tab（用户正在看的 active tab——释放它=页面空白+输入失效）。
    pub fn hibernate_all(&mut self, keep: usize) {
        eprintln!("rxb: hibernate_all fired, slots={} keep={keep}", self.slots.len());
        for idx in 0..self.slots.len() {
            eprintln!("rxb: hibernate loop idx={idx} is_some={}", self.slots[idx].is_some());
            if self.slots[idx].is_none() || idx == keep {
                continue;
            }
            eprintln!("rxb: hibernate probing {idx}...");
            let media = self.probe_media(idx);
            eprintln!("rxb: hibernate slot {idx} media={media}");
            if media {
                if let Some(slot) = self.slot_mut(idx) {
                    if !slot.shrunk {
                        slot.view.resize(PhysicalSize::new(1, 1));
                        slot.shrunk = true;
                    }
                    slot.texture = None;
                }
            } else {
                self.snapshot_now(idx);
                self.slots[idx] = None;
            }
        }
        self.pending_resize = true;
    }

    /// 关闭 tab：释放 + 快照清理 + 索引对齐（caller 负责 tabs 同步）。
    pub fn close_view(&mut self, idx: usize) {
        if idx < self.slots.len() {
            self.slots.remove(idx);
        }
        self.snapshots.remove(&idx);
    }

    /// 切换到 tab 时把它的快照字段标记（title/url 显示用）。
    /// Navigate a tab to a URL (creates the view if needed). Keeps snapshot URL in sync.
    pub fn navigate(&mut self, idx: usize, raw: &str) {
        let trimmed = raw.trim();
        let candidate = if trimmed.contains("://") {
            trimmed.to_string()
        } else if trimmed.contains('.') && !trimmed.contains(' ') {
            format!("https://{trimmed}")
        } else {
            format!("https://www.google.com/search?q={}", trimmed.replace(' ', "+"))
        };
        // 快照 URL 同步
        {
            let entry = self.snapshots.entry(idx).or_insert(Snapshot {
                url: candidate.clone(),
                scroll_y: 0.0,
            });
            entry.url = candidate.clone();
        }
        if self.slots.get(idx).and_then(|s| s.as_ref()).is_none() {
            if let Err(e) = self.ensure_view(idx, &candidate) {
                eprintln!("rxb: ensure_view failed: {e}");
                return;
            }
        }
        if let Some(slot) = self.slot_mut(idx) {
            match servo::ServoUrl::parse(&candidate) {
                Ok(u) => slot.view.load(u.into_url()),
                Err(e) => eprintln!("rxb: bad url {candidate}: {e:?}"),
            }
        }
    }

    pub fn page_url(&self, idx: usize) -> Option<String> {
        self.slots
            .get(idx)
            .and_then(|s| s.as_ref())
            .map(|s| s.delegate.url.borrow().clone())
            .filter(|u| !u.is_empty())
            .or_else(|| self.snapshots.get(&idx).map(|s| s.url.clone()))
    }

    pub fn favicon(&self, idx: usize) -> Option<(u32, u32, Vec<u8>, u64)> {
        self.slots
            .get(idx)
            .and_then(|s| s.as_ref())
            .map(|s| s.delegate.favicon.borrow().clone())
            .flatten()
    }

    pub fn title(&self, idx: usize) -> Option<String> {
        self.slots
            .get(idx)
            .and_then(|s| s.as_ref())
            .map(|s| s.delegate.title.borrow().clone())
            .filter(|t| !t.is_empty())
    }

    /// S1 媒体检测：只信 JS 真实播放状态（YouTube 等站 MediaSession 谎报 Playing）。
    pub fn probe_media(&mut self, idx: usize) -> bool {
        // JS 真实播放探测：任何 video/audio 在播且未静音
        eprintln!("rxb: probe_media({idx}) js probe start");
        let sink: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        if let Some(slot) = self.slot_mut(idx) {
            let sink2 = sink.clone();
            slot.view.evaluate_javascript(
                "[...document.querySelectorAll('video,audio')].some(m=>!m.paused&&!m.muted&&m.currentTime>0)",
                move |res| {
                    if let Ok(v) = res {
                        sink2.set(matches!(v, JSValue::Boolean(true)));
                    }
                },
            );
        }
        for _ in 0..15 {
            if sink.get() {
                break;
            }
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(2));
        }
        eprintln!("rxb: probe_media({idx}) result={}", sink.get());
        sink.get()
    }

    /// Paint tab `idx`'s view into the shared surface and read it back.
    pub fn sync_frame(
        &mut self,
        egui_ctx: &egui::Context,
        idx: usize,
    ) -> Option<egui::TextureHandle> {
        // P0-2：resize 后等一帧再 read（旧尺寸读回=灰帧/空帧）
        if self.pending_resize {
            self.pending_resize = false;
            return self
                .slots
                .get(idx)
                .and_then(|s| s.as_ref())
                .and_then(|s| s.texture.clone());
        }

        let (w, h) = self.size;
        let rect: DeviceIntRect = euclid::Box2D::from_size(euclid::Size2D::new(w as i32, h as i32));

        {
            let slot = self.slots.get_mut(idx)?.as_mut()?;
            if slot.shrunk {
                return None;
            }
            // 只在有新帧（frame_ready）或强制刷新（fresh）时才同步读回——
            // 静止页面/切 tab 一律直接返回缓存纹理，零阻塞（解决"切一页卡一次"）。
            let has_new_frame = slot.delegate.frame_ready.get();
            if !slot.delegate.fresh.get() && !has_new_frame {
                return slot.texture.clone();
            }
            slot.delegate.frame_ready.set(false);
            // Servo prepares the frame; the embedder paints it into the surface
            // before reading (M1-3 lesson: paint() then read/present).
            slot.view.paint();
        }

        let img = self.ctx.read_to_image(rect);
        if let Some(im) = img {
            let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], im.as_raw());
            let slot = self.slot_mut(idx)?;
            match &mut slot.texture {
                Some(t) => t.set(color, egui::TextureOptions::default()),
                None => {
                    slot.texture = Some(egui_ctx.load_texture(
                        format!("web-frame-{idx}"),
                        color,
                        egui::TextureOptions::default(),
                    ));
                }
            }
            slot.delegate.fresh.set(false);
            slot.last_paint = Instant::now();
        }
        self.slot_mut(idx).and_then(|s| s.texture.clone())
    }

    // ---------------- input forwarding (webview-local coords) ---------------

    fn view(&self, idx: usize) -> Option<&WebView> {
        let slot = self.slots.get(idx)?.as_ref()?;
        if slot.shrunk {
            return None;
        }
        Some(&slot.view)
    }

    pub fn mouse_move(&self, idx: usize, x: f32, y: f32) {
        eprintln!("rxb: MOUSE_MOVE at({x:.0},{y:.0})");
        if let Some(v) = self.view(idx) {
            v.notify_input_event(InputEvent::MouseMove(MouseMoveEvent {
                point: servo::WebViewPoint::Device(DevicePoint::new(x, y)),
                is_compatibility_event_for_touch: false,
            }));
        }
    }

    pub fn mouse_button(&self, idx: usize, pressed: bool, x: f32, y: f32) {
        eprintln!("rxb: MOUSE_BTN idx={idx} pressed={pressed} at({x:.0},{y:.0})");
        if let Some(v) = self.view(idx) {
            let action = if pressed {
                MouseButtonAction::Down
            } else {
                MouseButtonAction::Up
            };
            v.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                action,
                MouseButton::Primary,
                servo::WebViewPoint::Device(DevicePoint::new(x, y)),
            )));
        }
    }

    /// 键盘事件转发（打字/快捷键）—— egui Key 映射在 main.rs 做。
    pub fn key_event(
        &self,
        idx: usize,
        state: servo::KeyState,
        key: servo::Key,
        code: servo::Code,
        modifiers: servo::Modifiers,
    ) {
        if let Some(v) = self.view(idx) {
            v.notify_input_event(InputEvent::Keyboard(servo::KeyboardEvent::new_without_event(
                state,
                key,
                code,
                servo::Location::Standard,
                modifiers,
                false,
                false,
            )));
        }
    }

    /// 文本输入（IME Composition 路径）—— 字符进 input 的正道。
    pub fn ime_text(&self, idx: usize, text: &str) {
        if let Some(v) = self.view(idx) {
            // 规范：Start -> (Update*) -> End
            v.notify_input_event(InputEvent::Ime(servo::ImeEvent::Composition(
                keyboard_types::CompositionEvent {
                    data: text.to_string(),
                    state: keyboard_types::CompositionState::Start,
                },
            )));
            v.notify_input_event(InputEvent::Ime(servo::ImeEvent::Composition(
                keyboard_types::CompositionEvent {
                    data: text.to_string(),
                    state: keyboard_types::CompositionState::End,
                },
            )));
        }
    }

    pub fn wheel(&self, idx: usize, dx: f32, dy: f32, x: f32, y: f32) {
        if let Some(v) = self.view(idx) {
            v.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                WheelDelta {
                    x: dx as f64,
                    y: dy as f64,
                    z: 0.0,
                    mode: WheelMode::DeltaPixel,
                },
                servo::WebViewPoint::Device(DevicePoint::new(x, y)),
            )));
        }
    }

    pub fn go_back(&self, idx: usize) {
        if let Some(v) = self.view(idx) {
            v.go_back(1);
        }
    }

    pub fn go_forward(&self, idx: usize) {
        if let Some(v) = self.view(idx) {
            v.go_forward(1);
        }
    }

    pub fn reload(&self, idx: usize) {
        if let Some(v) = self.view(idx) {
            v.reload();
        }
    }
}
