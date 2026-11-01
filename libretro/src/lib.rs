#![allow(unsafe_op_in_unsafe_fn)]
mod audio;
mod filters;
mod fonts;
mod gl;
mod glsl;
mod input;
mod keyboard;
mod offline;
mod pacing;
mod performance;
mod pixelbender;
mod savestate;
mod session;
mod ui;
use ruffle_core::{
    FloatDuration, Player, PlayerBuilder, backend::audio::*, events::*, impl_audio_mixer_backend,
};
use ruffle_render::quality::StageQuality;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::PathBuf,
    ptr,
    time::Duration,
};

type Environment = unsafe extern "C" fn(u32, *mut c_void) -> bool;
type Video = unsafe extern "C" fn(*const c_void, u32, u32, usize);
type Audio = unsafe extern "C" fn(i16, i16);
type AudioBatch = unsafe extern "C" fn(*const i16, usize) -> usize;
type Poll = unsafe extern "C" fn();
type Input = unsafe extern "C" fn(u32, u32, u32, u32) -> i16;
#[repr(C)]
pub struct SystemInfo {
    name: *const c_char,
    version: *const c_char,
    extensions: *const c_char,
    fullpath: bool,
    extract: bool,
}
#[repr(C)]
pub struct Geometry {
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
    aspect: f32,
}
#[repr(C)]
pub struct Timing {
    fps: f64,
    sample_rate: f64,
}
#[repr(C)]
pub struct AvInfo {
    geometry: Geometry,
    timing: Timing,
}
#[repr(C)]
pub struct GameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}
#[repr(C)]
struct Variable {
    key: *const c_char,
    value: *const c_char,
}
#[repr(C)]
struct Keyboard {
    callback: unsafe extern "C" fn(bool, u32, u32, u16),
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Hardware {
    kind: u32,
    reset: Option<unsafe extern "C" fn()>,
    framebuffer: Option<unsafe extern "C" fn() -> usize>,
    proc: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
    depth: bool,
    stencil: bool,
    bottom_left: bool,
    major: u32,
    minor: u32,
    cache: bool,
    destroy: Option<unsafe extern "C" fn()>,
    debug: bool,
}
struct Trace;
impl ruffle_core::backend::log::LogBackend for Trace {
    fn avm_trace(&self, s: &str) {
        log(s)
    }
    fn avm_warning(&self, s: &str) {
        log(s)
    }
}
struct Mixer {
    mixer: AudioMixer,
}
impl AudioBackend for Mixer {
    impl_audio_mixer_backend!(mixer);
    fn play(&mut self) {}
    fn pause(&mut self) {}
}
struct State {
    env: Option<Environment>,
    video: Option<Video>,
    audio: Option<Audio>,
    batch: Option<AudioBatch>,
    poll: Option<Poll>,
    input: Option<Input>,
    hw: Hardware,
    player: Option<std::sync::Arc<std::sync::Mutex<Player>>>,
    mixer: Option<AudioMixerProxy>,
    audio_float: Vec<f32>,
    audio_output: audio::Output,
    path: Option<PathBuf>,
    w: u32,
    h: u32,
    mouse: (f64, f64),
    buttons: [bool; 3],
    keyboard: keyboard::Keyboard,
    options: Vec<CString>,
    variables: Vec<Variable>,
    save: PathBuf,
    sensitivity: f64,
    volume: f32,
    cursor: bool,
    cursor_size: f32,
    frames: u64,
    performance: performance::Window,
    metrics: performance::Metrics,
    frontend_performance: performance::Frontend,
    pacing: pacing::Pacing,
    automatic_frameskip: bool,
    quality_option: StageQuality,
    pending_quality: Option<StageQuality>,
    applied_quality: Option<StageQuality>,
    pointer: bool,
    pad_keys: [u32; 16],
    executor: ruffle_core::backend::navigator::NullExecutor,
    settings: serde_json::Value,
    journal: Option<savestate::Tape>,
    restoring: Option<savestate::Restore>,
    initial_override: Option<savestate::Initial>,
    option_override: Option<std::collections::BTreeMap<String, String>>,
    clock: Option<savestate::Clock>,
    storage: Option<savestate::Storage>,
    audio_signature: sha2::Sha256,
}
impl Default for State {
    fn default() -> Self {
        Self {
            env: None,
            video: None,
            audio: None,
            batch: None,
            poll: None,
            input: None,
            hw: Hardware {
                kind: 1,
                reset: Some(context_reset),
                framebuffer: None,
                proc: None,
                depth: true,
                stencil: true,
                bottom_left: true,
                major: 3,
                minor: 0,
                cache: false,
                destroy: Some(context_destroy),
                debug: false,
            },
            player: None,
            mixer: None,
            audio_float: vec![0.; 1600],
            audio_output: audio::Output::default(),
            path: None,
            w: 640,
            h: 480,
            mouse: (320., 240.),
            buttons: [false; 3],
            keyboard: keyboard::Keyboard::default(),
            options: vec![],
            variables: vec![],
            save: PathBuf::from("."),
            sensitivity: 1.,
            volume: 1.,
            cursor: true,
            cursor_size: 24.,
            frames: 0,
            performance: performance::Window::default(),
            metrics: performance::Metrics::default(),
            frontend_performance: performance::Frontend::default(),
            pacing: pacing::Pacing::default(),
            automatic_frameskip: true,
            quality_option: StageQuality::High,
            pending_quality: None,
            applied_quality: None,
            pointer: false,
            pad_keys: [
                122, 97, 32, 13, 273, 274, 276, 275, 120, 115, 113, 119, 0, 0, 0, 0,
            ],
            executor: ruffle_core::backend::navigator::NullExecutor::new(),
            settings: serde_json::Value::Null,
            journal: None,
            restoring: None,
            initial_override: None,
            option_override: None,
            clock: None,
            storage: None,
            audio_signature: <sha2::Sha256 as sha2::Digest>::new(),
        }
    }
}
// RetroArch may create the GL context on its video thread. Every access is serialized;
// no Player, GC, renderer or executor references escape this owner.
unsafe impl Send for State {}
struct StateAccess;
impl StateAccess {
    fn borrow(&self) -> std::sync::MutexGuard<'static, State> {
        self.borrow_mut()
    }
    fn borrow_mut(&self) -> std::sync::MutexGuard<'static, State> {
        static STATE: std::sync::OnceLock<std::sync::Mutex<State>> = std::sync::OnceLock::new();
        STATE
            .get_or_init(|| std::sync::Mutex::new(State::default()))
            .lock()
            .unwrap()
    }
}
fn with_state<T>(f: impl FnOnce(&StateAccess) -> T) -> T {
    f(&StateAccess)
}
static EVENTS: std::sync::Mutex<Vec<keyboard::Callback>> = std::sync::Mutex::new(Vec::new());
type Log = unsafe extern "C" fn(u32, *const c_char, ...);
static LOG: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(ptr::null_mut());
#[repr(C)]
struct Logger {
    callback: Option<Log>,
}
fn log(message: impl AsRef<str>) {
    let callback = LOG.load(std::sync::atomic::Ordering::Relaxed);
    if !callback.is_null() {
        let message = CString::new(message.as_ref().replace('\0', "")).unwrap();
        unsafe {
            let callback: Log = std::mem::transmute(callback);
            callback(1, c"[Ruffle] %s\n".as_ptr(), message.as_ptr());
        }
    }
}
impl State {
    fn environment<T>(&self, command: u32, data: &mut T) -> bool {
        self.env
            .is_some_and(|f| unsafe { f(command, data as *mut T as *mut c_void) })
    }
    fn option(&self, key: &str) -> Option<String> {
        if let Some(options) = &self.option_override
            && let Some(value) = options.get(key)
        {
            return Some(value.clone());
        }
        if let Some(v) = self.settings.get(key) {
            return Some(
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string()),
            );
        }
        let key = CString::new(key).ok()?;
        let mut v = Variable {
            key: key.as_ptr(),
            value: ptr::null(),
        };
        if self.environment(15, &mut v) && !v.value.is_null() {
            Some(
                unsafe { CStr::from_ptr(v.value) }
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        }
    }
    fn read_options(&mut self) {
        let quality = self
            .option("ruffle_quality")
            .and_then(|q| q.parse().ok())
            .unwrap_or(StageQuality::High);
        if quality != self.quality_option {
            self.pending_quality = Some(quality);
        }
        self.quality_option = quality;
        if let Some(restore) = &mut self.restoring {
            restore.resume_quality = Some(quality.to_string());
        }
        self.automatic_frameskip = self.option("ruffle_frame_skip").as_deref() != Some("disabled");
        self.sensitivity = self
            .option("ruffle_mouse_speed")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.);
        self.volume = self
            .option("ruffle_volume")
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(100.)
            / 100.;
        self.cursor = self.option("ruffle_cursor").as_deref() != Some("disabled");
        self.cursor_size = self
            .option("ruffle_cursor_size")
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(24.)
            .clamp(8., 64.);
        self.pointer = self.option("ruffle_mouse_mode").as_deref() == Some("pointer");
        for (i, name) in [
            "b", "y", "select", "start", "up", "down", "left", "right", "a", "x", "l", "r",
        ]
        .iter()
        .enumerate()
        {
            if let Some(value) = self.option(&format!("ruffle_pad_{name}")) {
                self.pad_keys[i] = match value.as_str() {
                    "none" => 0,
                    "enter" => 13,
                    "space" => 32,
                    "up" => 273,
                    "down" => 274,
                    "left" => 276,
                    "right" => 275,
                    "shift" => 304,
                    "ctrl" => 306,
                    "escape" => 27,
                    _ if value.len() == 1 => value.as_bytes()[0] as u32,
                    _ => self.pad_keys[i],
                };
            }
        }
    }
    fn create_player(&mut self) -> Result<(), String> {
        self.performance = performance::Window::default();
        let path = offline::local_path(self.path.as_ref().ok_or("No content")?)?;
        let restored = self.initial_override.take();
        let sidecar = offline::local_path(&path.with_extension("ruffle.json"))?;
        self.settings = if let Some(initial) = &restored {
            initial.settings.clone()
        } else {
            match std::fs::read(sidecar) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Invalid per-game settings: {e}"))?,
                Err(_) => serde_json::Value::Null,
            }
        };
        self.option_override = restored.as_ref().map(|i| i.options.clone());
        let mut movie =
            ruffle_core::tag_utils::movie_from_path(&path, None).map_err(|e| e.to_string())?;
        if let Some(vars) = self.settings.get("parameters").and_then(|v| v.as_object()) {
            movie.append_parameters(
                vars.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned())),
            );
        }
        let load = self.hw.proc.ok_or("No GL loader")?;
        let renderer = unsafe {
            gl::GlRenderer::new(
                |s| {
                    let s = CString::new(s).unwrap();
                    load(s.as_ptr())
                },
                self.w,
                self.h,
            )
        }?;
        let mixer = AudioMixer::new(2, 48000);
        self.mixer = Some(mixer.proxy());
        self.read_options();
        let options = self
            .variables
            .iter()
            .filter(|v| !v.key.is_null())
            .filter_map(|v| {
                let key = unsafe { CStr::from_ptr(v.key) }
                    .to_string_lossy()
                    .into_owned();
                self.option(&key).map(|value| (key, value))
            })
            .collect();
        let (epoch_micros, timezone) = savestate::Clock::now();
        let movie_hash = savestate::hash(&std::fs::read(&path).map_err(|e| e.to_string())?);
        let initial = restored.clone().unwrap_or_else(|| savestate::Initial {
            movie_hash,
            options,
            settings: self.settings.clone(),
            epoch_micros,
            timezone,
            memory_storage: self.option("ruffle_storage").as_deref() == Some("memory"),
            saves: Default::default(),
        });
        let clock = savestate::Clock::new(initial.epoch_micros, initial.timezone);
        self.clock = Some(clock.clone());
        let storage = savestate::Storage::new(
            self.save.join("ruffle"),
            initial.memory_storage,
            restored.as_ref().map(|i| i.saves.clone()),
        );
        self.storage = Some(storage.clone());
        let quality = match self.option("ruffle_quality").as_deref() {
            Some("low") => StageQuality::Low,
            Some("medium") => StageQuality::Medium,
            _ => StageQuality::High,
        };
        self.executor = ruffle_core::backend::navigator::NullExecutor::new();
        let base = self
            .option("ruffle_base_path")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.path.clone().unwrap());
        let navigator = offline::OfflineNavigator::new(&base, &self.executor)?;
        let mut builder = PlayerBuilder::new()
            .with_navigator(navigator)
            .with_ui(ui::CoreUi::new())
            .with_log(Trace)
            .with_movie(movie)
            .with_renderer(renderer)
            .with_audio(Mixer { mixer })
            .with_storage(Box::new(storage))
            .with_locale(clock)
            .with_video(ruffle_video_software::backend::SoftwareVideoBackend::new())
            .with_autoplay(true)
            .with_default_font(true)
            .with_viewport_dimensions(self.w, self.h, 1.0)
            .with_quality(quality)
            .with_max_execution_duration(Duration::from_secs(
                self.option("ruffle_script_timeout")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(15),
            ))
            .with_player_version(
                self.option("ruffle_player_version")
                    .and_then(|s| s.parse().ok()),
            )
            .with_avm2_optimizer_enabled(
                self.option("ruffle_avm2_optimizer").as_deref() != Some("disabled"),
            );
        builder = builder
            .with_scale_mode(
                self.option("ruffle_scale")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(ruffle_core::StageScaleMode::ShowAll),
                self.option("ruffle_force_scale").as_deref() == Some("enabled"),
            )
            .with_align(
                self.option("ruffle_align")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_default(),
                self.option("ruffle_force_align").as_deref() == Some("enabled"),
            )
            .with_letterbox(
                self.option("ruffle_letterbox")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(ruffle_core::config::Letterbox::On),
            )
            .with_player_runtime(if self.option("ruffle_runtime").as_deref() == Some("air") {
                ruffle_core::PlayerRuntime::AIR
            } else {
                ruffle_core::PlayerRuntime::FlashPlayer
            })
            .with_player_mode(
                if self.option("ruffle_player_mode").as_deref() == Some("debug") {
                    ruffle_core::PlayerMode::Debug
                } else {
                    ruffle_core::PlayerMode::Release
                },
            )
            .with_spoofed_url(self.option("ruffle_spoofed_url"))
            .with_page_url(self.option("ruffle_page_url"))
            .with_load_behavior(match self.option("ruffle_load_behavior").as_deref() {
                Some("blocking") => ruffle_core::LoadBehavior::Blocking,
                Some("delayed") => ruffle_core::LoadBehavior::Delayed,
                _ => ruffle_core::LoadBehavior::Streaming,
            });
        if let Some(fps) = self
            .option("ruffle_frame_rate")
            .and_then(|s| s.parse().ok())
        {
            builder = builder.with_frame_rate(Some(fps));
        }
        let player = builder.build();
        player.lock().unwrap().set_deterministic_time(Some(0));
        player
            .lock()
            .unwrap()
            .handle_event(PlayerEvent::FocusGained);
        self.player = Some(player);
        self.keyboard = keyboard::Keyboard::default();
        self.audio_output = audio::Output::default();
        self.audio_signature = <sha2::Sha256 as sha2::Digest>::new();
        self.pacing = pacing::Pacing::default();
        self.pending_quality = None;
        self.applied_quality = None;
        EVENTS.lock().unwrap().clear();
        self.buttons.fill(false);
        self.frames = 0;
        self.mouse = (self.w as f64 / 2., self.h as f64 / 2.);
        self.journal = Some(savestate::Tape::new(initial));
        self.option_override = None;
        log("Player created, native OpenGL; audio 48000 Hz");
        Ok(())
    }
}
fn key_descriptor(id: u32) -> Option<KeyDescriptor> {
    let physical_key = match id {
        9 => PhysicalKey::Tab,
        303 => PhysicalKey::ShiftRight,
        281 => PhysicalKey::PageDown,
        280 => PhysicalKey::PageUp,
        279 => PhysicalKey::End,
        278 => PhysicalKey::Home,
        277 => PhysicalKey::Insert,
        276 => PhysicalKey::ArrowLeft,
        275 => PhysicalKey::ArrowRight,
        274 => PhysicalKey::ArrowDown,
        273 => PhysicalKey::ArrowUp,
        308 => PhysicalKey::AltLeft,
        32 => PhysicalKey::Space,
        27 => PhysicalKey::Escape,
        127 => PhysicalKey::Delete,
        13 => PhysicalKey::Enter,
        307 => PhysicalKey::AltRight,
        306 => PhysicalKey::ControlLeft,
        305 => PhysicalKey::ControlRight,
        304 => PhysicalKey::ShiftLeft,
        8 => PhysicalKey::Backspace,
        97 => PhysicalKey::KeyA,
        98 => PhysicalKey::KeyB,
        99 => PhysicalKey::KeyC,
        100 => PhysicalKey::KeyD,
        101 => PhysicalKey::KeyE,
        102 => PhysicalKey::KeyF,
        103 => PhysicalKey::KeyG,
        104 => PhysicalKey::KeyH,
        105 => PhysicalKey::KeyI,
        106 => PhysicalKey::KeyJ,
        107 => PhysicalKey::KeyK,
        108 => PhysicalKey::KeyL,
        109 => PhysicalKey::KeyM,
        110 => PhysicalKey::KeyN,
        111 => PhysicalKey::KeyO,
        112 => PhysicalKey::KeyP,
        113 => PhysicalKey::KeyQ,
        114 => PhysicalKey::KeyR,
        115 => PhysicalKey::KeyS,
        116 => PhysicalKey::KeyT,
        117 => PhysicalKey::KeyU,
        118 => PhysicalKey::KeyV,
        119 => PhysicalKey::KeyW,
        120 => PhysicalKey::KeyX,
        121 => PhysicalKey::KeyY,
        122 => PhysicalKey::KeyZ,
        48 => PhysicalKey::Digit0,
        49 => PhysicalKey::Digit1,
        50 => PhysicalKey::Digit2,
        51 => PhysicalKey::Digit3,
        52 => PhysicalKey::Digit4,
        53 => PhysicalKey::Digit5,
        54 => PhysicalKey::Digit6,
        55 => PhysicalKey::Digit7,
        56 => PhysicalKey::Digit8,
        57 => PhysicalKey::Digit9,
        _ => PhysicalKey::Unknown,
    };
    let logical_key = match id {
        8 => LogicalKey::Named(NamedKey::Backspace),
        9 => LogicalKey::Named(NamedKey::Tab),
        13 => LogicalKey::Named(NamedKey::Enter),
        27 => LogicalKey::Named(NamedKey::Escape),
        127 => LogicalKey::Named(NamedKey::Delete),
        273 => LogicalKey::Named(NamedKey::ArrowUp),
        274 => LogicalKey::Named(NamedKey::ArrowDown),
        275 => LogicalKey::Named(NamedKey::ArrowRight),
        276 => LogicalKey::Named(NamedKey::ArrowLeft),
        278 => LogicalKey::Named(NamedKey::Home),
        279 => LogicalKey::Named(NamedKey::End),
        280 => LogicalKey::Named(NamedKey::PageUp),
        281 => LogicalKey::Named(NamedKey::PageDown),
        303 | 304 => LogicalKey::Named(NamedKey::Shift),
        305 | 306 => LogicalKey::Named(NamedKey::Control),
        307 | 308 => LogicalKey::Named(NamedKey::Alt),
        32..=126 => LogicalKey::Character(char::from_u32(id)?),
        _ => return None,
    };
    Some(KeyDescriptor {
        physical_key,
        logical_key,
        key_location: KeyLocation::Standard,
    })
}
unsafe extern "C" fn keyboard(down: bool, key: u32, character: u32, modifiers: u16) {
    EVENTS.lock().unwrap().push(keyboard::Callback {
        down,
        key,
        character,
        modifiers,
    });
}
unsafe extern "C" fn context_reset() {
    with_state(|s| {
        let mut s = s.borrow_mut();
        if let Some(player) = &s.player {
            let mut player = player.lock().unwrap();
            if let Some(r) =
                (player.renderer_mut() as &mut dyn std::any::Any).downcast_mut::<gl::GlRenderer>()
            {
                r.abandon();
            }
        }
        s.executor = ruffle_core::backend::navigator::NullExecutor::new();
        s.player = None;
        if let Err(e) = s.create_player() {
            log(format!("GL reset failed: {e}"));
        }
    });
}
unsafe extern "C" fn context_destroy() {
    with_state(|s| {
        let mut s = s.borrow_mut();
        s.executor = ruffle_core::backend::navigator::NullExecutor::new();
        s.player = None;
        s.mixer = None;
        s.journal = None;
        s.restoring = None;
        s.storage = None;
        s.clock = None;
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_api_version() -> u32 {
    1
}
#[unsafe(no_mangle)]
/// # Safety
/// The frontend must supply a valid libretro environment callback that remains callable until deinitialization.
pub unsafe extern "C" fn retro_set_environment(env: Environment) {
    with_state(|s| {
        let mut s = s.borrow_mut();
        s.env = Some(env);
        let mut logger = Logger { callback: None };
        if s.environment(27, &mut logger) {
            LOG.store(
                logger
                    .callback
                    .map(|f| f as *mut c_void)
                    .unwrap_or(ptr::null_mut()),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        let specs = [
            ("ruffle_mouse_mode", "Mouse position mode; relative|pointer"),
            (
                "ruffle_pad_b",
                "Gamepad B key; z|x|a|s|space|enter|ctrl|shift|none",
            ),
            (
                "ruffle_pad_a",
                "Gamepad A key; x|z|a|s|space|enter|ctrl|shift|none",
            ),
            (
                "ruffle_pad_y",
                "Gamepad Y key; a|s|z|x|space|enter|ctrl|shift|none",
            ),
            (
                "ruffle_pad_x",
                "Gamepad X key; s|a|z|x|space|enter|ctrl|shift|none",
            ),
            (
                "ruffle_pad_l",
                "Gamepad L key; q|w|e|r|a|s|z|x|space|ctrl|shift|none",
            ),
            (
                "ruffle_pad_r",
                "Gamepad R key; w|q|e|r|a|s|z|x|space|ctrl|shift|none",
            ),
            (
                "ruffle_pad_start",
                "Gamepad Start key; enter|space|escape|none",
            ),
            (
                "ruffle_pad_select",
                "Gamepad Select key; space|enter|escape|none",
            ),
            ("ruffle_pad_up", "Gamepad Up key; up|w|none"),
            ("ruffle_pad_down", "Gamepad Down key; down|s|none"),
            ("ruffle_pad_left", "Gamepad Left key; left|a|none"),
            ("ruffle_pad_right", "Gamepad Right key; right|d|none"),
            (
                "ruffle_scale",
                "Stage scale (restart); show_all|exact_fit|no_border|no_scale",
            ),
            (
                "ruffle_align",
                "Stage alignment (restart); center|top_left|top|top_right|left|right|bottom_left|bottom|bottom_right",
            ),
            (
                "ruffle_force_scale",
                "Force stage scale (restart); disabled|enabled",
            ),
            (
                "ruffle_force_align",
                "Force alignment (restart); disabled|enabled",
            ),
            ("ruffle_letterbox", "Letterbox (restart); on|off|fullscreen"),
            ("ruffle_runtime", "Runtime (restart); flash_player|air"),
            ("ruffle_player_mode", "Player mode (restart); release|debug"),
            (
                "ruffle_load_behavior",
                "Load behavior (restart); streaming|delayed|blocking",
            ),
            ("ruffle_storage", "Game saves (restart); disk|memory"),
            ("ruffle_quality", "Quality; high|medium|low"),
            ("ruffle_mouse_speed", "Mouse sensitivity; 1|0.5|1.5|2|3"),
            ("ruffle_volume", "Volume (%); 100|75|50|25|0"),
            (
                "ruffle_frame_skip",
                "Skip rendering under load (preserve audio); automatic|disabled",
            ),
            ("ruffle_cursor", "Show cursor; enabled|disabled"),
            ("ruffle_cursor_size", "Cursor size; 24|32|16|48"),
            (
                "ruffle_frame_rate",
                "Flash frame rate (restart); native|30|60|24|50",
            ),
            (
                "ruffle_player_version",
                "Flash Player version (restart); auto|32|11|10|9|8|7|6",
            ),
            (
                "ruffle_script_timeout",
                "Script timeout seconds (restart); 15|30|60|5",
            ),
            (
                "ruffle_avm2_optimizer",
                "AVM2 optimizer (restart); enabled|disabled",
            ),
        ];
        s.options = specs
            .iter()
            .flat_map(|(k, v)| [CString::new(*k).unwrap(), CString::new(*v).unwrap()])
            .collect();
        s.variables = (0..specs.len())
            .map(|i| Variable {
                key: s.options[i * 2].as_ptr(),
                value: s.options[i * 2 + 1].as_ptr(),
            })
            .collect();
        s.variables.push(Variable {
            key: ptr::null(),
            value: ptr::null(),
        });
        env(16, s.variables.as_mut_ptr().cast());
        let mut keyboard = Keyboard { callback: keyboard };
        s.environment(12, &mut keyboard);
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_set_video_refresh(f: Video) {
    with_state(|s| s.borrow_mut().video = Some(f));
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample(f: Audio) {
    with_state(|s| s.borrow_mut().audio = Some(f));
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample_batch(f: AudioBatch) {
    with_state(|s| s.borrow_mut().batch = Some(f));
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_poll(f: Poll) {
    with_state(|s| s.borrow_mut().poll = Some(f));
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_state(f: Input) {
    with_state(|s| s.borrow_mut().input = Some(f));
}
#[unsafe(no_mangle)]
/// # Safety
/// A non-null info pointer must be aligned and writable for one SystemInfo.
pub unsafe extern "C" fn retro_get_system_info(info: *mut SystemInfo) {
    if !info.is_null() {
        *info = SystemInfo {
            name: c"Ruffle".as_ptr(),
            version: concat!(env!("CARGO_PKG_VERSION"), " OpenGL\0")
                .as_ptr()
                .cast(),
            extensions: c"swf".as_ptr(),
            fullpath: true,
            extract: false,
        };
    }
}
#[unsafe(no_mangle)]
/// # Safety
/// A non-null info pointer must be aligned and writable for one AvInfo.
pub unsafe extern "C" fn retro_get_system_av_info(info: *mut AvInfo) {
    if !info.is_null() {
        with_state(|s| {
            let s = s.borrow();
            *info = AvInfo {
                geometry: Geometry {
                    width: s.w,
                    height: s.h,
                    max_width: s.w,
                    max_height: s.h,
                    aspect: s.w as f32 / s.h as f32,
                },
                timing: Timing {
                    fps: 60.,
                    sample_rate: 48000.,
                },
            };
        });
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_init() {
    log("Ruffle libretro init");
    with_state(|s| {
        let mut quirks = 1u64;
        s.borrow().environment(44, &mut quirks);
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_deinit() {
    unsafe {
        context_destroy();
    }
    with_state(|s| s.borrow_mut().path = None);
}
#[unsafe(no_mangle)]
/// # Safety
/// A non-null info pointer and its NUL-terminated path must remain readable for this call.
pub unsafe extern "C" fn retro_load_game(info: *const GameInfo) -> bool {
    if info.is_null() || (*info).path.is_null() {
        return false;
    }
    let path = PathBuf::from(CStr::from_ptr((*info).path).to_string_lossy().into_owned());
    let Ok(path) = offline::local_path(&path) else {
        log("Blocked non-local SWF path");
        return false;
    };
    let Ok(movie) = ruffle_core::tag_utils::movie_from_path(&path, None) else {
        log("Invalid SWF");
        return false;
    };
    with_state(|state| {
        let mut s = state.borrow_mut();
        s.w = (movie.width().to_pixels() as u32).clamp(1, 4096);
        s.h = (movie.height().to_pixels() as u32).clamp(1, 4096);
        s.mouse = (s.w as f64 / 2., s.h as f64 / 2.);
        s.path = Some(path);
        s.journal = None;
        s.restoring = None;
        s.storage = None;
        s.frames = 0;
        let mut save: *const c_char = ptr::null();
        if s.environment(31, &mut save) && !save.is_null() {
            s.save = PathBuf::from(CStr::from_ptr(save).to_string_lossy().into_owned());
        }
        let mut format = 1u32;
        s.environment(10, &mut format);
    });
    // Frontends may invoke context callbacks synchronously; don't borrow state across this call.
    let (env, mut hw) = with_state(|s| {
        let s = s.borrow();
        (s.env, s.hw)
    });
    let ok = env.is_some_and(|env| env(14, (&mut hw as *mut Hardware).cast()));
    with_state(|s| {
        s.borrow_mut().hw = hw;
        if !ok {
            s.borrow_mut().path = None;
        }
    });
    if !ok {
        log("Frontend rejected OpenGL");
    }
    ok
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_unload_game() {
    unsafe {
        context_destroy();
    }
    with_state(|s| s.borrow_mut().path = None);
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_reset() {
    with_state(|s| {
        let mut s = s.borrow_mut();
        s.restoring = None;
        s.player = None;
        if let Err(e) = s.create_player() {
            log(e);
        }
    });
}
#[unsafe(no_mangle)]
/// # Safety
/// The frontend must keep all registered callbacks valid and make the negotiated OpenGL context current.
pub unsafe extern "C" fn retro_run() {
    let poll = with_state(|s| s.borrow().poll);
    if let Some(poll) = poll {
        poll();
    }
    let output = with_state(|state| {
        let work_started = std::time::Instant::now();
        let mut s = state.borrow_mut();
        let mut changed = false;
        if s.environment(17, &mut changed) && changed {
            s.read_options();
        }
        let fb = s.hw.framebuffer.map(|f| f()).unwrap_or(0);
        let (w, h) = (s.w, s.h);
        let (samples, rendered) = if s.restoring.is_some() {
            s.restore_chunk(fb)
        } else {
            let input = s.capture_input();
            let automatic = s.automatic_frameskip;
            let rendered = s.pacing.render(automatic);
            let volume = s.volume;
            let samples = s.step(&input, rendered, volume, fb, false);
            s.record_frame(input, rendered, volume);
            if let Some(message) = s
                .pacing
                .finish(work_started.elapsed().as_secs_f64() * 1000., rendered)
            {
                log(message);
            }
            (samples, rendered)
        };
        (
            s.video,
            s.batch,
            s.audio,
            w,
            h,
            samples,
            rendered,
            work_started.elapsed().as_secs_f64() * 1000.,
        )
    });
    let (video, batch, audio, w, h, samples, rendered, core_ms) = output;
    let video_started = std::time::Instant::now();
    if let Some(video) = video {
        video(
            if rendered {
                usize::MAX as *const c_void
            } else {
                ptr::null()
            },
            w,
            h,
            0,
        );
    }
    let video_ms = video_started.elapsed().as_secs_f64() * 1000.;
    let audio_started = std::time::Instant::now();
    if let Some(batch) = batch {
        batch(samples.as_ptr(), 800);
    } else if let Some(audio) = audio {
        for frame in samples.as_chunks::<2>().0 {
            audio(frame[0], frame[1]);
        }
    }
    let audio_ms = audio_started.elapsed().as_secs_f64() * 1000.;
    with_state(|s| {
        if let Some(message) = s
            .borrow_mut()
            .frontend_performance
            .record(core_ms, video_ms, audio_ms)
        {
            log(message);
        }
    });
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_get_region() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_set_controller_port_device(_: u32, _: u32) {}
#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize_size() -> usize {
    savestate::CAPACITY
}
#[unsafe(no_mangle)]
/// # Safety
/// A non-null data pointer must be writable for at least size bytes and must not alias core state.
pub unsafe extern "C" fn retro_serialize(data: *mut c_void, size: usize) -> bool {
    if data.is_null() || size < savestate::CAPACITY {
        return false;
    }
    with_state(|state| {
        let s = state.borrow();
        if s.restoring.is_some() || s.player.is_none() {
            return false;
        }
        let Some(mut tape) = s.journal.clone() else {
            return false;
        };
        {
            use sha2::Digest;
            tape.audio_signature = Some(
                s.audio_signature
                    .clone()
                    .finalize()
                    .iter()
                    .map(|x| format!("{x:02x}"))
                    .collect(),
            );
        }
        if let Some(storage) = &s.storage {
            match storage.initial() {
                Ok(initial) => tape.initial.saves = initial,
                Err(error) => {
                    log(error);
                    return false;
                }
            }
        }
        match tape.encode() {
            Ok(bytes) => {
                ptr::write_bytes(data as *mut u8, 0, savestate::CAPACITY);
                ptr::copy_nonoverlapping(bytes.as_ptr(), data as *mut u8, bytes.len());
                log(format!(
                    "Saved reconstruction state at frame {} ({} compressed bytes)",
                    tape.frames.len(),
                    bytes.len()
                ));
                true
            }
            Err(error) => {
                log(format!("Cannot save state: {error}"));
                false
            }
        }
    })
}
#[unsafe(no_mangle)]
/// # Safety
/// A non-null data pointer must remain readable for size bytes and must not alias core state.
pub unsafe extern "C" fn retro_unserialize(data: *const c_void, size: usize) -> bool {
    if data.is_null() || !(52..=savestate::CAPACITY).contains(&size) {
        return false;
    }
    with_state(|state| {
        let mut s = state.borrow_mut();
        let Some(path) = &s.path else {
            return false;
        };
        let movie_hash = match offline::local_path(path)
            .and_then(|p| std::fs::read(p).map_err(|e| e.to_string()))
        {
            Ok(bytes) => savestate::hash(&bytes),
            Err(error) => {
                log(error);
                return false;
            }
        };
        match savestate::Tape::decode(
            std::slice::from_raw_parts(data as *const u8, size),
            &movie_hash,
        ) {
            Ok(tape) => {
                let mut restore = savestate::Restore::new(tape);
                restore.resume_quality = s.option("ruffle_quality");
                s.restoring = Some(restore);
                true
            }
            Err(error) => {
                log(format!("Cannot load state: {error}"));
                false
            }
        }
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_reset() {}
#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_set(_: u32, _: bool, _: *const c_char) {}
#[unsafe(no_mangle)]
pub extern "C" fn retro_load_game_special(_: u32, _: *const GameInfo, _: usize) -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_data(_: u32) -> *mut c_void {
    ptr::null_mut()
}
#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_size(_: u32) -> usize {
    0
}
