//! Hidden SDL OpenGL context exercising the actual native renderer and shaders.
use super::*;
#[path = "state_tests.rs"]
mod state_tests;
use state_tests::savestate_roundtrip;
use std::ffi::{CString, c_char, c_void};
#[test]
fn linear_rgb_gradient_uses_linear_light() {
    assert_eq!(gradient_component(0, 255, 0.5, false), 128);
    assert_eq!(gradient_component(0, 255, 0.5, true), 188);
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
fn test_output() -> std::path::PathBuf {
    std::env::var_os("RUFFLE_TEST_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/libretro-tests")
        })
}
unsafe fn symbol<T: Copy>(library: *mut c_void, name: &str) -> T {
    let name = CString::new(name).unwrap();
    let address = unsafe { GetProcAddress(library, name.as_ptr()) };
    assert!(!address.is_null(), "SDL symbol missing");
    unsafe { std::mem::transmute_copy(&address) }
}
fn rect(w: f32, h: f32) -> Matrix {
    Matrix {
        a: w,
        d: h,
        ..Matrix::IDENTITY
    }
}
fn pixels(r: &GlRenderer) -> Vec<u8> {
    let handle = r.texture_for_test();
    let target = (&*handle.0 as &dyn std::any::Any)
        .downcast_ref::<Texture>()
        .unwrap();
    r.read_texture(target).unwrap()
}
impl GlRenderer {
    fn texture_for_test(&self) -> BitmapHandle {
        unsafe {
            let id = self.gl.create_texture().unwrap();
            self.gl.bind_texture(glow::TEXTURE_2D, Some(id));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                self.dimensions.width as i32,
                self.dimensions.height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            let t = Texture {
                pool: None,
                id,
                w: self.dimensions.width,
                h: self.dimensions.height,
                gl: self.gl.clone(),
                alive: self.alive.clone(),
                sampling: std::cell::Cell::new((glow::LINEAR, glow::CLAMP_TO_EDGE)),
            };
            self.copy_target_to(&t);
            BitmapHandle(Arc::new(t))
        }
    }
}
#[test]
#[ignore = "requires Windows, SDL2 and OpenGL; use libretro/build.ps1 -Task Test -GraphicsTests -SdlPath ..."]
fn native_opengl_contracts() {
    unsafe {
        let path = std::path::PathBuf::from(
            std::env::var_os("RUFFLE_TEST_SDL2")
                .expect("Set RUFFLE_TEST_SDL2 to the absolute SDL2.dll path"),
        );
        let wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let lib = LoadLibraryW(wide.as_ptr());
        assert!(!lib.is_null(), "SDL2 test runtime unavailable");
        let init: unsafe extern "C" fn(u32) -> i32 = symbol(lib, "SDL_Init");
        let attr: unsafe extern "C" fn(i32, i32) -> i32 = symbol(lib, "SDL_GL_SetAttribute");
        let window: unsafe extern "C" fn(*const c_char, i32, i32, i32, i32, u32) -> *mut c_void =
            symbol(lib, "SDL_CreateWindow");
        let context: unsafe extern "C" fn(*mut c_void) -> *mut c_void =
            symbol(lib, "SDL_GL_CreateContext");
        let proc: unsafe extern "C" fn(*const c_char) -> *const c_void =
            symbol(lib, "SDL_GL_GetProcAddress");
        let delete: unsafe extern "C" fn(*mut c_void) = symbol(lib, "SDL_GL_DeleteContext");
        let destroy: unsafe extern "C" fn(*mut c_void) = symbol(lib, "SDL_DestroyWindow");
        let quit: unsafe extern "C" fn() = symbol(lib, "SDL_Quit");
        assert_eq!(init(32), 0);
        attr(17, 3);
        attr(18, 0);
        attr(3, 8);
        attr(6, 24);
        attr(7, 8);
        let window = window(c"Ruffle renderer tests".as_ptr(), 0, 0, 64, 64, 2 | 8);
        assert!(!window.is_null());
        let context = context(window);
        assert!(!context.is_null());
        {
            let mut r =
                GlRenderer::new(|name| proc(CString::new(name).unwrap().as_ptr()), 8, 8).unwrap();
            r.set_quality(StageQuality::Low);
            let frontend = r.texture(8, 8, &vec![0; 8 * 8 * 4]);
            let front = (&*frontend.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .unwrap();
            let framebuffer = r.gl.create_framebuffer().unwrap();
            r.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            r.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(front.id),
                0,
            );
            r.framebuffer = Some(framebuffer);
            let leased = r.transient(4, 4);
            let other = r.transient(4, 4);
            assert_ne!(leased, other, "live render targets must never alias");
            let old = leased.clone();
            drop(leased);
            let third = r.transient(4, 4);
            assert_ne!(
                old, third,
                "a texture retained by a command must not be reused"
            );
            drop(old);
            drop(other);
            drop(third);
            let canvas = r.texture(2, 2, &[255, 0, 0, 255].repeat(4));
            let mut commands = CommandList::new();
            let mut m = Matrix::IDENTITY;
            m.tx = swf::Twips::from_pixels(1.);
            m.ty = swf::Twips::from_pixels(1.);
            commands.draw_rect(Color::BLUE, m);
            let read = r
                .render_offscreen(
                    canvas.clone(),
                    commands,
                    StageQuality::Low,
                    PixelRegion {
                        x_min: 1,
                        y_min: 1,
                        x_max: 2,
                        y_max: 2,
                    },
                )
                .unwrap();
            let read = (&*read as &dyn std::any::Any)
                .downcast_ref::<Readback>()
                .unwrap();
            assert_eq!(read.bytes, [0, 0, 255, 255]);
            assert_eq!(read.stride, 4);
            let texture = (&*canvas.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .unwrap();
            let data = r.read_texture(texture).unwrap();
            assert_eq!(&data[..12], &[255, 0, 0, 255].repeat(3));
            let mut commands = CommandList::new();
            commands.render_bitmap(
                canvas,
                Transform::default(),
                false,
                PixelSnapping::Never,
                PixelRegion {
                    x_min: 1,
                    y_min: 1,
                    x_max: 2,
                    y_max: 2,
                },
            );
            r.submit_frame(Color::TRANSPARENT, commands, vec![]);
            let data = pixels(&r);
            assert_eq!(&data[..4], &[0, 0, 255, 255]);
            assert_eq!(
                &data[4..8],
                &[0, 0, 0, 0],
                "atlas region must not draw neighboring texels"
            );
            for quality in [StageQuality::Low, StageQuality::High] {
                r.set_quality(quality);
                let mut commands = CommandList::new();
                let mut source = CommandList::new();
                source.draw_rect(
                    Color {
                        r: 255,
                        g: 0,
                        b: 0,
                        a: 128,
                    },
                    rect(8., 8.),
                );
                commands.blend(source, RenderBlendMode::Builtin(swf::BlendMode::Multiply));
                r.submit_frame(Color::TRANSPARENT, commands, vec![]);
                let data = pixels(&r);
                assert_eq!(
                    &data[..4],
                    &[128, 0, 0, 128],
                    "multiply on transparent destination, {quality:?}"
                );
                let mut commands = CommandList::new();
                let mut source = CommandList::new();
                let mut mask = CommandList::new();
                source.draw_rect(Color::RED, rect(8., 8.));
                mask.draw_rect(
                    Color {
                        r: 255,
                        g: 255,
                        b: 255,
                        a: 128,
                    },
                    rect(8., 8.),
                );
                commands.render_alpha_mask(source, mask);
                r.submit_frame(Color::TRANSPARENT, commands, vec![]);
                let data = pixels(&r);
                assert_eq!(
                    &data[..4],
                    &[128, 0, 0, 128],
                    "fractional alpha mask, {quality:?}"
                );
                assert_eq!(r.gl.get_error(), glow::NO_ERROR);
                for (mode, expected) in [
                    (swf::BlendMode::Multiply, [0, 0, 127, 255]),
                    (swf::BlendMode::Screen, [128, 0, 255, 255]),
                    (swf::BlendMode::Lighten, [128, 0, 255, 255]),
                    (swf::BlendMode::Darken, [0, 0, 127, 255]),
                    (swf::BlendMode::Difference, [128, 0, 255, 255]),
                    (swf::BlendMode::Add, [128, 0, 255, 255]),
                    (swf::BlendMode::Subtract, [0, 0, 255, 255]),
                    (swf::BlendMode::Invert, [128, 128, 127, 255]),
                    (swf::BlendMode::Overlay, [0, 0, 255, 255]),
                    (swf::BlendMode::HardLight, [128, 0, 127, 255]),
                    (swf::BlendMode::Alpha, [0, 0, 128, 128]),
                    (swf::BlendMode::Erase, [0, 0, 127, 127]),
                ] {
                    let mut commands = CommandList::new();
                    let mut source = CommandList::new();
                    source.draw_rect(
                        Color {
                            r: 255,
                            g: 0,
                            b: 0,
                            a: 128,
                        },
                        rect(8., 8.),
                    );
                    commands.blend(source, RenderBlendMode::Builtin(mode));
                    r.submit_frame(Color::BLUE, commands, vec![]);
                    let data = pixels(&r);
                    for c in 0..4 {
                        assert!(
                            (data[c] as i32 - expected[c]).abs() <= 1,
                            "{mode:?}, {quality:?}: got {:?}, expected {expected:?}",
                            &data[..4]
                        );
                    }
                    assert_eq!(r.gl.get_error(), glow::NO_ERROR);
                }
            }
            let mut commands = CommandList::new();
            let mut source = CommandList::new();
            source.draw_rect(Color::RED, rect(8., 8.));
            commands.blend(source, RenderBlendMode::Builtin(swf::BlendMode::Multiply));
            r.submit_frame(Color::BLUE, commands.clone(), vec![]);
            r.take_stats();
            for _ in 0..20 {
                r.submit_frame(Color::BLUE, commands.clone(), vec![]);
                assert_eq!(
                    r.take_stats().textures,
                    0,
                    "steady-state effects must reuse their textures"
                );
            }
            let vertices = [
                Vertex {
                    x: 0.,
                    y: 0.,
                    color: Color::RED,
                },
                Vertex {
                    x: 2.,
                    y: 0.,
                    color: Color::RED,
                },
                Vertex {
                    x: 0.,
                    y: 2.,
                    color: Color::RED,
                },
            ];
            let indices = [0, 1, 2];
            let geometry = r.geometry(&vertices, &indices, None);
            let mut transform = Transform::default();
            transform.matrix.a = 2.;
            transform.matrix.tx = swf::Twips::from_pixels(3.);
            transform.matrix.ty = swf::Twips::from_pixels(2.);
            r.begin(Color::TRANSPARENT);
            r.draw(
                &vertices, &indices, &transform, None, None, 0, false, false, None,
            );
            let expected = pixels(&r);
            r.begin(Color::TRANSPARENT);
            r.take_stats();
            r.draw_geometry(
                &geometry,
                indices.len(),
                &transform,
                None,
                0,
                false,
                false,
                None,
            );
            assert_eq!(
                r.take_stats().uploaded_bytes,
                0,
                "cached shapes must not reupload vertices"
            );
            assert_eq!(
                pixels(&r),
                expected,
                "cached and streamed geometry must render identically"
            );
            r.begin(Color::TRANSPARENT);
            r.draw_cursor(0., 0., 24.);
            let cursor = pixels(&r);
            assert!(
                cursor
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[3] > 0)
                    .count()
                    > 12,
                "cursor must remain visible beyond the old 3px marker"
            );
            assert!(cursor.as_chunks::<4>().0.contains(&[0, 0, 0, 255]));
            assert!(cursor.as_chunks::<4>().0.contains(&[255, 255, 255, 255]));
            pixelbender_contracts(&mut r);
            let empty = r
                .create_empty_texture(NonZeroU32::new(19).unwrap(), NonZeroU32::new(23).unwrap())
                .unwrap();
            let mut commands = CommandList::new();
            commands.draw_rect(Color::BLUE, rect(19., 23.));
            r.render_texture(&empty, commands, Color::TRANSPARENT, false);
            let old_id = (&*empty.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .unwrap()
                .id;
            drop(empty);
            r.take_stats();
            let recycled = r
                .create_empty_texture(NonZeroU32::new(19).unwrap(), NonZeroU32::new(23).unwrap())
                .unwrap();
            let texture = (&*recycled.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .unwrap();
            assert_eq!(texture.id, old_id);
            assert_eq!(r.take_stats().textures, 0);
            assert!(
                r.read_texture(texture).unwrap().iter().all(|x| *x == 0),
                "reused empty textures must not expose previous pixels"
            );
            let live = r
                .create_empty_texture(NonZeroU32::new(19).unwrap(), NonZeroU32::new(23).unwrap())
                .unwrap();
            assert_ne!(live, recycled, "live cache handles must not alias");
            drop(live);
            drop(recycled);
            // Animated caches change logical size almost every frame. Their
            // stencil and private scratch storage should remain reusable.
            for (i, (w, h)) in [(17u32, 21u32), (19, 23), (24, 31), (18, 22)]
                .into_iter()
                .enumerate()
            {
                let output = r.texture(w, h, &[]);
                let mut commands = CommandList::new();
                commands.draw_rect(Color::RED, rect(w as f32, h as f32));
                let mut corner = rect(1., 1.);
                corner.tx = swf::Twips::from_pixels((w - 1) as f64);
                corner.ty = swf::Twips::from_pixels((h - 1) as f64);
                commands.draw_rect(Color::BLUE, corner);
                r.take_stats();
                r.render_texture(&output, commands, Color::TRANSPARENT, false);
                let stats = r.take_stats();
                if i > 0 {
                    assert_eq!(
                        stats.target_allocations, 0,
                        "nearby sizes must reuse stencil storage"
                    );
                    assert_eq!(
                        stats.textures, 0,
                        "nearby sizes must reuse private scratch images"
                    );
                }
                let texture = (&*output.0 as &dyn std::any::Any)
                    .downcast_ref::<Texture>()
                    .unwrap();
                let data = r.read_texture(texture).unwrap();
                assert_eq!(&data[..4], &[255, 0, 0, 255]);
                assert_eq!(&data[data.len() - 4..], &[0, 0, 255, 255]);
                assert_eq!(r.gl.get_error(), glow::NO_ERROR);
            }
        }

        if let Ok(path) = std::env::var("RUFFLE_BENCH_GAME") {
            benchmark_game(&path, proc);
        }
        if let Ok(path) = std::env::var("RUFFLE_STATE_GAME") {
            savestate_roundtrip(&path, proc);
        }
        delete(context);
        destroy(window);
        quit();
        FreeLibrary(lib);
    }
}

fn pixelbender_contracts(r: &mut GlRenderer) {
    use ruffle_render::pixel_bender::*;
    let register = |index, channels: Vec<PixelBenderRegChannel>| PixelBenderReg {
        index,
        channels,
        kind: PixelBenderRegKind::Float,
    };
    let rgba = PixelBenderRegChannel::RGBA.to_vec();
    let shader = PixelBenderShader {
        name: "native constant parameters".into(),
        version: 1,
        metadata: vec![],
        params: vec![
            PixelBenderParam::Normal {
                qualifier: PixelBenderParamQualifier::Input,
                param_type: PixelBenderTypeOpcode::TFloat4,
                reg: register(0, rgba.clone()),
                name: "color".into(),
                metadata: vec![],
            },
            PixelBenderParam::Normal {
                qualifier: PixelBenderParamQualifier::Output,
                param_type: PixelBenderTypeOpcode::TFloat4,
                reg: register(1, rgba.clone()),
                name: "dst".into(),
                metadata: vec![],
            },
        ],
        operations: vec![Operation::Normal {
            opcode: Opcode::Mov,
            dst: register(1, rgba.clone()),
            src: register(0, rgba.clone()),
        }],
    };
    let shader = r.compile_pixelbender_shader(shader).unwrap();
    let args = [PixelBenderShaderArgument::ValueInput {
        index: 0,
        value: PixelBenderType::TFloat4(2.5, -1., 0.25, 1.),
    }];
    let out = r
        .run_pixelbender_shader(
            shader.clone(),
            &args,
            &PixelBenderTarget::Bytes {
                width: 2,
                height: 1,
            },
        )
        .unwrap();
    let PixelBenderOutput::Bytes(bytes) = out else {
        panic!("expected float bytes")
    };
    assert_eq!(
        bytemuck::cast_slice::<u8, f32>(&bytes),
        &[2.5, -1., 0.25, 1., 2.5, -1., 0.25, 1.]
    );
    let target = r.texture(2, 2, &[0; 16]);
    let out = r
        .run_pixelbender_shader(shader, &args, &PixelBenderTarget::Bitmap(target.clone()))
        .unwrap();
    let PixelBenderOutput::Bitmap(sync) = out else {
        panic!("expected bitmap")
    };
    r.resolve_sync_handle(
        sync,
        Box::new(|bytes, stride| {
            assert_eq!(stride, 8);
            assert_eq!(bytes, [255, 0, 64, 255].repeat(4));
        }),
    )
    .unwrap();
    let coords = register(0, PixelBenderRegChannel::RG.to_vec());
    let shader = PixelBenderShader {
        name: "native sampled coordinates".into(),
        version: 1,
        metadata: vec![],
        params: vec![
            PixelBenderParam::Texture {
                index: 0,
                channels: 4,
                name: "src".into(),
            },
            PixelBenderParam::Normal {
                qualifier: PixelBenderParamQualifier::Input,
                param_type: PixelBenderTypeOpcode::TFloat2,
                reg: coords.clone(),
                name: OUT_COORD_NAME.into(),
                metadata: vec![],
            },
            PixelBenderParam::Normal {
                qualifier: PixelBenderParamQualifier::Output,
                param_type: PixelBenderTypeOpcode::TFloat4,
                reg: register(1, rgba.clone()),
                name: "dst".into(),
                metadata: vec![],
            },
        ],
        operations: vec![Operation::SampleNearest {
            dst: register(1, rgba),
            src: coords,
            tf: 0,
        }],
    };
    let shader = r.compile_pixelbender_shader(shader).unwrap();
    let input = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let source = r.texture(2, 2, &input);
    let args = [PixelBenderShaderArgument::ImageInput {
        index: 0,
        channels: 4,
        name: "src".into(),
        texture: Some(source.clone().into()),
    }];
    r.run_pixelbender_shader(
        shader.clone(),
        &args,
        &PixelBenderTarget::Bitmap(target.clone()),
    )
    .unwrap();
    let t = (&*target.0 as &dyn std::any::Any)
        .downcast_ref::<Texture>()
        .unwrap();
    assert_eq!(
        r.read_texture(t).unwrap(),
        input,
        "PixelBender must preserve pixel-coordinate orientation"
    );
    #[derive(Debug)]
    struct Owner;
    impl ruffle_render::filters::ShaderObject for Owner {
        fn clone_box(&self) -> Box<dyn ruffle_render::filters::ShaderObject> {
            Box::new(Owner)
        }
        fn equals(&self, _: &dyn ruffle_render::filters::ShaderObject) -> bool {
            true
        }
    }
    let filter = ruffle_render::filters::ShaderFilter {
        bottom_extension: 0,
        left_extension: 0,
        right_extension: 0,
        top_extension: 0,
        shader_object: Box::new(Owner),
        shader: shader.clone(),
        shader_args: vec![PixelBenderShaderArgument::ImageInput {
            index: 0,
            channels: 4,
            name: "src".into(),
            texture: None,
        }],
    };
    let output = r.texture(2, 2, &[0; 16]);
    let sync = r
        .apply_filter(
            source,
            (1, 0),
            (1, 2),
            output.clone(),
            (0, 0),
            ruffle_render::filters::Filter::ShaderFilter(filter),
        )
        .unwrap();
    r.resolve_sync_handle(
        sync,
        Box::new(|bytes, _| {
            assert_eq!(
                bytes,
                [0, 255, 0, 255, 0, 0, 0, 0, 255, 255, 0, 255, 0, 0, 0, 0]
            )
        }),
    )
    .unwrap();
    unsafe {
        assert_eq!(r.gl.get_error(), glow::NO_ERROR);
    }
}

unsafe fn benchmark_game(path: &str, proc: unsafe extern "C" fn(*const c_char) -> *const c_void) {
    use ruffle_core::{
        FloatDuration, PlayerBuilder,
        backend::{audio::AudioMixer, navigator::NullExecutor},
        events::{MouseButton, PlayerEvent},
    };
    let movie = ruffle_core::tag_utils::movie_from_path(path, None).unwrap();
    let (w, h) = (
        movie.width().to_pixels() as u32,
        movie.height().to_pixels() as u32,
    );
    let mut renderer =
        unsafe { GlRenderer::new(|name| proc(CString::new(name).unwrap().as_ptr()), w, h) }
            .unwrap();
    let frontend = renderer.texture(w, h, &vec![0; w as usize * h as usize * 4]);
    let tex = (&*frontend.0 as &dyn std::any::Any)
        .downcast_ref::<Texture>()
        .unwrap();
    let fb = unsafe { renderer.gl.create_framebuffer() }.unwrap();
    unsafe {
        renderer.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
        renderer.gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex.id),
            0,
        );
    }
    renderer.framebuffer = Some(fb);
    let mut executor = NullExecutor::new();
    let navigator =
        crate::offline::OfflineNavigator::new(std::path::Path::new(path), &executor).unwrap();
    let mixer = AudioMixer::new(2, 48000);
    let audio = mixer.proxy();
    let player = PlayerBuilder::new()
        .with_movie(movie)
        .with_renderer(renderer)
        .with_navigator(navigator)
        .with_ui(crate::ui::CoreUi::new())
        .with_default_font(true)
        .with_audio(crate::Mixer { mixer })
        .with_viewport_dimensions(w, h, 1.)
        .with_autoplay(true)
        .build();
    let root = test_output()
        .join(std::env::var("RUFFLE_BENCH_TAG").unwrap_or_else(|_| "benchmark".into()));
    std::fs::create_dir_all(&root).unwrap();
    let actions: serde_json::Value = std::env::var("RUFFLE_BENCH_ACTIONS")
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!([{ "frame":360,"x":0.5,"y":0.53 }]));
    let frames = std::env::var("RUFFLE_BENCH_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1800u32);
    let mut csv = String::from(
        "frame,tick_ms,render_ms,draws,upload_kb,textures,groups,cpu_filter_ms,filter_pixels,readbacks\n",
    );
    let mut totals = [0.; 9];
    let mut samples = vec![0f32; 1600];
    let mut peak = 0f32;
    let mut over = 0u64;
    let mut keyboard = crate::keyboard::Keyboard::default();
    let mut audio_output = crate::audio::Output::default();
    let mut audio_log = String::new();
    for frame in 0..frames {
        executor.run();
        let mut p = player.lock().unwrap();
        let mut polled = [false; 324];
        if let Some(events) = actions.as_array() {
            for event in events {
                let at = event["frame"].as_u64().unwrap_or(0) as u32;
                if let Some(key) = event["polled_key"].as_u64() {
                    if key < 324
                        && frame >= at
                        && frame < at + event["duration"].as_u64().unwrap_or(1) as u32
                    {
                        polled[key as usize] = true;
                    }
                    continue;
                }
                if let Some(key) = event["key"]
                    .as_u64()
                    .and_then(|key| crate::key_descriptor(key as u32))
                {
                    if frame == at {
                        p.handle_event(PlayerEvent::KeyDown { key });
                    }
                    if frame == at + event["duration"].as_u64().unwrap_or(60) as u32 {
                        p.handle_event(PlayerEvent::KeyUp { key });
                    }
                    continue;
                }
                let x = event["x"].as_f64().unwrap_or(0.5) * w as f64;
                let y = event["y"].as_f64().unwrap_or(0.5) * h as f64;
                if frame + 3 == at {
                    p.handle_event(PlayerEvent::MouseMove { x, y });
                }
                if frame == at {
                    p.handle_event(PlayerEvent::MouseMove { x, y });
                    p.handle_event(PlayerEvent::MouseDown {
                        x,
                        y,
                        button: MouseButton::Left,
                        index: None,
                    });
                }
                if frame == at + 1 {
                    p.handle_event(PlayerEvent::MouseUp {
                        x,
                        y,
                        button: MouseButton::Left,
                    });
                }
            }
        }
        for event in keyboard.update(&polled, &[false; 324], &[]) {
            p.handle_event(event);
        }
        let started = std::time::Instant::now();
        p.tick(FloatDuration::from_millis(1000. / 60.));
        let tick = started.elapsed().as_secs_f64() * 1000.;
        let started = std::time::Instant::now();
        p.render();
        let render = started.elapsed().as_secs_f64() * 1000.;
        let r = (p.renderer_mut() as &mut dyn std::any::Any)
            .downcast_mut::<GlRenderer>()
            .unwrap();
        let stats = r.stats.replace(FrameStats::default());
        for (sum, value) in totals.iter_mut().zip([
            tick,
            render,
            stats.draws as f64,
            stats.uploaded_bytes as f64 / 1024.,
            stats.textures as f64,
            stats.groups as f64,
            stats.cpu_filter_us as f64 / 1000.,
            stats.filter_pixels as f64,
            stats.readbacks as f64,
        ]) {
            *sum += value;
        }
        let audio_started = std::time::Instant::now();
        audio.mix(&mut samples);
        for sample in &samples {
            peak = peak.max(sample.abs());
            over += u64::from(sample.abs() > 1.);
        }
        let converted = audio_output.convert(&samples, 1.);
        if let Some(message) = audio_output.report(audio_started.elapsed().as_secs_f64() * 1000.) {
            audio_log.push_str(&format!("{frame}: {message}\n"));
        }
        std::hint::black_box(converted);
        if frame % 300 == 299 {
            let row = format!(
                "{frame},{}\n",
                totals
                    .iter()
                    .map(|v| format!("{:.3}", v / 300.))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            print!("{row}");
            csv.push_str(&row);
            totals.fill(0.);
            let image = pixels(r);
            let file = std::fs::File::create(root.join(format!("frame-{frame}.png"))).unwrap();
            let mut encoder = png::Encoder::new(file, w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&image)
                .unwrap();
            std::fs::write(root.join("timings.csv"), &csv).unwrap();
        }
    }
    std::fs::write(root.join("audio.txt"),format!("mixed_float_peak={peak}\nsamples_exceeding_pcm16_range_before_final_saturation={over}\n")).unwrap();
    std::fs::write(root.join("audio-limiter.txt"), audio_log).unwrap();
}
