//! GPUI side: the wallpaper window (attached under the desktop icons) and the settings window.
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::actors::{self, World};
use crate::assets::Game;
use crate::desktop::{self, Attachment, IconReader};
use crate::iconfx::IconFx;
use crate::scene::{self, Rect, Scene, Theme, ThemeKit};
use crate::settings::{self, Settings};
use crate::tray::{self, Cmd, Tray};

pub struct AppState {
    pub settings: Settings,
    pub game: Option<Arc<Game>>,
    pub wallpaper: Option<Entity<Wallpaper>>,
    pub settings_win: Option<WindowHandle<SettingsView>>,
    pub tray: Option<Tray>,
}

impl Global for AppState {}

fn bgra_image(w: usize, h: usize, px: &[[u8; 4]]) -> Arc<RenderImage> {
    let mut v = Vec::with_capacity(w * h * 4);
    for p in px {
        v.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, v).expect("image size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buf)]))
}

struct Water {
    w: f32,
    h: f32,
    a: Arc<RenderImage>,
    b: Arc<RenderImage>,
}

struct Built {
    scene: Scene,
    ground: Arc<RenderImage>,
    water: Option<Water>,
    took: Duration,
}

pub struct Wallpaper {
    hwnd: Option<windows::Win32::Foundation::HWND>,
    att: Option<Attachment>,
    reader: IconReader,
    phys: (usize, usize),
    origin: (i32, i32),
    scale: f32,
    theme: Option<Theme>,
    kit: Option<Arc<ThemeKit>>,
    actor_kit: Option<actors::Kit>,
    icons: Vec<Rect>,
    icon_key: Vec<i32>,
    scene: Option<Scene>,
    ground: Option<Arc<RenderImage>>,
    fade: Option<(Arc<RenderImage>, Instant)>,
    water: Option<Water>,
    world: Option<World>,
    building: bool,
    rebuild: bool,
    hidden: bool,
    last_poll: Instant,
    last_step: Instant,
    /// Water scroll offsets (logical px), each wrapped to its tile so they never lose precision.
    water_phase: [f32; 4],
    /// Icons changed: rebuild once they have stayed put for a moment (and the desktop is visible).
    build_due: Option<Instant>,
    last_health: Instant,
    to_drop: Vec<Arc<RenderImage>>,
    /// Icon Wars: the damage drawn over the real icons, and the icon pictures' rectangles.
    fx: Option<IconFx>,
    pics: Vec<(i32, i32, i32, i32)>,
    pub status: SharedString,
    _ticker: Option<Task<()>>,
}

const FADE: f32 = 0.45;

impl Wallpaper {
    fn new(cx: &mut Context<Self>) -> Self {
        let r = desktop::primary_rect();
        let ticker = cx.spawn(async move |this, cx| loop {
            let Ok(next) = this.update(cx, |this, cx| this.tick(cx)) else { break };
            cx.background_executor().timer(next).await;
        });
        let mut w = Wallpaper {
            hwnd: None,
            att: None,
            reader: IconReader::new(),
            phys: ((r.right - r.left) as usize, (r.bottom - r.top) as usize),
            origin: (r.left, r.top),
            scale: 1.0,
            theme: None,
            kit: None,
            actor_kit: None,
            icons: Vec::new(),
            icon_key: vec![-1],
            scene: None,
            ground: None,
            fade: None,
            water: None,
            world: None,
            building: false,
            rebuild: false,
            hidden: false,
            last_poll: Instant::now() - Duration::from_secs(5),
            last_step: Instant::now(),
            water_phase: [0.0, 0.0, 37.0, 11.0],
            build_due: None,
            last_health: Instant::now(),
            to_drop: Vec::new(),
            fx: None,
            pics: Vec::new(),
            status: "Starting".into(),
            _ticker: Some(ticker),
        };
        w.read_icons();
        w
    }

    fn settings<'a>(&self, cx: &'a App) -> &'a Settings {
        &cx.global::<AppState>().settings
    }

    /// Sprite scale for the current display scaling.
    fn ds(&self) -> f32 {
        self.scale.max(0.5)
    }

    fn read_icons(&mut self) -> bool {
        let Some(rects) = self.reader.read() else { return false };
        let (ox, oy) = self.origin;
        let (w, h) = (self.phys.0 as i32, self.phys.1 as i32);
        let mut key = Vec::with_capacity(rects.len() * 4);
        let mut icons = Vec::with_capacity(rects.len());
        for r in rects {
            let (x0, y0, x1, y1) = (r.left - ox, r.top - oy, r.right - ox, r.bottom - oy);
            if x1 <= 0 || y1 <= 0 || x0 >= w || y0 >= h || x1 <= x0 {
                continue;
            }
            key.extend_from_slice(&[x0, y0, x1, y1]);
            icons.push(Rect { x0: x0 as f32, y0: y0 as f32, x1: x1 as f32, y1: y1 as f32, pad: 0.0 });
        }
        if key == self.icon_key {
            return false;
        }
        self.icon_key = key;
        self.icons = icons;
        true
    }

    /// Reloads the theme from scratch, e.g. after the game folder changed.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.theme = None;
        self.settings_changed(cx);
    }

    pub fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let s = self.settings(cx).clone();
        if self.theme != Some(s.theme) {
            self.load_theme(s.theme, cx);
        } else {
            self.request_build(cx);
        }
        cx.notify();
    }

    fn load_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        let Some(game) = cx.global::<AppState>().game.clone() else {
            self.status = "Age of Empires II: Definitive Edition was not found".into();
            return;
        };
        self.theme = Some(theme);
        self.status = format!("Loading {}", theme.name()).into();
        let ds = self.ds();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move { (ThemeKit::load(&game, theme, ds), actors::Kit::load(&game, theme, ds)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.theme != Some(theme) {
                    return; // switched again meanwhile
                }
                match loaded {
                    (Ok(kit), Ok(ak)) => {
                        this.kit = Some(Arc::new(kit));
                        this.actor_kit = Some(ak);
                        if let Some(old) = this.world.take() {
                            this.to_drop.extend(old.images());
                        }
                        this.request_build(cx);
                    }
                    (Err(e), _) | (_, Err(e)) => this.status = format!("Could not load {}: {e:#}", theme.name()).into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn request_build(&mut self, cx: &mut Context<Self>) {
        let Some(kit) = self.kit.clone() else { return };
        if self.building {
            self.rebuild = true;
            return;
        }
        self.building = true;
        let (w, h) = self.phys;
        let icons = self.icons.clone();
        let seed = self.settings(cx).seed;
        cx.spawn(async move |this, cx| {
            let built = cx
                .background_spawn(async move {
                    let t0 = Instant::now();
                    let mut scene = scene::build(&kit, w, h, &icons, seed);
                    let ground = bgra_image(w, h, &std::mem::take(&mut scene.ground));
                    let water = scene.water.take().map(|(tw, th, a, b)| Water {
                        w: tw as f32,
                        h: th as f32,
                        a: bgra_image(tw, th, &a),
                        b: bgra_image(tw, th, &b),
                    });
                    Built { scene, ground, water, took: t0.elapsed() }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.apply(built, cx);
                this.building = false;
                if std::mem::take(&mut this.rebuild) {
                    this.request_build(cx);
                }
            });
        })
        .detach();
    }

    fn apply(&mut self, b: Built, cx: &mut Context<Self>) {
        if let Some(old) = self.ground.take() {
            if let Some((older, _)) = self.fade.replace((old, Instant::now())) {
                self.to_drop.push(older);
            }
        }
        self.ground = Some(b.ground);
        if let Some(old) = std::mem::replace(&mut self.water, b.water) {
            self.to_drop.extend([old.a, old.b]);
        }
        let seed = self.settings(cx).seed;
        match (&mut self.world, self.actor_kit.take()) {
            (_, Some(ak)) => self.world = Some(World::new(ak, &b.scene, seed)),
            (Some(world), None) => world.revalidate(&b.scene),
            _ => {}
        }
        self.status = format!(
            "{} · {} icons · built in {} ms",
            self.theme.map_or("", |t| t.name()),
            self.icons.len(),
            b.took.as_millis()
        )
        .into();
        crate::log(&self.status);
        self.scene = Some(b.scene);
        cx.notify();
        cx.refresh_windows();
    }

    /// Periodic work; returns the delay until the next call.
    fn tick(&mut self, cx: &mut Context<Self>) -> Duration {
        let now = Instant::now();
        if now.duration_since(self.last_poll) >= Duration::from_millis(400) {
            self.last_poll = now;
            desktop::ensure_z_order();
            // Debounce: a file being written to the desktop, or icons being dragged, changes the
            // layout many times in a row; build once it settles instead of on every change.
            if self.read_icons() {
                self.build_due = Some(now + Duration::from_millis(700));
            }
            if self.theme == Some(Theme::IconWars) {
                let (ox, oy) = self.origin;
                self.pics = self.reader.icon_rects().unwrap_or_default().into_iter().map(|r| (r.left - ox, r.top - oy, r.right - ox, r.bottom - oy)).collect();
            }
            if let Some(h) = self.hwnd {
                let hidden = desktop::wallpaper_hidden(h) && std::env::var_os("WOLOLAND_NOPAUSE").is_none();
                if hidden != self.hidden {
                    self.hidden = hidden;
                    crate::log(if hidden { "paused: wallpaper covered" } else { "resumed" });
                }
            }
        }
        if self.build_due.is_some_and(|t| now >= t) && !self.hidden {
            self.build_due = None;
            self.request_build(cx);
        }
        if now.duration_since(self.last_health) >= Duration::from_secs(600) {
            self.last_health = now;
            crate::log(&desktop::health());
        }
        let s = self.settings(cx);
        let (animate, fps, paused) = (s.animate, s.fps.clamp(5, 60), s.paused);
        let active = !paused && !self.hidden;
        let dt = now.duration_since(self.last_step).as_secs_f32().min(0.25);
        self.last_step = now;
        let mut redraw = false;
        if let Some((_, t)) = &self.fade {
            if t.elapsed().as_secs_f32() > FADE {
                let (old, _) = self.fade.take().unwrap();
                self.to_drop.push(old);
            }
            redraw = true;
        }
        // the icons get their own look back as soon as the battle is over
        if self.world.as_ref().is_none_or(|w| w.battle().is_none()) || self.theme != Some(Theme::IconWars) {
            self.fx = None;
        }
        let moving = animate && (self.water.is_some() || self.world.as_ref().is_some_and(|w| !w.is_empty()));
        if active && moving {
            if let Some(w) = &self.water {
                let (tw, th) = (w.w / self.scale, w.h / self.scale);
                let p = &mut self.water_phase;
                p[0] = (p[0] + dt * 7.0).rem_euclid(tw);
                p[1] = (p[1] + dt * 3.5).rem_euclid(th);
                p[2] = (p[2] - dt * 5.0).rem_euclid(tw);
                p[3] = (p[3] + dt * 2.0).rem_euclid(th);
            }
            if let (Some(world), Some(scene)) = (&mut self.world, &self.scene) {
                world.step(scene, dt);
                if world.battle().is_some() && self.theme == Some(Theme::IconWars) {
                    let (origin, ds) = (self.origin, self.scale.max(0.5));
                    self.fx.get_or_insert_with(|| IconFx::new(origin, ds)).update(world, scene, &self.pics, dt);
                }
            }
            redraw = true;
        }
        if redraw || !self.to_drop.is_empty() {
            cx.notify();
        }
        if active && moving {
            Duration::from_secs_f32(1.0 / fps as f32)
        } else if self.fade.is_some() {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(400)
        }
    }

    pub fn attach(&mut self, window: &mut Window) {
        if let Some(mut a) = self.att.take() {
            a.detach();
        }
        if let Some(h) = desktop::hwnd_of(window) {
            self.hwnd = Some(h);
            self.att = desktop::attach(h);
            if self.att.is_none() {
                // Never leave a full-screen popup over the user's windows; the watchdog retries.
                desktop::hide(h);
                self.status = "Could not attach to the desktop".into();
            }
        }
    }

    /// The window is already destroyed: stop ticking and release the event hook without touching it.
    pub fn forget(&mut self) {
        self._ticker = None;
        if let Some(mut a) = self.att.take() {
            a.release();
        }
    }

    pub fn detach(&mut self) {
        if let Some(mut a) = self.att.take() {
            a.detach();
        }
    }
}

impl Render for Wallpaper {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.hwnd.is_none() {
            self.attach(window);
        }
        let scale = window.scale_factor();
        if (scale - self.scale).abs() > 0.01 || self.theme.is_none() {
            self.scale = scale;
            let theme = self.settings(cx).theme;
            self.kit = None;
            self.load_theme(theme, cx);
        }
        for img in self.to_drop.drain(..) {
            let _ = window.drop_image(img);
        }
        let water = self.water.as_ref().map(|w| (w.w / scale, w.h / scale, w.a.clone(), w.b.clone()));
        let ph = self.water_phase;
        let sprites: Vec<(Bounds<Pixels>, Arc<RenderImage>)> = self
            .world
            .as_ref()
            .map(|w| {
                w.visible()
                    .into_iter()
                    .map(|(x, y, f)| {
                        let b = Bounds::new(point(px((x + f.ox) / scale), px((y + f.oy) / scale)), size(px(f.w / scale), px(f.h / scale)));
                        (b, f.img.clone())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let fade = self.fade.as_ref().map(|(img, t0)| (img.clone(), (t0.elapsed().as_secs_f32() / FADE).min(1.0)));

        div()
            .size_full()
            .relative()
            .bg(rgb(0x202818))
            .when_some(water, |d, (tw, th, a, b)| {
                d.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let layer = |window: &mut Window, img: &Arc<RenderImage>, ox: f32, oy: f32| {
                                let (ox, oy) = (ox.rem_euclid(tw), oy.rem_euclid(th));
                                let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                                let mut y = oy - th;
                                while y < bh {
                                    let mut x = ox - tw;
                                    while x < bw {
                                        let b = Bounds::new(point(px(x), px(y)), size(px(tw + 0.5), px(th + 0.5)));
                                        let _ = window.paint_image(b, Corners::default(), img.clone(), 0, false);
                                        x += tw;
                                    }
                                    y += th;
                                }
                            };
                            layer(window, &a, ph[0], ph[1]);
                            layer(window, &b, ph[2], ph[3]);
                        },
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .when_some(fade.as_ref().filter(|(_, a)| *a < 1.0), |d, (img, _)| {
                d.child(img_el(img.clone()))
            })
            .when_some(self.ground.clone(), |d, g| {
                let a = fade.as_ref().map_or(1.0, |(_, a)| *a);
                d.child(img_el(g).opacity(a))
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        for (b, img) in sprites {
                            let _ = window.paint_image(b, Corners::default(), img, 0, false);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

fn img_el(img: Arc<RenderImage>) -> Img {
    gpui::img(ImageSource::Render(img)).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Fill)
}

// ------------------------------------------------------------------------------ commands

pub fn apply_cmd(cmd: Cmd, cx: &mut App) {
    let update = |cx: &mut App, f: &dyn Fn(&mut Settings)| {
        let st = cx.global_mut::<AppState>();
        f(&mut st.settings);
        st.settings.save();
        tray::sync(st.settings.theme, st.settings.animate, st.settings.paused);
        if let Some(wp) = st.wallpaper.clone() {
            wp.update(cx, |w, cx| w.settings_changed(cx));
        }
        cx.refresh_windows();
    };
    match cmd {
        Cmd::OpenSettings => open_settings(cx),
        Cmd::SetTheme(t) => update(cx, &|s| s.theme = t),
        Cmd::ToggleAnimate => update(cx, &|s| s.animate = !s.animate),
        Cmd::TogglePause => update(cx, &|s| s.paused = !s.paused),
        Cmd::Shuffle => update(cx, &|s| s.seed = s.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 1),
        Cmd::Reattach => {
            if let Some(t) = cx.global::<AppState>().tray.as_ref() {
                t.readd();
            }
            if let Some(wp) = cx.global::<AppState>().wallpaper.clone() {
                let windows = cx.windows();
                for w in windows {
                    let _ = w.update(cx, |_, window, cx| {
                        if desktop::hwnd_of(window) == wp.read(cx).hwnd {
                            wp.update(cx, |wp, cx| {
                                wp.attach(window);
                                cx.notify();
                            });
                        }
                    });
                }
            }
        }
        Cmd::Exit => {
            if let Some(wp) = cx.global::<AppState>().wallpaper.clone() {
                wp.update(cx, |w, _| w.detach());
            }
            if let Some(t) = cx.global::<AppState>().tray.as_ref() {
                t.remove();
            }
            cx.quit();
        }
    }
}

struct Keeper;

impl Render for Keeper {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn open_wallpaper(cx: &mut App) {
    let r = desktop::primary_rect();
    let bounds = Bounds::new(point(px(r.left as f32), px(r.top as f32)), size(px((r.right - r.left) as f32), px((r.bottom - r.top) as f32)));
    let win = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Wololand wallpaper".into()), appears_transparent: false, traffic_light_position: None }),
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        |window, cx| {
            let e = cx.new(Wallpaper::new);
            e.update(cx, |w, _| w.attach(window));
            e
        },
    );
    if let Ok(win) = win {
        if let Ok(e) = win.entity(cx) {
            cx.global_mut::<AppState>().wallpaper = Some(e);
        }
    }
}

pub fn run(settings: Settings, open_ui: bool) {
    Application::new().run(move |cx: &mut App| {
        let game = Game::locate(Some(&settings.game_path)).map(Arc::new);
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Cmd>();
        let tray = Tray::create(tx);
        tray::sync(settings.theme, settings.animate, settings.paused);
        let has_game = game.is_some();
        cx.set_global(AppState { settings, game, wallpaper: None, settings_win: None, tray });

        // GPUI quits when its last window closes. Explorer destroys our wallpaper window when it
        // restarts (it is a child of Progman), so a hidden keeper window keeps the process alive.
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(1.), px(1.))))),
                titlebar: Some(TitlebarOptions { title: Some("Wololand keeper".into()), appears_transparent: false, traffic_light_position: None }),
                focus: false,
                show: false,
                kind: WindowKind::PopUp,
                ..Default::default()
            },
            |_, cx| cx.new(|_| Keeper),
        );
        if has_game {
            open_wallpaper(cx);
        }
        // Watchdog: re-create the wallpaper if Explorer took it down, re-attach if it got unparented.
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = cx.update(|cx| {
                let Some(wp) = cx.global::<AppState>().wallpaper.clone() else { return };
                let Some(h) = wp.read(cx).hwnd else { return };
                match desktop::attachment_state(h) {
                    desktop::AttachState::Ok => {}
                    desktop::AttachState::Gone => {
                        crate::log("wallpaper window lost; re-creating");
                        wp.update(cx, |w, _| w.forget());
                        cx.global_mut::<AppState>().wallpaper = None;
                        open_wallpaper(cx);
                    }
                    desktop::AttachState::Detached => {
                        crate::log("wallpaper detached; re-attaching");
                        apply_cmd(Cmd::Reattach, cx);
                    }
                }
            });
        })
        .detach();

        cx.spawn(async move |cx| {
            while let Some(cmd) = rx.next().await {
                let _ = cx.update(|cx| apply_cmd(cmd, cx));
            }
        })
        .detach();

        if open_ui || !has_game {
            open_settings(cx);
        }
    });
}

// ------------------------------------------------------------------------------ settings

pub fn open_settings(cx: &mut App) {
    if let Some(h) = cx.global::<AppState>().settings_win {
        if h.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(1060.0), px(950.0)), cx);
    let h = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // no system caption: the layout draws its own drag strip, minimize and close
            titlebar: Some(TitlebarOptions { title: Some("Wololand".into()), appears_transparent: true, traffic_light_position: None }),
            focus: true,
            show: true,
            window_min_size: Some(size(px(860.0), px(600.0))),
            ..Default::default()
        },
        |_, cx| cx.new(SettingsView::new),
    );
    if let Ok(h) = h {
        cx.global_mut::<AppState>().settings_win = Some(h);
        cx.activate(true);
    }
}

