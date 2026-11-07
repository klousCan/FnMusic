use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use fnmusic::api::{AuthFuture, Authorization, Credentials, Error, Result, SessionHooks};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    platform::{
        run_return::EventLoopExtRunReturn,
        windows::{EventLoopBuilderExtWindows, WindowBuilderExtWindows},
    },
    window::{Window, WindowBuilder, WindowId},
};
use tokio::sync::oneshot;
use url::Url;
use webview2_com::{
    ClearBrowsingDataCompletedHandler,
    Microsoft::Web::WebView2::Win32::{ICoreWebView2_13, ICoreWebView2Profile2},
};
use windows::core::Interface;
use wry::{WebContext, WebView, WebViewBuilder, WebViewExtWindows};

use super::vault::Vault;

enum Command {
    Open {
        request: Authorization,
        profile: String,
        silent: bool,
        reply: oneshot::Sender<Result<String>>,
    },
    Complete(WindowId, Result<String>),
    Clear(String, oneshot::Sender<Result<()>>),
    Stop,
}

#[derive(Clone)]
pub struct Broker {
    proxy: EventLoopProxy<Command>,
}

impl Broker {
    pub fn start(profiles: PathBuf) -> Result<Self> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        thread::Builder::new()
            .name("fnos-authorization".into())
            .spawn(move || {
                let mut event_loop = EventLoopBuilder::<Command>::with_user_event()
                    .with_any_thread(true)
                    .build();
                let proxy = event_loop.create_proxy();
                if send.send(proxy.clone()).is_ok() {
                    let mut app = OAuthApp {
                        profiles,
                        proxy,
                        active: None,
                    };
                    event_loop.run_return(move |event, window_target, flow| {
                        match event {
                            Event::UserEvent(Command::Stop) => {
                                *flow = ControlFlow::Exit;
                                return;
                            }
                            Event::UserEvent(command) => app.user_event(window_target, command),
                            Event::WindowEvent {
                                window_id, event, ..
                            } => app.window_event(window_id, event),
                            Event::MainEventsCleared => app.check_deadline(),
                            _ => {}
                        }
                        *flow = app.active.as_ref().map_or(ControlFlow::Wait, |active| {
                            ControlFlow::WaitUntil(active.deadline)
                        });
                    });
                }
            })
            .map_err(|_| unavailable())?;
        let proxy = receive
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| unavailable())?;
        Ok(Self { proxy })
    }

    pub async fn authorize(
        &self,
        request: Authorization,
        profile: String,
        silent: bool,
    ) -> Result<String> {
        let (reply, receive) = oneshot::channel();
        self.proxy
            .send_event(Command::Open {
                request,
                profile,
                silent,
                reply,
            })
            .map_err(|_| unavailable())?;
        tokio::time::timeout(Duration::from_secs(if silent { 25 } else { 195 }), receive)
            .await
            .map_err(|_| Error::Invalid("NAS 授权超时，请重试".into()))?
            .map_err(|_| unavailable())?
    }

    pub async fn clear(&self, profile: String) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.proxy
            .send_event(Command::Clear(profile, reply))
            .map_err(|_| unavailable())?;
        tokio::time::timeout(Duration::from_secs(15), receive)
            .await
            .map_err(|_| Error::Storage)?
            .map_err(|_| unavailable())?
    }

    pub fn stop(&self) {
        let _ = self.proxy.send_event(Command::Stop);
    }
}

pub struct Hooks {
    pub browser: Broker,
    pub vault: Vault,
}

impl SessionHooks for Hooks {
    fn authorize(&self, request: Authorization, profile: String, silent: bool) -> AuthFuture {
        let browser = self.browser.clone();
        Box::pin(async move { browser.authorize(request, profile, silent).await })
    }

    fn save(&self, credentials: &Credentials) -> Result<()> {
        self.vault.save(credentials)
    }
}

fn unavailable() -> Error {
    Error::Invalid("无法启动 NAS 授权窗口，请检查 Microsoft Edge WebView2 Runtime".into())
}

struct LoginWindow {
    // Drop the webview before its host window.
    _webview: WebView,
    window: Window,
    _context: WebContext,
    profile: String,
    deadline: Instant,
    reply: oneshot::Sender<Result<String>>,
}

struct OAuthApp {
    profiles: PathBuf,
    proxy: EventLoopProxy<Command>,
    active: Option<LoginWindow>,
}

impl OAuthApp {
    fn create_window(event_loop: &EventLoopWindowTarget<Command>) -> Result<Window> {
        WindowBuilder::new()
            // Tao and winit otherwise register the same Win32 class with different callbacks.
            .with_window_classname("FnMusic.OAuthWindow")
            .with_title("飞牛音乐 · 登录 fnOS")
            .with_inner_size(LogicalSize::new(500.0, 650.0))
            .with_min_inner_size(LogicalSize::new(360.0, 420.0))
            .with_visible(false)
            .build(event_loop)
            .map_err(|_| unavailable())
    }

