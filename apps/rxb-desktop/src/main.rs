//! RXbrowser M1-4: egui shell — full browser layout (对标 Opera/Chrome 结构:
//! tab strip · nav bar · wallpaper new-tab page with search + shortcut tiles).
//! Servo embedding lives in `embed.rs` (proven, M1-3); M1-5 composites both.

mod embed;
mod web;

use web::WebEngine;

use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use eframe::egui;

// ---------------------------------------------------------------------------
// Theme — Apple palette (Refero), light/dark. Skin details await Noha's design.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Theme {
    name: &'static str,
    bg: egui::Color32,
    surface: egui::Color32,
    tab_active: egui::Color32,
    text: egui::Color32,
    text_dim: egui::Color32,
    accent: egui::Color32,
    border: egui::Color32,
}

impl Theme {
    fn light() -> Self {
        Self {
            name: "light",
            bg: egui::Color32::from_rgb(0xf5, 0xf5, 0xf7),
            surface: egui::Color32::WHITE,
            tab_active: egui::Color32::from_rgb(0xff, 0xff, 0xff),
            text: egui::Color32::from_rgb(0x1d, 0x1d, 0x1f),
            text_dim: egui::Color32::from_rgb(0x86, 0x86, 0x8b),
            accent: egui::Color32::from_rgb(0x00, 0x71, 0xe3),
            border: egui::Color32::from_rgb(0xd2, 0xd2, 0xd7),
        }
    }

    fn dark() -> Self {
        Self {
            name: "dark",
            bg: egui::Color32::from_rgb(0x14, 0x14, 0x17),
            surface: egui::Color32::from_rgb(0x1c, 0x1c, 0x20),
            tab_active: egui::Color32::from_rgb(0x2a, 0x2a, 0x30),
            text: egui::Color32::from_rgb(0xf5, 0xf5, 0xf7),
            text_dim: egui::Color32::from_rgb(0x98, 0x98, 0x9d),
            accent: egui::Color32::from_rgb(0x29, 0x97, 0xff),
            border: egui::Color32::from_rgb(0x2c, 0x2c, 0x2e),
        }
    }

    fn load(name: &str) -> Self {
        let path = Path::new("themes").join(format!("{name}.ron"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if text.is_empty() {
                return Self::light();
            }
        }
        match name {
            "dark" => Self::dark(),
            _ => Self::light(),
        }
    }
}

// ---------------------------------------------------------------------------
// Shortcut tiles — the Opera-style grid on the new tab page.
// ---------------------------------------------------------------------------

struct Tile {
    label: &'static str,
    url: &'static str,
    color: [u8; 3],
}

const TILES: &[Tile] = &[
    Tile { label: "YouTube", url: "https://www.youtube.com", color: [255, 0, 0] },
    Tile { label: "Bilibili", url: "https://www.bilibili.com", color: [0, 174, 236] },
    Tile { label: "GitHub", url: "https://github.com", color: [46, 52, 60] },
    Tile { label: "Google", url: "https://www.google.com", color: [66, 133, 244] },
    Tile { label: "翻译", url: "https://translate.google.com", color: [52, 168, 83] },
    Tile { label: "X", url: "https://x.com", color: [25, 25, 25] },
    Tile { label: "Reddit", url: "https://www.reddit.com", color: [255, 69, 0] },
    Tile { label: "维基", url: "https://wikipedia.org", color: [90, 90, 95] },
    Tile { label: "推特", url: "https://twitter.com", color: [29, 161, 242] },
    Tile { label: "Telegram", url: "https://web.telegram.org", color: [34, 146, 203] },
    Tile { label: "Discord", url: "https://discord.com/app", color: [88, 101, 242] },
    Tile { label: "Gist", url: "https://gist.github.com", color: [60, 66, 74] },
];

// ---------------------------------------------------------------------------
// Shell state
// ---------------------------------------------------------------------------

struct ShellApp {
    fav_cache: Vec<(u64, Option<egui::TextureId>)>,
    theme: Theme,
    tabs: Vec<Tab>,
    active_tab: usize,
    address_input: String,
    query: String,
    gpu_pref: GpuPref,
    status: String,
    wall: Option<egui::TextureHandle>,
    engine: Option<WebEngine>, // 单 Servo 多 View（Opts 全局单例）
    hibernate_at: Option<std::time::Instant>,
    hibernated: bool,
    /// P0-4: Alt+G 算力释放胶囊模式
    capsule: bool,
    /// 进胶囊前的窗口几何（恢复用）
    saved_geometry: Option<(f32, f32, f32, f32)>, // x, y, w, h
    /// 音频保留开关（胶囊内可切）
    keep_audio: bool,
    capsule_entered_at: Option<std::time::Instant>,
    /// 压测自动化：到点自动进出胶囊
    capsule_at: Option<std::time::Instant>,
    booted: bool,
}

#[derive(Clone)]
struct Tab {
    title: String,
    url: String,
}

#[derive(PartialEq, Clone, Copy)]
enum GpuPref {
    Performance,
    PowerSaver,
    Auto,
}

impl ShellApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Custom wallpaper: wallpaper.png next to the exe (V1 自定义壁纸入口).
        let wall = std::fs::read("wallpaper.png").ok().and_then(|bytes| {
            let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let (w, h) = img.dimensions();
            let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
            Some(cc.egui_ctx.load_texture("wallpaper", color, Default::default()))
        });