/// Switches to another game folder (`custom` empty = auto-detected) and rebuilds the wallpaper.
fn set_game(game: Option<Game>, custom: String, cx: &mut App) {
    let st = cx.global_mut::<AppState>();
    st.settings.game_path = custom;
    st.settings.save();
    st.game = game.map(Arc::new);
    let has = st.game.is_some();
    match st.wallpaper.clone() {
        Some(wp) => wp.update(cx, |w, cx| w.reload(cx)),
        None if has => open_wallpaper(cx),
        None => {}
    }
    cx.refresh_windows();
}

pub struct SettingsView {
    /// Message under the game folder row (a folder without the game, nothing found...).
    path_msg: Option<SharedString>,
    thumbs: Vec<Option<Arc<RenderImage>>>,
    autostart: bool,
    /// `resources/_common/wpfg` of the game install: the game's own menu art is used in place.
    ui: std::path::PathBuf,
    icon: Option<Arc<RenderImage>>,
}

impl SettingsView {
    fn new(cx: &mut Context<Self>) -> Self {
        let ui = cx.global::<AppState>().game.as_ref().map(|g| g.root().join("resources/_common/wpfg")).unwrap_or_default();
        let mut v = SettingsView { path_msg: None, thumbs: vec![None; Theme::ALL.len()], autostart: settings::autostart_enabled(), ui, icon: app_icon(28) };
        v.start_thumbs(cx);
        v
    }

