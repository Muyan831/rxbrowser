//! M1-3: minimal libservo embedding (kept for M1-5 same-window compositing).
//! Proven working: window + example.com rendered (2026-11-24).

//! RXbrowser M1-3: minimal libservo embedding — proof of route B.
//!
//! Structure referenced from servo's official `winit_minimal` example (MPL-2.0);
//! this file is original code that only *calls* public servo APIs (never copies
//! servo sources — see MPL red lines in design doc §4).

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use servo::{
    InputEvent, RenderingContext, Servo, ServoBuilder, WebView, WebViewBuilder, WheelDelta,
    WheelEvent, WheelMode, WindowRenderingContext,
};
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoop;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowAttributes};

/// Owns everything the window and servo need to share.
struct AppContext {
    window: Window,
    servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    webviews: RefCell<Vec<WebView>>,
}

impl servo::WebViewDelegate for AppContext {
    fn notify_new_frame_ready(&self, _view: WebView) {
        self.window.request_redraw();
    }
}

/// Bridges servo's cross-thread wake-ups into the winit event loop.
#[derive(Clone)]
struct LoopWaker(winit::event_loop::EventLoopProxy<WakerEvent>);

#[derive(Debug)]
struct WakerEvent;

impl servo::EventLoopWaker for LoopWaker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(Self(self.0.clone()))
    }
    fn wake(&self) {
        let _ = self.0.send_event(WakerEvent);
    }
}

enum Phase {
    /// Before the window exists (winit calls `resumed` once ready).
    Starting(LoopWaker),
    Live(Rc<AppContext>),
}

impl Phase {
    fn new(event_loop: &EventLoop<WakerEvent>) -> Self {
        Self::Starting(LoopWaker(event_loop.create_proxy()))
    }
}

impl ApplicationHandler<WakerEvent> for Phase {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Self::Starting(waker) = self else { return };

        let display_handle = match event_loop.display_handle() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("rxb: no display handle: {e}");
                event_loop.exit();
                return;
            }
        };
        let window = match event_loop.create_window(
            WindowAttributes::default().with_title("RXbrowser"),
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("rxb: window creation failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let window_handle = match window.window_handle() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("rxb: no window handle: {e}");
                event_loop.exit();
                return;
            }
        };

        let rendering_context = match WindowRenderingContext::new(
            display_handle,
            window_handle,
            window.inner_size(),
        ) {
            Ok(rc) => Rc::new(rc),
            Err(e) => {
                eprintln!("rxb: rendering context failed: {e:?}");
                event_loop.exit();
                return;
            }
        };
        let _ = rendering_context.make_current();

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(waker.clone()))
            .build();

        let ctx = Rc::new(AppContext {
            window,
            servo,
            rendering_context,
            webviews: RefCell::new(Vec::new()),
        });

        let url = servo::ServoUrl::parse("https://example.com").expect("static url is valid");
        let view = WebViewBuilder::new(&ctx.servo, ctx.rendering_context.clone())
            .url(url.into_url())
            .hidpi_scale_factor(euclid::Scale::new(ctx.window.scale_factor() as f32))
            .delegate(ctx.clone())
            .build();
        ctx.webviews.borrow_mut().push(view);

        eprintln!("rxb: M1-3 servo embedded, view created");
        *self = Self::Live(ctx);
    }

    fn user_event(&mut self, _el: &winit::event_loop::ActiveEventLoop, _e: WakerEvent) {
        if let Self::Live(ctx) = self {
            ctx.servo.spin_event_loop();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Self::Live(ctx) = self else { return };
        ctx.servo.spin_event_loop();

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Some(view) = ctx.webviews.borrow().last() {
                    view.paint();
                    ctx.rendering_context.present();
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        ((x * 76.0) as f64, (y * 76.0) as f64, WheelMode::DeltaLine)
                    },
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y, WheelMode::DeltaPixel),
                };
                if let Some(view) = ctx.webviews.borrow().last() {
                    view.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta { x: dx, y: dy, z: 0.0, mode },
                        servo::DevicePoint::default().into(),
                    )));
                }
            },
            WindowEvent::Resized(size) => {
                if let Some(view) = ctx.webviews.borrow().last() {
                    view.resize(size);
                }
            },
            _ => {},
        }
    }
}

#[allow(dead_code)]
pub fn m13_entry() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<WakerEvent>::with_user_event().build()?;
    let mut app = Phase::new(&event_loop);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// M1-4 keeps this module compiled; M1-5 replaces `probe()` with the real
/// `Phase` bootstrap that shares the window with the egui shell.
pub fn probe() -> usize {
    std::mem::size_of::<LoopWaker>()
}