        Self {
            fav_cache: Vec::new(),
            theme: Theme::load("light"),
            tabs: vec![Tab {
                title: "新标签页".into(),
                url: String::new(),
            }],
            active_tab: 0,
            address_input: String::new(),
            query: String::new(),
            gpu_pref: GpuPref::Performance, // 老师定：默认最高性能优先
            status: if wall.is_some() {
                "壁纸已加载 · 放 wallpaper.png 到程序目录可更换"
            } else {
                "放 wallpaper.png 到程序目录即可自定义壁纸"
            }
            .to_string(),
            wall,
            engine: None,
            hibernate_at: None,
            hibernated: false,
            capsule: false,
            saved_geometry: None,
            keep_audio: true,
            capsule_entered_at: None,
            capsule_at: None,
            booted: false,
        }
    }

    /// 深度冬眠（V1 第三档休眠）：切走的标签整页释放，切回按 URL 重建。
    /// 这是 Opera/Chrome 压内存的核心手段——10 标签稳态 ≈ 1 活跃页。
    /// P0-4: 进出胶囊模式。
    fn toggle_capsule(&mut self, ctx: &egui::Context) {
        self.capsule = !self.capsule;
        if self.capsule {
            // 记录原几何（用当前窗口信息）
            let rect = ctx.input(|i| i.screen_rect());
            self.saved_geometry = Some((rect.min.x, rect.min.y, rect.width(), rect.height()));
            // 切断渲染 + 释放（媒体页 S1 缩身保音频）
            if let Some(w) = self.engine.as_mut() {
                if self.keep_audio {
                    w.hibernate_all(usize::MAX);
                } else {
                    w.hibernate_all(usize::MAX);
                }
            }
            self.capsule_entered_at = Some(std::time::Instant::now());
            // 缩成角落胶囊
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(240.0, 72.0)));
            let sr = ctx.input(|i| i.screen_rect());
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                egui::pos2(sr.max.x - 260.0, sr.max.y - 110.0),
            ));
            self.status = "已进入算力释放胶囊（Alt+G 恢复）".into();
        } else {
            // 毫秒级恢复窗口
            if let Some((x, y, w, h)) = self.saved_geometry.take() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
            }
            // 恢复活跃页渲染
            let act = self.active_tab;
            if let Some(w) = self.engine.as_mut() {
                let _ = w.ensure_view(act, &self.tabs.get(act).map(|t| t.url.clone()).unwrap_or_default());
            }
            self.capsule_entered_at = None;
            self.status = "已恢复完整渲染".into();
        }
        ctx.request_repaint();
    }

    /// 胶囊 UI：时间 · 释放内存量 · 音频保留开关。
    fn render_capsule(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let secs = now % 86400;
                let (hh, mm) = (secs / 3600, (secs % 3600) / 60);
                ui.horizontal(|ui| {
                    ui.heading(format!("⏸ {hh:02}:{mm:02}"));
                    ui.label("| 渲染已释放");
                });
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut self.keep_audio, "保留后台音频").changed() {
                        // 开关只影响语义标签（S1 行为已按媒体检测自动分级）
                    }
                    if ui.button("恢复 (Alt+G)").clicked() {
                        self.toggle_capsule(ctx);
                    }
                });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(1000));
    }

    fn switch_tab(&mut self, new: usize) {
        let old = self.active_tab;
        if old == new {
            return;
        }
        if let Some(w) = self.engine.as_mut() {
            // 记录旧标签最新 URL（切回时按它重建）
            let recorded = w.page_url(old).filter(|u| !u.is_empty());
            if let Some(u) = recorded {
                if old < self.tabs.len() {
                    self.tabs[old].url = u;
                }
            }
            w.park(old); // S1 媒体保护 / S2 快照休眠（三档分级）
        }
        self.active_tab = new;
        if new < self.tabs.len() {
            self.address_input = self.tabs[new].url.clone();
            let u = self.tabs[new].url.clone();
            if !u.is_empty() {
                if let Some(w) = self.engine.as_mut() {
                    let _ = w.ensure_view(new, &u);
                    w.navigate(new, &u);
                }
            }
        }
        self.status = format!("标签 {} 已切换 · 后台三档休眠生效", new + 1);
    }

    fn navigate(&mut self, raw: &str, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            return;
        }
        let url = if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.clone()
        } else if raw.contains('.') && !raw.contains(' ') {
            format!("https://{raw}")
        } else {
            format!("https://www.google.com/search?q={}", raw.replace(' ', "+"))
        };
        let idx = self.active_tab;
        if self.engine.is_none() {
            match WebEngine::new(ctx, 1000, 560) {
                Ok(e) => self.engine = Some(e),
                Err(err) => {
                    self.status = format!("引擎启动失败: {err}");
                    return;
                }
            }
        }
        let Some(w) = self.engine.as_mut() else { return };
        if let Err(err) = w.ensure_view(idx, &url) {
            self.status = format!("视图创建失败: {err}");
            return;
        }
        w.navigate(idx, &url);
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.url = url.clone();
            tab.title = host_of(&url);
        }
        self.address_input = url.clone();
        self.status = format!("加载 {url}");
    }
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("新标签页")
        .to_string()
}