    fn start_thumbs(&mut self, cx: &mut Context<Self>) {
        let Some(game) = cx.global::<AppState>().game.clone() else { return };
        self.thumbs = vec![None; Theme::ALL.len()];
        let root = game.root().to_path_buf();
        cx.spawn(async move |this, cx| {
            for (i, theme) in Theme::ALL.into_iter().enumerate() {
                let root = root.clone();
                let thumb = cx.background_spawn(async move { make_thumb(&root, theme) }).await;
                if this
                    .update(cx, |this, cx| {
                        this.thumbs[i] = thumb;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Applies a newly found game to the app and to this window.
    fn use_game(&mut self, game: Game, custom: String, cx: &mut Context<Self>) {
        self.ui = game.root().join("resources/_common/wpfg");
        self.path_msg = None;
        set_game(Some(game), custom, cx);
        self.start_thumbs(cx);
        cx.notify();
    }
}

/// A small preview: a full 1280x720 build around sample icons, box-filtered down 4x.
fn make_thumb(root: &std::path::Path, theme: Theme) -> Option<Arc<RenderImage>> {
    let game = Game::locate(Some(&root.to_string_lossy()))?;
    let kit = ThemeKit::load(&game, theme, 1.0).ok()?;
    let (w, h) = (1280usize, 720usize);
    let icons = scene::sample_icons(w, h, 1.0, 12);
    let sc = scene::build(&kit, w, h, &icons, 7);
    let f = 4usize;
    let (tw, th) = (w / f, h / f);
    let water = sc.water.as_ref().map(|(ww, wh, a, _)| (*ww, *wh, a));
    let mut out = vec![[0u8; 4]; tw * th];
    for y in 0..th {
        for x in 0..tw {
            let mut acc = [0f32; 3];
            for dy in 0..f {
                for dx in 0..f {
                    let (sx, sy) = (x * f + dx, y * f + dy);
                    let g = sc.ground[sy * w + sx];
                    let a = g[3] as f32 / 255.0;
                    let under = water.map_or([32, 40, 24, 255], |(ww, wh, wa)| wa[(sy % wh) * ww + sx % ww]);
                    for k in 0..3 {
                        acc[k] += g[k] as f32 * a + under[k] as f32 * (1.0 - a);
                    }
                }
            }
            let n = (f * f) as f32;
            out[y * tw + x] = [(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, 255];
        }
    }
    // sample icons as soft white tiles so the preview reads like a desktop
    for r in &icons {
        let (x0, y0) = ((r.x0 as usize + 14) / f, (r.y0 as usize + 6) / f);
        for y in y0..(y0 + 10).min(th) {
            for x in x0..(x0 + 10).min(tw) {
                let p = &mut out[y * tw + x];
                for k in 0..3 {
                    p[k] = ((p[k] as u16 + 3 * 235) / 4) as u8;
                }
            }
        }
    }
    Some(bgra_image(tw, th, &out))
}

// Styling after the game's own menus: parchment on dark wood, double gold rules around panels,
// red lacquered buttons and the map-pool frames. All art is read from the user's install.
const INK: u32 = 0x2a1a0b;
const INK_SOFT: u32 = 0x5a4126;
const RULE_OUT: u32 = 0xb39a55;
const RULE_IN: u32 = 0x6d5a32;
const PARCHMENT: u32 = 0xdcb894;
const BUTTON_TEXT: u32 = 0xf7e9c6;
const SERIF: &str = "Georgia";

/// Image stretched over its (relative) parent.
fn fill(p: std::path::PathBuf) -> Img {
    gpui::img(p).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Fill)
}

/// Red game button; `checked` uses the pressed-in gold variant (selected chip).
fn red_button(ui: &std::path::Path, id: impl Into<ElementId>, label: impl Into<SharedString>, w: f32, h: f32, checked: bool) -> Stateful<Div> {
    let dir = ui.join("resources/button_large");
    let (n, hv) = if checked {
        ("button_large_checked_normal.png", "button_large_checked_hover.png")
    } else {
        ("button_large_normal.png", "button_large_hover.png")
    };
    div()
        .id(id)
        .group("red-button")
        .relative()
        .w(px(w))
        .h(px(h))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(rgb(0x6e0b0b))
        .child(fill(dir.join(n)))
        .child(fill(dir.join(hv)).opacity(0.0).group_hover("red-button", |s| s.opacity(1.0)))
        .child(plate_text(label, BUTTON_TEXT))
}

/// The embedded app icon at `n`x`n`, for the custom title strip.
fn app_icon(n: u32) -> Option<Arc<RenderImage>> {
    let im = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?.to_rgba8();
    let im = image::imageops::resize(&im, n, n, image::imageops::FilterType::Triangle);
    let px: Vec<[u8; 4]> = im.pixels().map(|p| p.0).collect();
    Some(bgra_image(n as usize, n as usize, &px))
}

/// Title-strip button drawn with the game's red checkbox art.
fn caption_button(ui: &std::path::Path, id: &'static str, close: bool) -> Stateful<Div> {
    let dir = ui.join("resources/checkbox");
    div()
        .id(id)
        .group("caption-button")
        .relative()
        .size(px(26.0))
        .flex_none()
        .cursor_pointer()
        .bg(rgb(0x5a0a0a))
        .child(fill(dir.join("checkbox_red_normal.png")))
        .child(fill(dir.join("checkbox_red_hover.png")).opacity(0.0).group_hover("caption-button", |s| s.opacity(1.0)))
        .map(|d| {
            if close {
                d.child(gpui::img(dir.join("Cross_yellowgradient.png")).absolute().top(px(3.0)).left(px(3.0)).size(px(20.0)))
            } else {
                d.child(div().absolute().left(px(7.0)).right(px(7.0)).bottom(px(7.0)).h(px(3.0)).bg(rgb(0xe2b445)))
            }
        })
}

/// Light text with a dark outline, like the labels on the game's metal plates and buttons.
fn plate_text(text: impl Into<SharedString>, color: u32) -> Div {
    let text: SharedString = text.into();
    let ghost = |dx: f32, dy: f32| div().absolute().left(px(dx)).top(px(dy)).text_color(rgba(0x140a04d0)).child(text.clone());
    div()
        .relative()
        .font_weight(FontWeight::BOLD)
        .child(ghost(1.0, 1.0))
        .child(ghost(-1.0, 0.0))
        .child(ghost(1.0, -1.0))
        .child(div().relative().text_color(rgb(color)).child(text.clone()))
}

/// Game checkbox with a gold tick.
fn checkbox(ui: &std::path::Path, id: &'static str, label: &'static str, on: bool) -> Stateful<Div> {
    let dir = ui.join("resources/checkbox");
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .child(
            div()
                .relative()
                .size(px(22.0))
                .flex_none()
                .bg(rgb(0x5a0a0a))
                .child(fill(dir.join("checkbox_red_normal.png")))
                .when(on, |d| d.child(fill(dir.join("Tick_yellowgradient.png")))),
        )
        .child(div().text_color(rgb(INK)).child(label))
}

/// A panel with the game's double rule: gold outside, dark olive inside.
fn panel() -> Div {
    div().border_1().border_color(rgb(RULE_OUT)).p(px(3.0)).bg(rgba(0xfff3dc1c))
}

fn panel_inner() -> Div {
    div().size_full().border_1().border_color(rgb(RULE_IN)).p_4().flex().flex_col().gap_3()
}

fn heading(text: impl Into<SharedString>) -> Div {
    div().text_size(px(21.0)).font_weight(FontWeight::BOLD).text_color(rgb(INK)).child(text.into())
}

fn map_icon(theme: Theme) -> &'static str {
    match theme {
        Theme::Arabia => "rm_arabia.png",
        Theme::Arena => "rm_arena.png",
        Theme::BlackForest => "rm_black-forest.png",
        Theme::Islands => "rm_islands.png",
        Theme::Nomad => "rm_nomad.png",
        Theme::Hideout => "rm_hideout.png",
        Theme::GoldRush => "rm_gold-rush.png",
        Theme::ForestNothing => "rm_sherwood_forest.png",
        Theme::Fortress => "rm_fortress.png",
        Theme::IconWars => "rm_land_madness.png",
    }
}

/// A map-pool card like the ranked queue: minimap diamond in the metal frame, name on the plate,
/// gold frame and a star for the active theme.
fn map_card(ui: &std::path::Path, i: usize, theme: Theme, selected: bool, w: f32) -> Stateful<Div> {
    let dir = ui.join("resources/mapicons");
    let k = w / 490.0; // frame art is 490x478; the minimap diamond sits in a 450 square at (20, 1)
    let h = 478.0 * k;
    let (n, hv) = if selected {
        ("frame_multi_select_normal.png", "frame_multi_select_hover.png")
    } else {
        ("frame_deselected_normal.png", "frame_deselected_hover.png")
    };
    div()
        .id(("map", i))
        .group("map-card")
        .relative()
        .flex_none()
        .w(px(w))
        .h(px(h))
        .cursor_pointer()
        .child(gpui::img(dir.join(map_icon(theme))).absolute().left(px(20.0 * k)).top(px(1.0 * k)).size(px(450.0 * k)))
        .child(fill(dir.join(n)))
        .child(fill(dir.join(hv)).opacity(0.0).group_hover("map-card", |s| s.opacity(1.0)))
        .child(
            div()
                .absolute()
                .left_0()
                .w_full()
                .top(px(402.0 * k))
                .h(px(70.0 * k))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px((w * 0.1).max(12.0)))
                .child(plate_text(theme.name(), 0xf6f0e2)),
        )
        .when(selected, |d| {
            d.child(gpui::img(ui.join("resources/timeline/star.png")).absolute().right(px(18.0 * k)).top(px(14.0 * k)).size(px(92.0 * k)))
        })
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let st = cx.global::<AppState>();
        let s = st.settings.clone();
        let ui = self.ui.clone();
        let game_line: SharedString = match &st.game {
            Some(g) => format!("Art is read in place from {}", g.root().display()).into(),
            None => "Age of Empires II: Definitive Edition was not found. Choose its folder under Game Folder.".into(),
        };
        let status = st.wallpaper.as_ref().map(|w| w.read(cx).status.clone()).unwrap_or_default();
        let sel = Theme::ALL.iter().position(|&t| t == s.theme).unwrap_or(0);
        let thumb = self.thumbs[sel].clone();
        // The map pool spans the window: all nine cards in a row when they fit, else two rows.
        const CARD_GAP: f32 = 12.0;
        let avail = f32::from(window.viewport_size().width) - 196.0;
        let n = Theme::ALL.len() as f32;
        let per_row = if (avail - (n - 1.0) * CARD_GAP) / n >= 110.0 { n } else { (n / 2.0).ceil() };
        let card_w = ((avail - (per_row - 1.0) * CARD_GAP) / per_row).clamp(96.0, 170.0);

        // dark wood planks behind the parchment
        let planks = (0..14).map(|i| {
            let c = [0x3a2413, 0x2f1d0f, 0x402816, 0x342011][i % 4];
            div().flex_1().h_full().bg(rgb(c)).border_r_1().border_color(rgb(0x1a0f07))
        });

        let preview = div()
            .relative()
            .flex_none()
            .w(px(288.0))
            .h(px(162.0))
            .border_1()
            .border_color(rgb(RULE_OUT))
            .bg(rgb(0x2a2016))
            .flex()
            .items_center()
            .justify_center()
            .map(|d| match thumb {
                Some(t) => d.child(gpui::img(ImageSource::Render(t)).size_full().object_fit(ObjectFit::Cover)),
                None => d.child(div().text_color(rgb(PARCHMENT)).child("Drawing map...")),
            });

        let current = panel().child(
            panel_inner().child(
                div()
                    .flex()
                    .gap_5()
                    .child(preview)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_color(rgb(INK_SOFT)).child("Current map"))
                            .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(INK)).child(s.theme.name()))
                            .child(div().text_color(rgb(INK)).child(s.theme.blurb()))
                            .child(div().flex_1())
                            .child(div().text_sm().text_color(rgb(INK_SOFT)).child(status)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap_3()
                            .child(red_button(&ui, "shuffle", "Shuffle Layout", 190.0, 38.0, false).on_click(cx.listener(|_, _, _, cx| apply_cmd(Cmd::Shuffle, cx))))
                            .child(
                                red_button(&ui, "pause", if s.paused { "Resume" } else { "Pause" }, 190.0, 38.0, false)
                                    .on_click(cx.listener(|_, _, _, cx| apply_cmd(Cmd::TogglePause, cx))),
                            ),
                    ),
            ),
        );

        let fps_chips = [10u32, 15, 24, 30].into_iter().map(|f| {
            red_button(&ui, ("fps", f as usize), format!("{f}"), 52.0, 30.0, s.fps == f).on_click(cx.listener(move |_, _, _, cx| {
                let st = cx.global_mut::<AppState>();
                st.settings.fps = f;
                st.settings.save();
                cx.refresh_windows();
            }))
        });

        let options = panel().child(
            panel_inner().child(heading("Wallpaper Settings:")).child(
                div()
                    .flex()
                    .gap_6()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(checkbox(&ui, "animate", "Animate units and water", s.animate).on_click(cx.listener(|_, _, _, cx| apply_cmd(Cmd::ToggleAnimate, cx))))
                            .child(checkbox(&ui, "paused", "Pause animation", s.paused).on_click(cx.listener(|_, _, _, cx| apply_cmd(Cmd::TogglePause, cx))))
                            .child(checkbox(&ui, "autostart", "Start with Windows", self.autostart).on_click(cx.listener(|this, _, _, cx| {
                                this.autostart = !this.autostart;
                                settings::set_autostart(this.autostart);
                                cx.notify();
                            }))),
                    )
                    .child(div().w(px(1.0)).bg(rgb(RULE_IN)))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_color(rgb(INK)).font_weight(FontWeight::BOLD).child("Frames per second"))
                            .child(div().flex().gap_2().children(fps_chips))
                            .child(div().text_sm().text_color(rgb(INK_SOFT)).child("Animation pauses by itself while a maximized window covers the desktop.")),
                    ),
            ),
        );

        let cards = Theme::ALL.into_iter().enumerate().map(|(i, theme)| {
            map_card(&ui, i, theme, s.theme == theme, card_w).on_click(cx.listener(move |_, _, _, cx| apply_cmd(Cmd::SetTheme(theme), cx)))
        });
        let maps = panel().child(
            panel_inner()
                .child(heading("Map Themes:"))
                .child(div().flex().flex_wrap().justify_center().gap(px(CARD_GAP)).children(cards))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(rgb(INK))
                        .child(gpui::img(ui.join("resources/timeline/star.png")).size(px(20.0)))
                        .child("= Active map"),
                ),
        );

        let custom = !s.game_path.is_empty();
        let path_text: SharedString = match &st.game {
            Some(g) => format!("{}{}", g.root().display(), if custom { "  (set by you)" } else { "  (found automatically)" }).into(),
            None => "Not found. Pick the folder where Age of Empires II: Definitive Edition is installed.".into(),
        };
        let game_panel = panel().child(
            panel_inner()
                .child(heading("Game Folder:"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().flex_1().min_w_0().text_color(rgb(INK)).child(path_text))
                        .child(red_button(&ui, "pick-game", "Change Folder", 180.0, 34.0, false).on_click(cx.listener(|_, _, _, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: false,
                                directories: true,
                                multiple: false,
                                prompt: Some("Use this folder".into()),
                            });
                            cx.spawn(async move |this, cx| {
                                let Ok(Ok(Some(paths))) = rx.await else { return };
                                let Some(dir) = paths.into_iter().next() else { return };
                                let game = Game::at(&dir);
                                let _ = this.update(cx, |this, cx| match game {
                                    Some(g) => {
                                        let root = g.root().to_string_lossy().to_string();
                                        this.use_game(g, root, cx);
                                    }
                                    None => {
                                        this.path_msg = Some(format!("No Age of Empires II: DE game files in {}", dir.display()).into());
                                        cx.notify();
                                    }
                                });
                            })
                            .detach();
                        })))
                        .child(red_button(&ui, "auto-game", "Auto-detect", 150.0, 34.0, false).on_click(cx.listener(|this, _, _, cx| match Game::detect() {
                            Some(g) => this.use_game(g, String::new(), cx),
                            None => {
                                this.path_msg = Some("Not found on Steam or the Microsoft Store / Xbox app. Use Change Folder.".into());
                                cx.notify();
                            }
                        }))),
                )
                .when_some(self.path_msg.clone(), |d, m| d.child(div().text_sm().text_color(rgb(0x8e1010)).child(m)))
                .child(div().text_sm().text_color(rgb(INK_SOFT)).child("Steam and Microsoft Store / Xbox app installs are found automatically.")),
        );

        let title = div()
            .relative()
            .w(px(440.0))
            .h(px(46.0))
            .flex()
            .items_center()
            .justify_center()
            .child(fill(ui.join("WPFUI/Common/Resources/Images/header_ornaments_plain.png")))
            .child(div().relative().text_size(px(24.0)).font_weight(FontWeight::BOLD).text_color(rgb(INK)).child("Wololand"));

        let parchment = div()
            .relative()
            .size_full()
            .child(fill(ui.join("WPFUI/Common/Resources/Images/decisions_panel.png")))
            .child(
                div()
                    .id("settings-scroll")
                    .relative()
                    .size_full()
                    .overflow_y_scroll()
                    .px(px(56.0))
                    .pt(px(34.0))
                    .pb(px(64.0))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(title)
                            .child(div().italic().text_color(rgb(INK_SOFT)).child("Age of Empires II maps that grow around your desktop icons")),
                    )
                    .child(current)
                    .child(maps)
                    .child(options)
                    .child(game_panel)
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(red_button(&ui, "exit", "Exit and Restore Wallpaper", 270.0, 38.0, false).on_click(cx.listener(|_, _, _, cx| apply_cmd(Cmd::Exit, cx))))
                            .child(red_button(&ui, "close", "Close", 180.0, 38.0, false).on_click(cx.listener(|_, _, window, _| window.remove_window()))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_0p5()
                            .text_xs()
                            .text_color(rgb(INK))
                            .child(game_line)
                            .child("Requires your own copy of Age of Empires II: Definitive Edition. Wololand is a fan project, not affiliated with Microsoft or World's Edge."),
                    ),
            );

        const STRIP: f32 = 40.0;
        let title_strip = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(STRIP))
            .flex()
            .items_center()
            .gap_2()
            .px(px(16.0))
            .when_some(self.icon.clone(), |d, ic| d.child(gpui::img(ImageSource::Render(ic)).size(px(28.0)).flex_none()))
            .child(plate_text("Wololand", PARCHMENT))
            .child(div().flex_1())
            // the whole strip except the buttons moves the window
            .child(div().absolute().top_0().left_0().bottom_0().right(px(90.0)).window_control_area(WindowControlArea::Drag))
            .child(caption_button(&ui, "minimize", false).on_click(cx.listener(|_, _, window, _| window.minimize_window())))
            .child(caption_button(&ui, "close-window", true).on_click(cx.listener(|_, _, window, _| window.remove_window())));

        div()
            .size_full()
            .relative()
            .font_family(SERIF)
            .text_size(px(15.0))
            .bg(rgb(0x2b1a0d))
            .child(div().absolute().size_full().flex().children(planks))
            .child(div().absolute().top(px(STRIP)).bottom(px(12.0)).left(px(16.0)).right(px(16.0)).child(parchment))
            .child(title_strip)
    }
}