    fn open(
        &mut self,
        event_loop: &EventLoopWindowTarget<Command>,
        request: Authorization,
        profile: String,
        silent: bool,
        reply: oneshot::Sender<Result<String>>,
    ) {
        if self.active.is_some() {
            let _ = reply.send(Err(Error::Invalid("已有一个 NAS 授权窗口".into())));
            return;
        }
        let result = (|| {
            let window = Self::create_window(event_loop)?;
            window.set_title(&format!(
                "飞牛音乐 · 登录 fnOS — {}",
                request.sign_in.origin().ascii_serialization()
            ));
            let id = window.id();
            let proxy = self.proxy.clone();
            let sign_in = request.sign_in.to_string();
            let mut context = WebContext::new(Some(self.profiles.join(&profile)));
            let webview = WebViewBuilder::with_web_context(&mut context)
                .with_url(sign_in)
                .with_navigation_handler(move |address| {
                    let Ok(target) = Url::parse(&address) else {
                        return false;
                    };
                    if let Some(result) = request.callback_code(&target) {
                        let _ = proxy.send_event(Command::Complete(id, result));
                        return false;
                    }
                    request.permits(&target)
                })
                .with_new_window_req_handler(|_| false)
                .with_devtools(false)
                .build(&window)
                .map_err(|_| unavailable())?;
            if !silent {
                window.set_visible(true);
                window.set_focus();
            }
            Ok((window, webview, context))
        })();
        match result {
            Ok((window, webview, context)) => {
                let deadline = Instant::now() + Duration::from_secs(if silent { 12 } else { 180 });
                self.active = Some(LoginWindow {
                    _webview: webview,
                    window,
                    _context: context,
                    profile,
                    deadline,
                    reply,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn finish(&mut self, result: Result<String>) {
        if let Some(active) = self.active.take() {
            let LoginWindow {
                reply,
                _webview,
                window,
                _context,
                ..
            } = active;
            drop(_webview);
            drop(window);
            drop(_context);
            let _ = reply.send(result);
        }
    }

    fn clear(&self, event_loop: &EventLoopWindowTarget<Command>, profile: &str) -> Result<()> {
        let path = self.profiles.join(profile);
        if !path.exists() {
            return Ok(());
        }
        let window = Self::create_window(event_loop)?;
        let mut context = WebContext::new(Some(path));
        let webview = WebViewBuilder::with_web_context(&mut context)
            .build(&window)
            .map_err(|_| unavailable())?;
        // Keep the controller alive until WebView2 confirms that it cleared its profile.
        let profile = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|view| view.cast::<ICoreWebView2_13>())
                .and_then(|view| view.Profile())
                .and_then(|profile| profile.cast::<ICoreWebView2Profile2>())
        }
        .map_err(|_| Error::Storage)?;
        ClearBrowsingDataCompletedHandler::wait_for_async_operation(
            Box::new(move |callback| {
                unsafe {
                    profile.ClearBrowsingDataAll(&callback)?;
                }
                Ok(())
            }),
            Box::new(|status| status),
        )
        .map_err(|_| Error::Storage)
    }
}

impl OAuthApp {
    fn user_event(&mut self, event_loop: &EventLoopWindowTarget<Command>, command: Command) {
        match command {
            Command::Open {
                request,
                profile,
                silent,
                reply,
            } => {
                self.open(event_loop, request, profile, silent, reply);
            }
            Command::Complete(id, result) => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.window.id() == id)
                {
                    self.finish(result);
                }
            }
            Command::Clear(profile, reply) => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.profile == profile)
                {
                    self.finish(Err(Error::Cancelled));
                }
                let _ = reply.send(self.clear(event_loop, &profile));
            }
            Command::Stop => {}
        }
    }

    fn window_event(&mut self, id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested)
            && self
                .active
                .as_ref()
                .is_some_and(|active| active.window.id() == id)
        {
            self.finish(Err(Error::Cancelled));
        }
    }

    fn check_deadline(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| Instant::now() >= active.deadline || active.reply.is_closed())
        {
            self.finish(Err(Error::Invalid("NAS 授权超时，请重试".into())));
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        extract::Query,
        http::{HeaderMap, header},
        response::{Html, IntoResponse},
        routing::get,
    };
    use tokio::{net::TcpListener, runtime::Runtime};

    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows session and WebView2 Runtime"]
    fn webview_captures_callback_and_clears_profile() {
        use winit::platform::windows::EventLoopBuilderExtWindows as _;
        let main_loop = winit::event_loop::EventLoop::builder()
            .with_any_thread(true)
            .build()
            .unwrap();
        // Register the same window class the production egui main window uses.
        #[allow(deprecated)]
        let _main_window = main_loop
            .create_window(winit::window::Window::default_attributes().with_visible(false))
            .unwrap();
        let runtime = Runtime::new().unwrap();
        let data = tempfile::tempdir().unwrap();
        let browser = Broker::start(data.path().to_owned()).unwrap();
        runtime.block_on(async {
            let app = Router::new().route("/signin", get(|headers: HeaderMap, Query(query): Query<std::collections::HashMap<String, String>>| async move {
                let had_cookie = headers.get(header::COOKIE).is_some_and(|cookie| cookie == "test_session=1");
                let code = if had_cookie { "existing" } else { "new" };
                let mut callback = Url::parse(&query["redirect_uri"]).unwrap();
                callback.query_pairs_mut().append_pair("code", code);
                let target = serde_json::to_string(callback.as_str()).unwrap();
                let body = Html(format!("<script>location.replace({target})</script>"));
                ([(header::SET_COOKIE, "test_session=1; Path=/; Max-Age=3600")], body).into_response()
            }));
            let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", server.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(server, app).await.unwrap() });
            let callback = Url::parse(&format!("{origin}/music/oauth/result")).unwrap();
            let mut sign_in = Url::parse(&format!("{origin}/signin")).unwrap();
            sign_in.query_pairs_mut().append_pair("redirect_uri", callback.as_str());
            let request = Authorization { sign_in, callback };
            let profile = "mock-browser".to_owned();
            for expected in ["new", "existing", "new"] {
                let result = browser.authorize(request.clone(), profile.clone(), true).await.unwrap();
                assert_eq!(result, expected);
                if expected == "existing" {
                    browser.clear(profile.clone()).await.unwrap();
                }
            }
        });
        browser.stop();
    }
}