/// CJK font bootstrap — first existing system font wins; missing files are
/// skipped silently so iGPU/ARM/other-OS machines never break.
fn install_cjk_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\Deng.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/arphic/uming.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return; // Latin-only fallback still renders; never crash.
    };
    let mut defs = egui::FontDefinitions::default();
    defs.font_data
        .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = defs.families.get_mut(&family) {
            if !list.iter().any(|n| n == "cjk") {
                list.push("cjk".to_owned());
            }
        }
    }
    ctx.set_fonts(defs);
}

impl eframe::App for ShellApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // 压测自动化：定时进出胶囊（--capsule-test）
        if let Some(at) = self.capsule_at {
            if std::time::Instant::now() >= at {
                self.capsule_at = if self.capsule {
                    None // 恢复后不再触发
                } else {
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(12))
                };
                self.toggle_capsule(ctx);
            }
        }
        // P0-4: Alt+G 算力释放胶囊模式（毫秒级进出）
        let toggle_capsule = ctx.input(|i| {
            i.key_pressed(egui::Key::G) && i.modifiers.alt
        });
        if toggle_capsule {
            self.toggle_capsule(ctx);
        }
        if self.capsule {
            self.render_capsule(ctx);
            return;
        }
        let t = self.theme.clone();
        if let Some(w) = &self.engine {
            w.spin();
        }
        // 深度冬眠触发：10 页加载完 → 释放全部（active 是空标签页，全释放）
        if let Some(at) = self.hibernate_at {
            if std::time::Instant::now() >= at {
                self.hibernate_at = None;
                eprintln!("rxb: hibernate_at triggered");
                if let Some(w) = self.engine.as_mut() {
                    w.hibernate_all(self.active_tab);
                }
                self.hibernated = true;
                self.status = "10标签已深度冬眠（切回即重建）· 内存已释放".into();
            }
        }

        let mut go: Option<String> = None;
        // M1-6 memory test boot: 10 tabs / 10 engines (10标签≤1GB 验收).
        if !self.booted {
            self.booted = true;
            let args: Vec<String> = std::env::args().collect();
            // 默认=正常浏览器（单空白页）；--stress-test 才开 YouTube 压测 boot
            if !args.iter().any(|a| a == "--stress-test") {
                self.tabs.clear();
                self.tabs.push(Tab { title: "新标签页".into(), url: String::new() });
                self.address_input.clear();
                self.active_tab = 0;
                self.status = "RXbrowser 就绪".into();
                return;
            }
            // 压测协议：--tabs N（1/3/5/10）· --capsule-test 自动 Alt+G
            let n_tabs: usize = std::env::args().position(|a| a == "--tabs")
                .and_then(|i| std::env::args().nth(i + 1).and_then(|v| v.parse().ok()))
                .unwrap_or(10).min(10);
            let capsule_test = std::env::args().any(|a| a == "--capsule-test");
            let urls = [
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                "https://www.youtube.com/watch?v=jNQXAC9IVRw",
                "https://www.youtube.com/watch?v=9bZkp7q19f0",
                "https://www.youtube.com/watch?v=kJQP7kiw5Fk",
                "https://www.youtube.com/watch?v=OPf0YbXqDm0",
                "https://www.youtube.com/watch?v=fJ9rUzIMcZQ",
                "https://www.youtube.com/watch?v=60ItHLz5WEA",
                "https://www.youtube.com/watch?v=YQHsXMglC9A",
                "https://www.youtube.com/watch?v=2Vv-BfVoq4g",
                "https://www.youtube.com/watch?v=Zi_XLOBDo_Y",
            ];
            for (i, u) in urls.iter().take(n_tabs).enumerate() {
                if i > 0 {
                    self.tabs.push(Tab {
                        title: format!("page{}", i + 1),
                        url: u.to_string(),
                    });
                }
                self.active_tab = i;
                self.navigate(u, ctx, frame);
            }
            // 空白新标签页停靠（磁贴/搜索可见；不建 engine，不占内存）
            self.tabs.push(Tab {
                title: "新标签页".into(),
                url: String::new(),
            });
            self.active_tab = 0; // 压测兼容性验证：停在第一个 YouTube watch 页
            self.address_input = self.tabs[0].url.clone();
            self.hibernate_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
            if capsule_test {
                self.capsule_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(12));
                self.status = "压测：加载中，12秒后自动Alt+G胶囊".into();
            } else {
                self.status = "压测：YouTube 标签加载中（10秒后后台休眠）".into();
            }
        }

        // ================= tab strip =======================================
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let mut close_idx: Option<usize> = None;
                let mut clicked_idx: Option<usize> = None;
                let mut drag_swap: Option<(usize, usize)> = None;
                for (i, tab) in self.tabs.iter().enumerate() {
                    let is_active = i == self.active_tab;
                    // 直绘 tab：allocate_exact_size(Sense::click_and_drag) = 点击/中键/拖拽一个响应全有
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(180.0, 30.0),
                        egui::Sense::click_and_drag(),
                    );
                    let p = ui.painter_at(rect);
                    let bg = if is_active {
                        t.tab_active
                    } else if resp.hovered() {
                        t.surface
                    } else {
                        t.bg
                    };
                    p.rect_filled(rect, egui::CornerRadius::same(9), bg);
                    // favicon 图标（版本号变才重建纹理，不泄漏）
                    let mut icon_drawn = false;
                    if let Some(eng) = &self.engine {
                        if let Some((fw, fh, fdata, ver)) = eng.favicon(i) {
                            while self.fav_cache.len() <= i { self.fav_cache.push((0, None)); }
                            if self.fav_cache[i].0 != ver && fw > 0 && fh > 0 && fw <= 256 && fh <= 256 {
                                if let Some(old_tex) = self.fav_cache[i].1.take() {
                                    ui.ctx().tex_manager().write().free(old_tex);
                                }
                                let tex = ui.ctx().load_texture(
                                    format!("rxb_fav_{i}_{ver}"),
                                    egui::ColorImage::from_rgba_unmultiplied(
                                        [fw as usize, fh as usize], &fdata),
                                    egui::TextureOptions::LINEAR,
                                );
                                self.fav_cache[i] = (ver, Some(tex.id()));
                            }
                            if let Some(tid) = self.fav_cache[i].1 {
                                let icon_rect = egui::Rect::from_min_size(
                                    egui::pos2(rect.min.x + 6.0, rect.center().y - 8.0),
                                    egui::vec2(16.0, 16.0),
                                );
                                p.image(tid, icon_rect, egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE);
                                icon_drawn = true;
                            }
                        }
                    }
                    if !icon_drawn {
                        let dot_x = rect.min.x + 14.0;
                        let dot = if tab.url.is_empty() { t.text_dim } else { t.accent };
                        p.circle_filled(egui::pos2(dot_x, rect.center().y), 4.0, dot);
                    }
                    // 标题（字符截断防溢出）
                    let max_chars = 14usize;
                    let title: String = tab.title.chars().take(max_chars).collect();
                    let title = if tab.title.chars().count() > max_chars { format!("{title}…") } else { title };
                    p.text(
                        egui::pos2(rect.min.x + 26.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        title,
                        egui::FontId::proportional(13.0),
                        t.text,
                    );
                    // ✕ 关闭区（右端 24px）
                    let close_rect = egui::Rect::from_min_max(
                        egui::pos2(rect.max.x - 24.0, rect.min.y + 4.0),
                        egui::pos2(rect.max.x - 6.0, rect.max.y - 4.0),
                    );
                    let close_hovered = resp
                        .interact_pointer_pos()
                        .map_or(false, |pp| close_rect.contains(pp));
                    p.text(
                        close_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if close_hovered { "✕" } else { "" },
                        egui::FontId::proportional(11.0),
                        t.text_dim,
                    );
                    // 命中判定
                    if resp.clicked() {
                        if close_hovered && self.tabs.len() > 1 {
                            close_idx = Some(i);
                        } else {
                            eprintln!("rxb: tab {i} CLICKED");
                            clicked_idx = Some(i);
                        }
                    }
                    if resp.middle_clicked() && self.tabs.len() > 1 {
                        close_idx = Some(i);
                    }
                    // 拖拽重排：拖过半 tab 宽就交换相邻
                    if resp.dragged() {
                        let dx = resp.drag_delta().x;
                        if dx.abs() > 60.0 {
                            let dir = if dx > 0.0 { 1isize } else { -1isize };
                            let j = i as isize + dir;
                            if j >= 0 && (j as usize) < self.tabs.len() {
                                drag_swap = Some((i, j as usize));
                            }
                        }
                    }
                }
                if let Some((a, b)) = drag_swap {
                    self.tabs.swap(a, b);
                    if self.active_tab == a {
                        self.active_tab = b;
                    } else if self.active_tab == b {
                        self.active_tab = a;
                    }
                }
                if let Some(i) = clicked_idx {
                        eprintln!("rxb: switching to tab {}", clicked_idx.unwrap());
                    self.switch_tab(i);
                }
                if let Some(i) = close_idx {
                    self.tabs.remove(i);
                    if let Some(w) = &mut self.engine {
                        w.close_view(i);
                    }
                    if self.active_tab >= self.tabs.len() {
                        self.active_tab = self.tabs.len() - 1;
                    }
                }
                if ui.button(" ＋ ").on_hover_text("新标签页").clicked() {
                    self.tabs.push(Tab {
                        title: "新标签页".into(),
                        url: String::new(),
                    });
                    self.active_tab = self.tabs.len() - 1;
                    self.address_input.clear();
                    self.query.clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("◐ {}", t.name)).clicked() {
                        self.theme = if self.theme.name == "light" {
                            Theme::dark()
                        } else {
                            Theme::light()
                        };
                    }
                });
            });
            ui.add_space(4.0);
        });

        // ================= nav bar =========================================
        egui::TopBottomPanel::top("nav").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                if ui.button("←").on_hover_text("后退").clicked() {}
                if ui.button("→").on_hover_text("前进").clicked() {}
                if ui.button("⟳").on_hover_text("刷新").clicked() {}
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.address_input)
                        .hint_text("搜索或输入网址")
                        .desired_width(f32::INFINITY)
                        .frame(true),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = Some(self.address_input.clone());
                }
                ui.add_space(6.0);
            });
            ui.add_space(5.0);
        });

        // ================= left rail =======================================
        egui::SidePanel::left("rail")
            .resizable(false)
            .default_width(46.0)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.vertical_centered(|ui| {
                    if ui.button("⌂").on_hover_text("主页").clicked() {}
                    if ui.button("★").on_hover_text("书签").clicked() {}
                    if ui.button("⬇").on_hover_text("下载").clicked() {}
                    if ui.button("⚙").on_hover_text("设置").clicked() {}
                });
            });

        // ================= main area =======================================
        let is_newtab = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.url.is_empty())
            .unwrap_or(true);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(t.bg))
            .show(ctx, |ui| {
                if is_newtab {
                    // ---- wallpaper background ----------------------------
                    if let Some(wall) = &self.wall {
                        let rect = ui.max_rect();
                        ui.painter().image(
                            wall.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }

                    // ---- centered search ---------------------------------
                    ui.add_space(70.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("RXbrowser")
                                .size(34.0)
                                .strong()
                                .color(t.text),
                        );
                        ui.add_space(18.0);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text("  搜索 Google 或输入网址")
                                .desired_width(560.0)
                                .frame(true),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            go = Some(self.query.clone());
                        }
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(&self.status).color(t.text_dim).small());
                    });

                    // ---- shortcut tiles (native labels = guaranteed text) --
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        for row_tiles in TILES.chunks(6) {
                            ui.horizontal(|ui| {
                                ui.add_space(4.0);
                                for tile in row_tiles {
                                    ui.vertical_centered(|ui| {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            egui::vec2(58.0, 58.0),
                                            egui::Sense::click(),
                                        );
                                        let p = ui.painter_at(rect);
                                        let fill = if resp.hovered() {
                                            t.surface
                                        } else {
                                            t.surface.gamma_multiply(0.88)
                                        };
                                        p.rect_filled(
                                            rect,
                                            egui::CornerRadius::same(14),
                                            fill,
                                        );
                                        p.text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            tile.label.chars().next().unwrap_or('?'),
                                            egui::FontId::proportional(24.0),
                                            egui::Color32::from_rgb(
                                                tile.color[0],
                                                tile.color[1],
                                                tile.color[2],
                                            ),
                                        );
                                        ui.label(
                                            egui::RichText::new(tile.label)
                                                .size(12.0)
                                                .color(t.text),
                                        );
                                        if resp.clicked() {
                                            go = Some(tile.url.to_string());
                                        }
                                    });
                                    ui.add_space(16.0);
                                }
                            });
                            ui.add_space(12.0);
                        }
                    });

                    // ---- GPU preference (老师定：默认最高性能) ---------------
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("渲染GPU：")
                                    .color(t.text_dim)
                                    .small(),
                            );
                            for (pref, label) in [
                                (GpuPref::Performance, "性能优先"),
                                (GpuPref::PowerSaver, "省电优先"),
                                (GpuPref::Auto, "自动"),
                            ] {
                                if ui
                                    .selectable_label(self.gpu_pref == pref, label)
                                    .clicked()
                                {
                                    self.gpu_pref = pref;
                                }
                            }
                        });
                    });
                } else {
                    // ---- live web view (M1-5: servo offscreen -> texture) --
                    let avail = ui.available_rect_before_wrap();
                    let (rect, _) =
                        ui.allocate_exact_size(avail.size(), egui::Sense::click_and_drag());
                    let act = self.active_tab;
                    if let Some(w) = self.engine.as_mut() {
                        w.resize(rect.width().max(1.0) as u32, rect.height().max(1.0) as u32);
                        if let Some(tex) = w.sync_frame(&ctx, act) {
                            ui.painter().image(
                                tex.id(),
                                rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        }
                    }
                    if let Some(w) = &self.engine {
                        // move 在拖拽中可出界（拖拽滚动不断链）；button 事件只在 rect 内转发（防负坐标污染）
                        let dragging = ctx.input(|i| i.pointer.primary_down());
                        let hover = ctx
                            .input(|i| i.pointer.hover_pos())
                            .filter(|p| rect.contains(*p) || dragging);
                        if let Some(p) = hover {
                            let lx = p.x - rect.min.x;
                            let ly = p.y - rect.min.y;
                            w.mouse_move(act, lx, ly);
                            let scroll = ctx.input(|i| i.raw_scroll_delta);
                            if scroll != egui::Vec2::ZERO {
                                w.wheel(act, scroll.x, -scroll.y, lx, ly);
                            }
                            // button 只在指针位于 rect 内时转发（拖拽中出界不发 down/up）
                            let in_rect = rect.contains(p);
                            let pp = ctx.input(|i| i.pointer.primary_pressed());
                            let pr = ctx.input(|i| i.pointer.primary_released());
                            if (pp || pr) && in_rect {
                                w.mouse_button(act, pp, lx, ly);
                            }
                        }
                        // 键盘转发：web 区活跃且 egui 无文本框焦点时
                        if !ctx.wants_keyboard_input() {
                            let _kb_dbg = true;
                            let key_events: Vec<(egui::Key, bool, egui::Modifiers)> = ctx.input(|i| {
                                i.events.iter().filter_map(|e| match e {
                                    egui::Event::Key { key, pressed, modifiers, .. } =>
                                        Some((*key, *pressed, *modifiers)),
                                    _ => None,
                                }).collect()
                            });
                            if !key_events.is_empty() || ctx.input(|i| !i.events.iter().all(|e| !matches!(e, egui::Event::Text(_)))) {
                                eprintln!("rxb: KEY-EVT n={} texts={}", key_events.len(), ctx.input(|i| i.events.iter().filter(|e| matches!(e, egui::Event::Text(_))).count()));
                            }
                            for (key, pressed, mods) in key_events {
                                if let Some((kt_key, kt_code)) = map_egui_key(key) {
                                    let state = if pressed {
                                        servo::KeyState::Down
                                    } else {
                                        servo::KeyState::Up
                                    };
                                    w.key_event(act, state, kt_key, kt_code, map_egui_mods(mods));
                                }
                            }
                            let texts: Vec<String> = ctx.input(|i| {
                                i.events.iter().filter_map(|e| match e {
                                    egui::Event::Text(t) => Some(t.clone()),
                                    _ => None,
                                }).collect()
                            });
                            for t in texts {
                                if !t.is_empty() {
                                    w.ime_text(act, &t);
                                }
                            }
                        }
                        if let Some(title) = w.title(act) {
                            if !title.is_empty() {
                                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                    if tab.title != title {
                                        tab.title = title;
                                    }
                                }
                            }
                        }
                        if let Some(live_url) = w.page_url(act) {
                            if !live_url.is_empty() {
                                self.address_input = live_url;
                            }
                        }
                    } else {
                        ui.add_space(80.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("引擎未启动")
                                    .size(22.0)
                                    .color(t.text_dim),
                            );
                            ui.add_space(8.0);
                            if ui.button("← 返回新标签页").clicked() {
                                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                    tab.url.clear();
                                    tab.title = "新标签页".into();
                                }
                                self.address_input.clear();
                            }
                        });
                    }
                }
            });

        // ================= deferred navigation ============================
        if let Some(raw) = go {
            self.navigate(&raw, ctx, frame);
        }

        // ================= global visuals ==================================
        let mut style = (*ctx.style()).clone();
        let mut visuals = style.visuals.clone();
        visuals.window_fill = t.surface;
        visuals.panel_fill = t.bg;
        visuals.extreme_bg_color = t.bg;
        visuals.faint_bg_color = t.surface;
        visuals.override_text_color = Some(t.text);
        visuals.widgets.noninteractive.bg_stroke.color = t.border;
        visuals.widgets.inactive.bg_stroke.color = t.border;
        visuals.widgets.inactive.weak_bg_fill = t.surface;
        visuals.widgets.hovered.weak_bg_fill = t.tab_active;
        visuals.selection.bg_fill = t.accent.linear_multiply(0.30);
        style.visuals = visuals;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        ctx.set_style(style);
    }
}

// Ask Windows GPU drivers to prefer the discrete card (RTX 4050 over Intel
// iGPU). Harmless on iGPU-only / ARM machines — the symbols are ignored.
surfman::declare_surfman!();


fn map_egui_key(key: egui::Key) -> Option<(servo::Key, servo::Code)> {
    use servo::Code;
    use servo::Key;
    use servo::NamedKey;
    Some(match key {
        egui::Key::Enter => (Key::Named(NamedKey::Enter), Code::Enter),
        egui::Key::Backspace => (Key::Named(NamedKey::Backspace), Code::Backspace),
        egui::Key::Delete => (Key::Named(NamedKey::Delete), Code::Delete),
        egui::Key::Escape => (Key::Named(NamedKey::Escape), Code::Escape),
        egui::Key::Tab => (Key::Named(NamedKey::Tab), Code::Tab),
        egui::Key::ArrowUp => (Key::Named(NamedKey::ArrowUp), Code::ArrowUp),
        egui::Key::ArrowDown => (Key::Named(NamedKey::ArrowDown), Code::ArrowDown),
        egui::Key::ArrowLeft => (Key::Named(NamedKey::ArrowLeft), Code::ArrowLeft),
        egui::Key::ArrowRight => (Key::Named(NamedKey::ArrowRight), Code::ArrowRight),
        egui::Key::Home => (Key::Named(NamedKey::Home), Code::Home),
        egui::Key::End => (Key::Named(NamedKey::End), Code::End),
        egui::Key::PageUp => (Key::Named(NamedKey::PageUp), Code::PageUp),
        egui::Key::PageDown => (Key::Named(NamedKey::PageDown), Code::PageDown),
        egui::Key::Space => (Key::Character(" ".into()), Code::Space),
        k => {
            let name = format!("{k:?}");
            if name.chars().count() == 1 {
                (Key::Character(name.to_lowercase()), Code::Unidentified)
            } else {
                return None;
            }
        }
    })
}

fn map_egui_mods(m: egui::Modifiers) -> servo::Modifiers {
    use servo::Modifiers as KM;
    let mut out = KM::empty();
    if m.shift { out |= KM::SHIFT; }
    if m.ctrl { out |= KM::CONTROL; }
    if m.alt { out |= KM::ALT; }
    if m.command { out |= KM::META; }
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    // Keep the proven M1-3 servo path linked & compiled (M1-5 activates it).
    let _ = embed::probe();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([760.0, 520.0])
            .with_title("RXbrowser"),
        ..Default::default()
    };
    eframe::run_native(
        "RXbrowser",
        options,
        Box::new(|cc| {
            install_cjk_fonts(&cc.egui_ctx);
            Ok(Box::new(ShellApp::new(cc)))
        }),
    )?;
    Ok(())
}
