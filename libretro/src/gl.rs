//! Native OpenGL 3.0 renderer. The frontend owns the context and framebuffer.
// BitmapHandle requires Arc ownership. All GL access, including context
// callbacks, is serialized by State; these handles are never used concurrently.
#![allow(clippy::arc_with_non_send_sync)]
use glow::HasContext;
use ruffle_render::bitmap::Bitmap;
use ruffle_render::{
    backend::*,
    bitmap::*,
    commands::*,
    error::Error,
    matrix::Matrix,
    pixel_bender::*,
    pixel_bender_support::PixelBenderShaderArgument,
    quality::StageQuality,
    shape_utils::{DistilledShape, GradientType},
    tessellator::*,
    transform::Transform,
};
use std::{
    borrow::Cow,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use swf::Color;
fn gradient_component(a: u8, b: u8, t: f32, linear: bool) -> u8 {
    if !linear {
        return (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    }
    let decode = |v: u8| {
        let v = v as f32 / 255.;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let v = decode(a) + (decode(b) - decode(a)) * t;
    ((if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }) * 255.)
        .round()
        .clamp(0., 255.) as u8
}

#[derive(Debug)]
struct Texture {
    id: glow::Texture,
    w: u32,
    h: u32,
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
    sampling: std::cell::Cell<(u32, u32)>,
    pool: Option<Arc<std::sync::Mutex<image_pool::ImagePool>>>,
}
impl Drop for Texture {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed) {
            if let Some(pool) = &self.pool {
                pool.lock().unwrap().retire(image_pool::Entry {
                    id: self.id,
                    w: self.w,
                    h: self.h,
                    sampling: self.sampling.get(),
                });
                return;
            }
            unsafe {
                self.gl.delete_texture(self.id);
            }
        }
    }
}
impl BitmapHandleImpl for Texture {}
struct Shape {
    mesh: Mesh,
    textures: Vec<Option<BitmapHandle>>,
    geometry: Vec<Geometry>,
}
struct Geometry {
    vao: glow::VertexArray,
    vb: glow::Buffer,
    ib: glow::Buffer,
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
}
impl Drop for Geometry {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed) {
            unsafe {
                self.gl.delete_vertex_array(self.vao);
                self.gl.delete_buffer(self.vb);
                self.gl.delete_buffer(self.ib);
            }
        }
    }
}
impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GlShape")
    }
}
impl ShapeHandleImpl for Shape {}
#[derive(Debug)]
struct Readback {
    bytes: Vec<u8>,
    stride: u32,
}
impl SyncHandle for Readback {}
#[derive(Debug)]
struct TextureReadback {
    handle: BitmapHandle,
}
impl SyncHandle for TextureReadback {}
impl RawTexture for Texture {
    fn equals(&self, other: &dyn RawTexture) -> bool {
        (other as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .is_some_and(|t| t.id == self.id && Arc::ptr_eq(&t.gl, &self.gl))
    }
}
#[path = "context3d_gl.rs"]
mod context3d_gl;
#[path = "image_pool.rs"]
mod image_pool;
#[path = "pixelbender_gl.rs"]
mod pixelbender_gl;

struct AaTarget {
    fb: glow::Framebuffer,
    color: glow::Renderbuffer,
    stencil: glow::Renderbuffer,
    w: u32,
    h: u32,
    samples: i32,
}
struct OffscreenTarget {
    fb: glow::Framebuffer,
    stencil: glow::Renderbuffer,
    w: u32,
    h: u32,
    busy: bool,
}

pub struct GlRenderer {
    stats: std::cell::Cell<FrameStats>,
    gl: Arc<glow::Context>,
    program: glow::Program,
    uniforms: std::collections::HashMap<&'static str, Option<glow::UniformLocation>>,
    transients: Vec<BitmapHandle>,
    image_pool: Arc<std::sync::Mutex<image_pool::ImagePool>>,
    targets: Vec<OffscreenTarget>,
    copy_fb: glow::Framebuffer,
    vao: glow::VertexArray,
    vb: glow::Buffer,
    ib: glow::Buffer,
    alive: Arc<AtomicBool>,
    dimensions: ViewportDimensions,
    framebuffer: Option<glow::Framebuffer>,
    mask: u32,
    writing: i32,
    aa: Option<AaTarget>,
    samples: i32,
    max_samples: i32,
    blend_mode: swf::BlendMode,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct FrameStats {
    pub draws: u64,
    pub uploaded_bytes: u64,
    pub textures: u64,
    pub groups: u64,
    pub cpu_filter_us: u64,
    pub filter_pixels: u64,
    pub readbacks: u64,
    pub group_pixels: u64,
    pub target_allocations: u64,
    pub target_us: u64,
    pub texture_us: u64,
    pub tessellation_us: u64,
    pub samples: u32,
}
impl GlRenderer {
    pub unsafe fn new(
        load: impl FnMut(&str) -> *const std::ffi::c_void,
        w: u32,
        h: u32,
    ) -> Result<Self, String> {
        unsafe {
            let gl = Arc::new(glow::Context::from_loader_function(load));
            let program = gl.create_program()?;
            for (kind, source) in [
                (glow::VERTEX_SHADER, include_str!("vertex.glsl")),
                (glow::FRAGMENT_SHADER, include_str!("fragment.glsl")),
            ] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.bind_attrib_location(program, 0, "position");
            gl.bind_attrib_location(program, 1, "color");
            gl.bind_attrib_location(program, 2, "uv");
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            let vao = gl.create_vertex_array()?;
            let vb = gl.create_buffer()?;
            let ib = gl.create_buffer()?;
            let copy_fb = gl.create_framebuffer()?;
            let uniforms = [
                "image",
                "mode",
                "maskWrite",
                "spread",
                "focal",
                "multiply",
                "addition",
                "destination",
                "blendMode",
                "world",
                "viewport",
            ]
            .into_iter()
            .map(|name| (name, gl.get_uniform_location(program, name)))
            .collect();
            let max_samples = gl.get_parameter_i32(glow::MAX_SAMPLES).max(1);
            let alive = Arc::new(AtomicBool::new(true));
            let image_pool = Arc::new(std::sync::Mutex::new(image_pool::ImagePool::new(
                gl.clone(),
                alive.clone(),
            )));
            Ok(Self {
                stats: std::cell::Cell::new(FrameStats::default()),
                gl,
                program,
                uniforms,
                transients: vec![],
                image_pool,
                targets: vec![],
                copy_fb,
                vao,
                vb,
                ib,
                alive,
                dimensions: ViewportDimensions {
                    width: w,
                    height: h,
                    scale_factor: 1.0,
                },
                framebuffer: None,
                mask: 0,
                writing: 0,
                aa: None,
                samples: 4,
                max_samples,
                blend_mode: swf::BlendMode::Normal,
            })
        }
    }
    pub fn abandon(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
    pub fn take_stats(&self) -> FrameStats {
        let mut stats = self.stats.replace(FrameStats::default());
        stats.samples = self.samples as u32;
        stats
    }
    pub fn draw_cursor(&mut self, x: f64, y: f64, size: f32) {
        let black = [
            (0., 0.),
            (0., 20.),
            (5., 15.),
            (17., 14.),
            (4., 11.),
            (9., 23.),
            (14., 21.),
            (8., 10.),
        ];
        let white = [
            (2., 4.),
            (2., 15.),
            (5., 12.),
            (12., 12.),
            (6., 12.),
            (10., 20.),
            (11., 19.),
            (7., 11.),
        ];
        let vertices: Vec<_> = black
            .into_iter()
            .map(|(x, y)| Vertex {
                x,
                y,
                color: Color::BLACK,
            })
            .chain(white.into_iter().map(|(x, y)| Vertex {
                x,
                y,
                color: Color::WHITE,
            }))
            .collect();
        let mut matrix = Matrix::scale(size / 24., size / 24.);
        matrix.tx = swf::Twips::from_pixels(x);
        matrix.ty = swf::Twips::from_pixels(y);
        self.draw(
            &vertices,
            &[
                0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15,
            ],
            &Transform {
                matrix,
                ..Default::default()
            },
            None,
            None,
            0,
            false,
            false,
            None,
        );
        unsafe {
            self.gl.bind_vertex_array(None);
            self.gl.use_program(None);
        }
    }
    pub fn target(&mut self, framebuffer: usize) {
        self.framebuffer = NonZeroU32::new(framebuffer as u32).map(glow::NativeFramebuffer);
    }
    fn drop_aa(&mut self) {
        if let Some(a) = self.aa.take() {
            unsafe {
                self.gl.delete_framebuffer(a.fb);
                self.gl.delete_renderbuffer(a.color);
                self.gl.delete_renderbuffer(a.stencil);
            }
        }
    }
    fn prepare_aa(&mut self) -> Option<glow::Framebuffer> {
        unsafe {
            let (w, h) = (self.dimensions.width, self.dimensions.height);
            let samples = self.samples.min(self.max_samples);
            if samples < 2 {
                return None;
            }
            if self
                .aa
                .as_ref()
                .is_some_and(|a| a.w != w || a.h != h || a.samples != samples)
            {
                self.drop_aa();
            }
            if self.aa.is_none() {
                let fb = self.gl.create_framebuffer().ok()?;
                let color = self.gl.create_renderbuffer().ok()?;
                let stencil = self.gl.create_renderbuffer().ok()?;
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
                self.gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
                self.gl.renderbuffer_storage_multisample(
                    glow::RENDERBUFFER,
                    samples,
                    glow::RGBA8,
                    w as i32,
                    h as i32,
                );
                self.gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::RENDERBUFFER,
                    Some(color),
                );
                self.gl.bind_renderbuffer(glow::RENDERBUFFER, Some(stencil));
                self.gl.renderbuffer_storage_multisample(
                    glow::RENDERBUFFER,
                    samples,
                    glow::DEPTH24_STENCIL8,
                    w as i32,
                    h as i32,
                );
                self.gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(stencil),
                );
                self.aa = Some(AaTarget {
                    fb,
                    color,
                    stencil,
                    w,
                    h,
                    samples,
                });
            }
            self.aa.as_ref().map(|a| a.fb)
        }
    }
    fn texture(&mut self, w: u32, h: u32, bytes: &[u8]) -> BitmapHandle {
        self.texture_owner(w, h, bytes, None)
    }
    fn texture_owner(
        &mut self,
        w: u32,
        h: u32,
        bytes: &[u8],
        pool: Option<Arc<std::sync::Mutex<image_pool::ImagePool>>>,
    ) -> BitmapHandle {
        let started = std::time::Instant::now();
        let mut stats = self.stats.get();
        stats.textures += 1;
        self.stats.set(stats);
        unsafe {
            let gl = &self.gl;
            let id = gl.create_texture().expect("GL texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(id));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(if bytes.is_empty() { None } else { Some(bytes) }),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            let handle = BitmapHandle(Arc::new(Texture {
                id,
                w,
                h,
                gl: self.gl.clone(),
                alive: self.alive.clone(),
                sampling: std::cell::Cell::new((glow::LINEAR, glow::CLAMP_TO_EDGE)),
                pool,
            }));
            let mut stats = self.stats.get();
            stats.texture_us += started.elapsed().as_micros() as u64;
            self.stats.set(stats);
            handle
        }
    }
    fn transient(&mut self, w: u32, h: u32) -> BitmapHandle {
        if let Some(handle) = self.transients.iter().find(|hnd| {
            Arc::strong_count(&hnd.0) == 1
                && (&*hnd.0 as &dyn std::any::Any)
                    .downcast_ref::<Texture>()
                    .is_some_and(|t| t.w == w && t.h == h)
        }) {
            return handle.clone();
        }
        let handle = self.texture(w, h, &[]);
        let mut bytes: u64 = self
            .transients
            .iter()
            .filter_map(|h| (&*h.0 as &dyn std::any::Any).downcast_ref::<Texture>())
            .map(|t| t.w as u64 * t.h as u64 * 4)
            .sum();
        let requested = w as u64 * h as u64 * 4;
        if requested <= 64 * 1024 * 1024 {
            while self.transients.len() >= 64 || bytes + requested > 64 * 1024 * 1024 {
                let Some(index) = self
                    .transients
                    .iter()
                    .position(|h| Arc::strong_count(&h.0) == 1)
                else {
                    break;
                };
                let old = self.transients.remove(index);
                let texture = (&*old.0 as &dyn std::any::Any)
                    .downcast_ref::<Texture>()
                    .unwrap();
                bytes -= texture.w as u64 * texture.h as u64 * 4;
            }
            if self.transients.len() < 64 && bytes + requested <= 64 * 1024 * 1024 {
                self.transients.push(handle.clone());
            }
        }
        handle
    }
    fn acquire_target(&mut self, w: u32, h: u32) -> (usize, glow::Framebuffer) {
        if let Some(index) = self
            .targets
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.busy && t.w >= w && t.h >= h)
            .min_by_key(|(_, t)| t.w as u64 * t.h as u64)
            .map(|(i, _)| i)
        {
            self.targets[index].busy = true;
            return (index, self.targets[index].fb);
        }
        let replacement = if self.targets.len() >= 32 {
            self.targets.iter().position(|t| !t.busy)
        } else {
            None
        };
        let started = std::time::Instant::now();
        unsafe {
            let g = &self.gl;
            if let Some(index) = replacement {
                let t = &self.targets[index];
                g.delete_framebuffer(t.fb);
                g.delete_renderbuffer(t.stencil);
            }
            let fb = g.create_framebuffer().expect("GL offscreen framebuffer");
            let stencil = g.create_renderbuffer().expect("GL stencil");
            let maximum = g.get_parameter_i32(glow::MAX_RENDERBUFFER_SIZE) as u32;
            let capacity_w = w.max(16).next_power_of_two().min(maximum).max(w);
            let capacity_h = h.max(16).next_power_of_two().min(maximum).max(h);
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            g.bind_renderbuffer(glow::RENDERBUFFER, Some(stencil));
            g.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::STENCIL_INDEX8,
                capacity_w as i32,
                capacity_h as i32,
            );
            g.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::STENCIL_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(stencil),
            );
            let target = OffscreenTarget {
                fb,
                stencil,
                w: capacity_w,
                h: capacity_h,
                busy: true,
            };
            let index = if let Some(index) = replacement {
                self.targets[index] = target;
                index
            } else {
                self.targets.push(target);
                self.targets.len() - 1
            };
            let mut stats = self.stats.get();
            stats.target_allocations += 1;
            stats.target_us += started.elapsed().as_micros() as u64;
            self.stats.set(stats);
            (index, fb)
        }
    }
    fn filter_pixels(
        &self,
        bytes: &mut Vec<u8>,
        w: usize,
        h: usize,
        f: &ruffle_render::filters::Filter,
    ) {
        let started = std::time::Instant::now();
        let map = if let ruffle_render::filters::Filter::DisplacementMapFilter(f) = f {
            f.map_bitmap
                .as_ref()
                .and_then(|h| (&*h.0 as &dyn std::any::Any).downcast_ref::<Texture>())
                .and_then(|t| {
                    self.read_texture(t)
                        .map(|b| (b, t.w as usize, t.h as usize))
                })
        } else {
            None
        };
        crate::filters::apply(
            bytes,
            w,
            h,
            f,
            map.as_ref().map(|(b, w, h)| (b.as_slice(), *w, *h)),
        );
        let mut stats = self.stats.get();
        stats.filter_pixels += (w * h) as u64;
        stats.cpu_filter_us += started.elapsed().as_micros() as u64;
        self.stats.set(stats);
    }
    fn upload(&self, t: &Texture, bytes: &[u8]) {
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(t.id));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                t.w as i32,
                t.h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
        }
    }
    fn read_texture(&self, t: &Texture) -> Option<Vec<u8>> {
        let mut stats = self.stats.get();
        stats.readbacks += 1;
        self.stats.set(stats);
        unsafe {
            let fb = self.copy_fb;
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(t.id),
                0,
            );
            let mut bytes = vec![0; t.w as usize * t.h as usize * 4];
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            self.gl.read_pixels(
                0,
                0,
                t.w as i32,
                t.h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut bytes)),
            );
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);

            Some(bytes)
        }
    }
    fn begin(&mut self, clear: Color) {
        unsafe {
            let g = &self.gl;
            g.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            g.viewport(
                0,
                0,
                self.dimensions.width as i32,
                self.dimensions.height as i32,
            );
            g.disable(glow::SCISSOR_TEST);
            g.disable(glow::DEPTH_TEST);
            g.disable(glow::CULL_FACE);
            g.disable(glow::STENCIL_TEST);
            g.color_mask(true, true, true, true);
            g.stencil_mask(255);
            g.clear_stencil(0);
            g.clear_color(
                clear.r as f32 / 255.,
                clear.g as f32 / 255.,
                clear.b as f32 / 255.,
                clear.a as f32 / 255.,
            );
            g.clear(glow::COLOR_BUFFER_BIT | glow::STENCIL_BUFFER_BIT);
            g.enable(glow::BLEND);
            g.blend_equation(glow::FUNC_ADD);
            g.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            g.use_program(Some(self.program));
            g.bind_vertex_array(Some(self.vao));
            g.bind_buffer(glow::ARRAY_BUFFER, Some(self.vb));
            g.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ib));
            for (i, size, offset) in [(0, 2, 0), (1, 4, 8), (2, 2, 24)] {
                g.enable_vertex_attrib_array(i);
                g.vertex_attrib_pointer_f32(i, size, glow::FLOAT, false, 32, offset);
            }
            g.active_texture(glow::TEXTURE0);
            g.uniform_1_i32(self.uniform("image"), 0);
            self.mask = 0;
            self.writing = 0;
        }
    }
    fn uniform(&self, name: &'static str) -> Option<&glow::UniformLocation> {
        self.uniforms.get(name).and_then(Option::as_ref)
    }
    fn vertex_data(vertices: &[Vertex], uv: Option<[[f32; 3]; 3]>) -> Vec<f32> {
        let mut data = Vec::with_capacity(vertices.len() * 8);
        for v in vertices {
            let t = uv
                .map(|m| {
                    [
                        m[0][0] * v.x + m[1][0] * v.y + m[2][0],
                        m[0][1] * v.x + m[1][1] * v.y + m[2][1],
                    ]
                })
                .unwrap_or([0., 0.]);
            data.extend_from_slice(&[
                v.x,
                v.y,
                v.color.r as f32 / 255.,
                v.color.g as f32 / 255.,
                v.color.b as f32 / 255.,
                v.color.a as f32 / 255.,
                t[0],
                t[1],
            ]);
        }
        data
    }
    fn geometry(
        &self,
        vertices: &[Vertex],
        indices: &[u32],
        uv: Option<[[f32; 3]; 3]>,
    ) -> Geometry {
        unsafe {
            let g = &self.gl;
            let vao = g.create_vertex_array().expect("shape VAO");
            let vb = g.create_buffer().expect("shape vertices");
            let ib = g.create_buffer().expect("shape indices");
            g.bind_vertex_array(Some(vao));
            g.bind_buffer(glow::ARRAY_BUFFER, Some(vb));
            g.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ib));
            let data = Self::vertex_data(vertices, uv);
            g.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&data),
                glow::STATIC_DRAW,
            );
            g.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                bytemuck::cast_slice(indices),
                glow::STATIC_DRAW,
            );
            for (i, size, offset) in [(0, 2, 0), (1, 4, 8), (2, 2, 24)] {
                g.enable_vertex_attrib_array(i);
                g.vertex_attrib_pointer_f32(i, size, glow::FLOAT, false, 32, offset);
            }
            let mut stats = self.stats.get();
            stats.uploaded_bytes += (data.len() * 4 + indices.len() * 4) as u64;
            self.stats.set(stats);
            Geometry {
                vao,
                vb,
                ib,
                gl: self.gl.clone(),
                alive: self.alive.clone(),
            }
        }
    }
    fn prepare_draw(
        &self,
        transform: &Transform,
        texture: Option<&BitmapHandle>,
        mode: i32,
        smooth: bool,
        repeat: bool,
        gradient: Option<&Gradient>,
    ) {
        unsafe {
            let g = &self.gl;
            let m = &transform.matrix;
            let c = &transform.color_transform;
            g.use_program(Some(self.program));
            g.uniform_matrix_3_f32_slice(
                self.uniform("world"),
                false,
                &[
                    m.a,
                    m.b,
                    0.,
                    m.c,
                    m.d,
                    0.,
                    m.tx.to_pixels() as f32,
                    m.ty.to_pixels() as f32,
                    1.,
                ],
            );
            g.uniform_2_f32(
                self.uniform("viewport"),
                self.dimensions.width as f32,
                self.dimensions.height as f32,
            );
            g.uniform_1_i32(self.uniform("mode"), mode);
            g.uniform_1_i32(self.uniform("maskWrite"), i32::from(self.writing != 0));
            if (2..=4).contains(&mode) {
                g.uniform_1_i32(
                    self.uniform("spread"),
                    gradient
                        .map(|g| match g.repeat_mode {
                            swf::GradientSpread::Pad => 0,
                            swf::GradientSpread::Repeat => 1,
                            swf::GradientSpread::Reflect => 2,
                        })
                        .unwrap_or(0),
                );
                g.uniform_1_f32(
                    self.uniform("focal"),
                    gradient
                        .map(|g| g.focal_point.to_f32().clamp(-0.999, 0.999))
                        .unwrap_or(0.),
                );
            }
            g.uniform_4_f32(
                self.uniform("multiply"),
                c.r_multiply.to_f32(),
                c.g_multiply.to_f32(),
                c.b_multiply.to_f32(),
                c.a_multiply.to_f32(),
            );
            g.uniform_4_f32(
                self.uniform("addition"),
                c.r_add as f32 / 255.,
                c.g_add as f32 / 255.,
                c.b_add as f32 / 255.,
                c.a_add as f32 / 255.,
            );
            if let Some(t) =
                texture.and_then(|t| (&*t.0 as &dyn std::any::Any).downcast_ref::<Texture>())
            {
                g.bind_texture(glow::TEXTURE_2D, Some(t.id));
                let filter = if smooth { glow::LINEAR } else { glow::NEAREST };
                let wrap = if repeat {
                    glow::REPEAT
                } else {
                    glow::CLAMP_TO_EDGE
                };
                if t.sampling.get() != (filter, wrap) {
                    g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter as i32);
                    g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter as i32);
                    g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, wrap as i32);
                    g.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, wrap as i32);
                    t.sampling.set((filter, wrap));
                }
            }
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit geometry/material parameters match the render commands"
    )]
    fn draw(
        &mut self,
        vertices: &[Vertex],
        indices: &[u32],
        transform: &Transform,
        uv: Option<[[f32; 3]; 3]>,
        texture: Option<&BitmapHandle>,
        mode: i32,
        smooth: bool,
        repeat: bool,
        gradient: Option<&Gradient>,
    ) {
        let data = Self::vertex_data(vertices, uv);
        let mut stats = self.stats.get();
        stats.draws += 1;
        stats.uploaded_bytes += (data.len() * 4 + indices.len() * 4) as u64;
        self.stats.set(stats);
        unsafe {
            let g = &self.gl;
            g.bind_vertex_array(Some(self.vao));
            g.bind_buffer(glow::ARRAY_BUFFER, Some(self.vb));
            g.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ib));
            g.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&data),
                glow::STREAM_DRAW,
            );
            g.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                bytemuck::cast_slice(indices),
                glow::STREAM_DRAW,
            );
        }
        self.prepare_draw(transform, texture, mode, smooth, repeat, gradient);
        unsafe {
            self.gl
                .draw_elements(glow::TRIANGLES, indices.len() as i32, glow::UNSIGNED_INT, 0);
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit geometry/material parameters match the render commands"
    )]
    fn draw_geometry(
        &mut self,
        geometry: &Geometry,
        count: usize,
        transform: &Transform,
        texture: Option<&BitmapHandle>,
        mode: i32,
        smooth: bool,
        repeat: bool,
        gradient: Option<&Gradient>,
    ) {
        let mut stats = self.stats.get();
        stats.draws += 1;
        self.stats.set(stats);
        unsafe {
            self.gl.bind_vertex_array(Some(geometry.vao));
        }
        self.prepare_draw(transform, texture, mode, smooth, repeat, gradient);
        unsafe {
            self.gl
                .draw_elements(glow::TRIANGLES, count as i32, glow::UNSIGNED_INT, 0);
        }
    }
    fn restore_target(&self) {
        unsafe {
            let g = &self.gl;
            g.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            g.viewport(
                0,
                0,
                self.dimensions.width as i32,
                self.dimensions.height as i32,
            );
            g.enable(glow::BLEND);
            self.apply_blend();
            g.active_texture(glow::TEXTURE0);
            if self.mask == 0 {
                g.disable(glow::STENCIL_TEST);
            } else {
                g.enable(glow::STENCIL_TEST);
            }
            g.color_mask(
                self.writing == 0,
                self.writing == 0,
                self.writing == 0,
                self.writing == 0,
            );
            g.stencil_mask(if self.writing == 0 { 0 } else { 255 });
            g.stencil_func(
                glow::EQUAL,
                if self.writing > 0 {
                    self.mask.saturating_sub(1)
                } else {
                    self.mask
                } as i32,
                255,
            );
            g.stencil_op(
                glow::KEEP,
                glow::KEEP,
                if self.writing > 0 {
                    glow::INCR
                } else if self.writing < 0 {
                    glow::DECR
                } else {
                    glow::KEEP
                },
            );
        }
    }
    fn copy_target_to(&self, texture: &Texture) {
        unsafe {
            let g = &self.gl;
            let fb = self.copy_fb;
            g.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fb));
            g.framebuffer_texture_2d(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture.id),
                0,
            );
            g.bind_framebuffer(glow::READ_FRAMEBUFFER, self.framebuffer);
            g.blit_framebuffer(
                0,
                0,
                texture.w as i32,
                texture.h as i32,
                0,
                texture.h as i32,
                texture.w as i32,
                0,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            g.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
        }
    }
    fn render_texture(
        &mut self,
        handle: &BitmapHandle,
        commands: CommandList,
        clear: Color,
        preserve: bool,
    ) {
        let mut stats = self.stats.get();
        stats.groups += 1;
        self.stats.set(stats);
        let texture = (&*handle.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        let (w, h) = (texture.w, texture.h);
        stats.group_pixels += w as u64 * h as u64;
        self.stats.set(stats);
        let previous = (
            self.framebuffer,
            self.dimensions,
            self.mask,
            self.writing,
            self.blend_mode,
        );
        // Scratch images are never exposed as bitmaps. Bucket their storage
        // size while preserving the exact logical viewport and copy rectangle.
        let temporary =
            self.transient(w.max(16).next_power_of_two(), h.max(16).next_power_of_two());
        let temporary = (&*temporary.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        let (target_index, fb) = self.acquire_target(w, h);
        unsafe {
            let g = &self.gl;
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            g.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(temporary.id),
                0,
            );
            self.framebuffer = Some(fb);
            self.dimensions.width = w;
            self.dimensions.height = h;

            self.blend_mode = swf::BlendMode::Normal;
            self.begin(clear);
            if preserve {
                self.render_bitmap(
                    handle.clone(),
                    Transform::default(),
                    false,
                    PixelSnapping::Never,
                    PixelRegion::for_whole_size(w, h),
                );
            }
            commands.execute(self);
            self.copy_target_to(texture);

            self.targets[target_index].busy = false;
            (
                self.framebuffer,
                self.dimensions,
                self.mask,
                self.writing,
                self.blend_mode,
            ) = previous;
            self.restore_target();
        }
    }
    fn offscreen(
        &mut self,
        handle: BitmapHandle,
        commands: CommandList,
        clear: Color,
        preserve: bool,
    ) -> Option<Box<dyn SyncHandle>> {
        self.render_texture(&handle, commands, clear, preserve);
        let t = (&*handle.0 as &dyn std::any::Any).downcast_ref::<Texture>()?;
        Some(Box::new(Readback {
            bytes: self.read_texture(t)?,
            stride: t.w * 4,
        }))
    }
    fn group(&mut self, commands: CommandList) -> BitmapHandle {
        let (w, h) = (self.dimensions.width, self.dimensions.height);
        let handle = self.transient(w, h);
        self.render_texture(&handle, commands, Color::TRANSPARENT, false);
        handle
    }
    fn composite(&mut self, source: &BitmapHandle, dest: &BitmapHandle, mode: i32, replace: bool) {
        let t = (&*dest.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        unsafe {
            let g = &self.gl;
            g.use_program(Some(self.program));
            g.active_texture(glow::TEXTURE1);
            g.bind_texture(glow::TEXTURE_2D, Some(t.id));
            g.uniform_1_i32(self.uniform("destination"), 1);
            g.uniform_1_i32(self.uniform("blendMode"), mode);
            g.active_texture(glow::TEXTURE0);
            if replace {
                g.disable(glow::BLEND);
            }
        }
        let (w, h) = (self.dimensions.width as f32, self.dimensions.height as f32);
        let v = [
            Vertex {
                x: 0.,
                y: 0.,
                color: Color::WHITE,
            },
            Vertex {
                x: w,
                y: 0.,
                color: Color::WHITE,
            },
            Vertex {
                x: w,
                y: h,
                color: Color::WHITE,
            },
            Vertex {
                x: 0.,
                y: h,
                color: Color::WHITE,
            },
        ];
        self.draw(
            &v,
            &[0, 1, 2, 0, 2, 3],
            &Transform::default(),
            Some([[1. / w, 0., 0.], [0., 1. / h, 0.], [0., 0., 1.]]),
            Some(source),
            5,
            false,
            false,
            None,
        );
        if replace {
            unsafe {
                self.gl.enable(glow::BLEND);
            }
        }
    }
}
impl RenderBackend for GlRenderer {
    fn viewport_dimensions(&self) -> ViewportDimensions {
        self.dimensions
    }
    fn set_viewport_dimensions(&mut self, d: ViewportDimensions) {
        self.dimensions = d;
    }
    fn register_shape(&mut self, s: DistilledShape, source: &dyn BitmapSource) -> ShapeHandle {
        let started = std::time::Instant::now();
        let mesh = ShapeTessellator::new().tessellate_shape(s, source);
        let mut textures = vec![];
        for d in &mesh.draws {
            textures.push(match &d.draw_type {
                DrawType::Bitmap(b) => source.bitmap_handle(b.bitmap_id, self),
                DrawType::Gradient { gradient, .. } => {
                    let grad = &mesh.gradients[*gradient];
                    let mut bytes = vec![];
                    let fallback = vec![swf::GradientRecord {
                        ratio: 0,
                        color: Color::TRANSPARENT,
                    }];
                    let records = if grad.records.is_empty() {
                        &fallback
                    } else {
                        &grad.records
                    };
                    for x in 0..256 {
                        let hi = records
                            .iter()
                            .position(|r| r.ratio as usize >= x)
                            .unwrap_or(records.len() - 1);
                        let lo = hi.saturating_sub(1);
                        let a = &records[lo];
                        let b = &records[hi];
                        let t = if b.ratio == a.ratio {
                            0.
                        } else {
                            ((x as f32 - a.ratio as f32) / (b.ratio as f32 - a.ratio as f32))
                                .clamp(0., 1.)
                        };
                        for (channel, (a, b)) in [
                            (a.color.r, b.color.r),
                            (a.color.g, b.color.g),
                            (a.color.b, b.color.b),
                            (a.color.a, b.color.a),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            bytes.push(gradient_component(
                                a,
                                b,
                                t,
                                channel < 3
                                    && grad.interpolation == swf::GradientInterpolation::LinearRgb,
                            ));
                        }
                    }
                    Some(self.texture(256, 1, &bytes))
                }
                _ => None,
            });
        }
        let geometry = mesh
            .draws
            .iter()
            .map(|d| {
                self.geometry(
                    &d.vertices,
                    &d.indices,
                    match &d.draw_type {
                        DrawType::Bitmap(b) => Some(b.matrix),
                        DrawType::Gradient { matrix, .. } => Some(*matrix),
                        _ => None,
                    },
                )
            })
            .collect();

        let result = ShapeHandle(Arc::new(Shape {
            mesh,
            textures,
            geometry,
        }));
        let mut stats = self.stats.get();
        stats.tessellation_us += started.elapsed().as_micros() as u64;
        self.stats.set(stats);
        result
    }
    fn submit_frame(&mut self, clear: Color, commands: CommandList, caches: Vec<BitmapCacheEntry>) {
        for cache in caches {
            let handle = cache.handle;
            self.render_texture(&handle, cache.commands, cache.clear, false);
            let tex = (&*handle.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .unwrap();
            let mut pixels: Option<Vec<u8>> = None;
            for filter in cache.filters {
                if let ruffle_render::filters::Filter::ShaderFilter(filter) = filter {
                    if let Some(bytes) = pixels.take() {
                        self.upload(tex, &bytes);
                    }
                    if let Err(e) = self.shader_filter(
                        handle.clone(),
                        (0, 0),
                        (tex.w, tex.h),
                        handle.clone(),
                        (0, 0),
                        &filter,
                    ) {
                        crate::log(format!("PixelBender filter: {e}"));
                    }
                } else {
                    if pixels.is_none() {
                        pixels = self.read_texture(tex);
                    }
                    if let Some(bytes) = pixels.as_mut() {
                        self.filter_pixels(bytes, tex.w as usize, tex.h as usize, &filter);
                    }
                }
            }
            if let Some(bytes) = pixels {
                self.upload(tex, &bytes);
            }
        }
        let frontend = self.framebuffer;
        if let Some(aa) = self.prepare_aa() {
            self.framebuffer = Some(aa);
        }
        self.begin(clear);
        commands.execute(self);
        unsafe {
            self.gl.color_mask(true, true, true, true);
            self.gl.disable(glow::STENCIL_TEST);
            if self.framebuffer != frontend {
                let (w, h) = (self.dimensions.width as i32, self.dimensions.height as i32);
                self.gl
                    .bind_framebuffer(glow::READ_FRAMEBUFFER, self.framebuffer);
                self.gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, frontend);
                self.gl.blit_framebuffer(
                    0,
                    0,
                    w,
                    h,
                    0,
                    0,
                    w,
                    h,
                    glow::COLOR_BUFFER_BIT,
                    glow::NEAREST,
                );
            }
            self.framebuffer = frontend;
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, frontend);
            self.gl.bind_vertex_array(None);
            self.gl.use_program(None);
        }
    }
    fn render_offscreen(
        &mut self,
        h: BitmapHandle,
        c: CommandList,
        _: StageQuality,
        mut bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        let read = self.offscreen(h, c, Color::TRANSPARENT, true)?;
        let read = (&*read as &dyn std::any::Any).downcast_ref::<Readback>()?;
        bounds.clamp(read.stride / 4, (read.bytes.len() as u32) / read.stride);
        let mut bytes = Vec::new();
        for y in bounds.y_min..bounds.y_max {
            let start = (y * read.stride + bounds.x_min * 4) as usize;
            bytes.extend_from_slice(&read.bytes[start..start + bounds.width() as usize * 4]);
        }
        Some(Box::new(Readback {
            bytes,
            stride: bounds.width() * 4,
        }))
    }
    fn is_offscreen_supported(&self) -> bool {
        true
    }
    fn is_filter_supported(&self, f: &ruffle_render::filters::Filter) -> bool {
        crate::filters::supported(f) || matches!(f, ruffle_render::filters::Filter::ShaderFilter(_))
    }
    fn apply_filter(
        &mut self,
        source: BitmapHandle,
        point: (u32, u32),
        size: (u32, u32),
        dest: BitmapHandle,
        at: (i32, i32),
        filter: ruffle_render::filters::Filter,
    ) -> Option<Box<dyn SyncHandle>> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        if let ruffle_render::filters::Filter::ShaderFilter(filter) = &filter {
            return self
                .shader_filter(source, point, size, dest, at, filter)
                .ok();
        }
        if !crate::filters::supported(&filter) || size.0 == 0 || size.1 == 0 {
            return None;
        }
        let source = (&*source.0 as &dyn std::any::Any).downcast_ref::<Texture>()?;
        let dest = (&*dest.0 as &dyn std::any::Any).downcast_ref::<Texture>()?;
        let pixels = self.read_texture(source)?;
        let mut result = self.read_texture(dest)?;
        let (w, h) = (size.0 as usize, size.1 as usize);
        let mut region = vec![0; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (point.0 as usize + x, point.1 as usize + y);
                if sx < source.w as usize && sy < source.h as usize {
                    let i = (sy * source.w as usize + sx) * 4;
                    region[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&pixels[i..i + 4]);
                }
            }
        }
        self.filter_pixels(&mut region, w, h, &filter);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (at.0 + x as i32, at.1 + y as i32);
                if dx >= 0 && dy >= 0 && dx < dest.w as i32 && dy < dest.h as i32 {
                    let i = (dy as usize * dest.w as usize + dx as usize) * 4;
                    result[i..i + 4].copy_from_slice(&region[(y * w + x) * 4..(y * w + x) * 4 + 4]);
                }
            }
        }
        self.upload(dest, &result);
        Some(Box::new(Readback {
            bytes: result,
            stride: dest.w * 4,
        }))
    }
    fn register_bitmap(&mut self, b: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        let b = b.to_rgba();
        Ok(self.texture(b.width(), b.height(), b.data()))
    }
    fn create_empty_texture(
        &mut self,
        w: NonZeroU32,
        h: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        // Cache bitmaps change size as movie clips animate. Reuse only images
        // with no remaining owner; a live BitmapHandle can never alias these.
        let entry = self.image_pool.lock().unwrap().take(w.get(), h.get());
        let handle = if let Some(entry) = entry {
            BitmapHandle(Arc::new(Texture {
                id: entry.id,
                w: entry.w,
                h: entry.h,
                gl: self.gl.clone(),
                alive: self.alive.clone(),
                sampling: std::cell::Cell::new(entry.sampling),
                pool: Some(self.image_pool.clone()),
            }))
        } else {
            self.texture_owner(w.get(), h.get(), &[], Some(self.image_pool.clone()))
        };
        let texture = (&*handle.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        unsafe {
            let g = &self.gl;
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(self.copy_fb));
            g.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture.id),
                0,
            );
            g.disable(glow::SCISSOR_TEST);
            g.color_mask(true, true, true, true);
            g.clear_color(0., 0., 0., 0.);
            g.clear(glow::COLOR_BUFFER_BIT);
        }
        self.restore_target();
        Ok(handle)
    }
    fn update_texture(
        &mut self,
        h: &BitmapHandle,
        b: Bitmap<'_>,
        _: PixelRegion,
    ) -> Result<(), Error> {
        let t = (&*h.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        let b = b.to_rgba();
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(t.id));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                b.width() as i32,
                b.height() as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(b.data())),
            );
        }
        Ok(())
    }
    fn resolve_sync_handle(
        &mut self,
        h: Box<dyn SyncHandle>,
        read: RgbaBufRead,
    ) -> Result<(), Error> {
        if let Some(h) = (&*h as &dyn std::any::Any).downcast_ref::<Readback>() {
            read(&h.bytes, h.stride);
            return Ok(());
        }
        if let Some(h) = (&*h as &dyn std::any::Any).downcast_ref::<TextureReadback>() {
            let t = (&*h.handle.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .ok_or(Error::UnknownType)?;
            let bytes = self.read_texture(t).ok_or(Error::UnknownType)?;
            read(&bytes, t.w * 4);
            return Ok(());
        }
        Err(Error::UnknownType)
    }
    fn create_context3d(&mut self, profile: Context3DProfile) -> Result<Box<dyn Context3D>, Error> {
        Ok(Box::new(context3d_gl::Context::new(
            self.gl.clone(),
            self.alive.clone(),
            profile,
        )?))
    }
    fn debug_info(&self) -> Cow<'static, str> {
        Cow::Borrowed("libretro native OpenGL")
    }
    fn name(&self) -> &'static str {
        "libretro-opengl"
    }
    fn set_quality(&mut self, q: StageQuality) {
        self.samples = match q {
            StageQuality::Low => 1,
            StageQuality::Medium => 2,
            _ => 4,
        };
    }
    fn compile_pixelbender_shader(
        &mut self,
        shader: PixelBenderShader,
    ) -> Result<PixelBenderShaderHandle, Error> {
        crate::pixelbender::compile(self.gl.clone(), self.alive.clone(), shader)
    }
    fn run_pixelbender_shader(
        &mut self,
        shader: PixelBenderShaderHandle,
        args: &[PixelBenderShaderArgument],
        target: &PixelBenderTarget,
    ) -> Result<PixelBenderOutput, Error> {
        self.run_pb(shader, args, target, false)
    }
}
impl CommandHandler for GlRenderer {
    fn render_shape(&mut self, h: ShapeHandle, t: Transform) {
        let s = (&*h.0 as &dyn std::any::Any)
            .downcast_ref::<Shape>()
            .unwrap();
        for (i, d) in s.mesh.draws.iter().enumerate() {
            let (_uv, mode, smooth, repeat) = match &d.draw_type {
                DrawType::Color => (None, 0, false, false),
                DrawType::Bitmap(b) => (Some(b.matrix), 1, b.is_smoothed, b.is_repeating),
                DrawType::Gradient { matrix, gradient } => {
                    let g = &s.mesh.gradients[*gradient];
                    (
                        Some(*matrix),
                        match g.gradient_type {
                            GradientType::Linear => 2,
                            GradientType::Radial => 3,
                            GradientType::Focal => 4,
                        },
                        true,
                        false,
                    )
                }
            };
            let gradient = if let DrawType::Gradient { gradient, .. } = &d.draw_type {
                Some(&s.mesh.gradients[*gradient])
            } else {
                None
            };
            if mode != 0 && s.textures[i].is_none() {
                continue;
            }
            let count = if self.writing != 0 {
                d.mask_index_count as usize
            } else {
                d.indices.len()
            };
            self.draw_geometry(
                &s.geometry[i],
                count,
                &t,
                s.textures[i].as_ref(),
                mode,
                smooth,
                repeat,
                gradient,
            );
        }
    }
    fn render_bitmap(
        &mut self,
        b: BitmapHandle,
        mut t: Transform,
        s: bool,
        p: PixelSnapping,
        region: PixelRegion,
    ) {
        p.apply(&mut t.matrix);
        let tex = (&*b.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        let (w, h) = (region.width() as f32, region.height() as f32);
        if w == 0. || h == 0. {
            return;
        }
        let v = vec![
            Vertex {
                x: 0.,
                y: 0.,
                color: Color::WHITE,
            },
            Vertex {
                x: w,
                y: 0.,
                color: Color::WHITE,
            },
            Vertex {
                x: w,
                y: h,
                color: Color::WHITE,
            },
            Vertex {
                x: 0.,
                y: h,
                color: Color::WHITE,
            },
        ];
        self.draw(
            &v,
            &[0, 1, 2, 0, 2, 3],
            &t,
            Some([
                [1. / tex.w as f32, 0., 0.],
                [0., 1. / tex.h as f32, 0.],
                [
                    region.x_min as f32 / tex.w as f32,
                    region.y_min as f32 / tex.h as f32,
                    1.,
                ],
            ]),
            Some(&b),
            1,
            s,
            false,
            None,
        );
    }
    fn render_stage3d(&mut self, b: BitmapHandle, t: Transform) {
        let tex = (&*b.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        let region = PixelRegion::for_whole_size(tex.w, tex.h);
        self.render_bitmap(b, t, true, PixelSnapping::Never, region);
    }
    fn draw_rect(&mut self, c: Color, m: Matrix) {
        let v = vec![
            Vertex {
                x: 0.,
                y: 0.,
                color: c,
            },
            Vertex {
                x: 1.,
                y: 0.,
                color: c,
            },
            Vertex {
                x: 1.,
                y: 1.,
                color: c,
            },
            Vertex {
                x: 0.,
                y: 1.,
                color: c,
            },
        ];
        self.draw(
            &v,
            &[0, 1, 2, 0, 2, 3],
            &Transform {
                matrix: m,
                ..Default::default()
            },
            None,
            None,
            0,
            false,
            false,
            None,
        );
    }
    fn draw_line(&mut self, c: Color, m: Matrix) {
        ruffle_render::lines::emulate_line(self, c, m);
    }
    fn draw_line_rect(&mut self, c: Color, m: Matrix) {
        ruffle_render::lines::emulate_line_rect(self, c, m);
    }
    fn render_alpha_mask(&mut self, a: CommandList, b: CommandList) {
        let source = self.group(a);
        let mask = self.group(b);
        self.composite(&source, &mask, 13, false);
    }
    fn push_mask(&mut self) {
        unsafe {
            self.gl.enable(glow::STENCIL_TEST);
            self.gl.color_mask(false, false, false, false);
            self.gl.stencil_mask(255);
            self.gl.stencil_func(glow::EQUAL, self.mask as i32, 255);
            self.gl.stencil_op(glow::KEEP, glow::KEEP, glow::INCR);
        }
        self.mask += 1;
        self.writing = 1;
    }
    fn activate_mask(&mut self) {
        unsafe {
            self.gl.color_mask(true, true, true, true);
            self.gl.stencil_mask(0);
            self.gl.stencil_func(glow::EQUAL, self.mask as i32, 255);
            self.gl.stencil_op(glow::KEEP, glow::KEEP, glow::KEEP);
        }
        self.writing = 0;
    }
    fn deactivate_mask(&mut self) {
        unsafe {
            self.gl.color_mask(false, false, false, false);
            self.gl.stencil_mask(255);
            self.gl.stencil_func(glow::EQUAL, self.mask as i32, 255);
            self.gl.stencil_op(glow::KEEP, glow::KEEP, glow::DECR);
        }
        self.writing = -1;
    }
    fn pop_mask(&mut self) {
        self.mask = self.mask.saturating_sub(1);
        self.activate_mask();
        if self.mask == 0 {
            unsafe {
                self.gl.disable(glow::STENCIL_TEST);
            }
        }
    }
    fn blend(&mut self, c: CommandList, mode: RenderBlendMode) {
        let mode = match mode {
            RenderBlendMode::Builtin(mode) => mode,
            RenderBlendMode::Shader(shader) => {
                self.blend_pixelbender(c, shader);
                return;
            }
        };
        if mode == swf::BlendMode::Normal {
            c.execute(self);
            return;
        }

        let source = self.group(c);
        if mode == swf::BlendMode::Layer {
            self.render_bitmap(
                source,
                Transform::default(),
                false,
                PixelSnapping::Never,
                PixelRegion::for_whole_size(self.dimensions.width, self.dimensions.height),
            );
            return;
        }
        let dest = self.transient(self.dimensions.width, self.dimensions.height);
        let t = (&*dest.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        self.copy_target_to(t);
        let mode = match mode {
            swf::BlendMode::Multiply => 1,
            swf::BlendMode::Screen => 2,
            swf::BlendMode::Lighten => 3,
            swf::BlendMode::Darken => 4,
            swf::BlendMode::Difference => 5,
            swf::BlendMode::Add => 6,
            swf::BlendMode::Subtract => 7,
            swf::BlendMode::Invert => 8,
            swf::BlendMode::Overlay => 9,
            swf::BlendMode::HardLight => 10,
            swf::BlendMode::Alpha => 11,
            swf::BlendMode::Erase => 12,
            _ => 0,
        };
        self.composite(&source, &dest, mode, true);
    }
}
impl GlRenderer {
    fn apply_blend(&self) {
        unsafe {
            self.gl.blend_equation(glow::FUNC_ADD);
            let (src, dst) = match self.blend_mode {
                swf::BlendMode::Add => (glow::ONE, glow::ONE),
                swf::BlendMode::Multiply => (glow::DST_COLOR, glow::ONE_MINUS_SRC_ALPHA),
                swf::BlendMode::Screen => (glow::ONE, glow::ONE_MINUS_SRC_COLOR),
                swf::BlendMode::Subtract => {
                    self.gl.blend_equation(glow::FUNC_REVERSE_SUBTRACT);
                    (glow::ONE, glow::ONE)
                }
                _ => (glow::ONE, glow::ONE_MINUS_SRC_ALPHA),
            };
            self.gl.blend_func(src, dst);
        }
    }
}
impl Drop for GlRenderer {
    fn drop(&mut self) {
        unsafe {
            if !self.alive.load(Ordering::Relaxed) {
                return;
            }
            self.drop_aa();
            self.gl.delete_buffer(self.vb);
            self.gl.delete_buffer(self.ib);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_program(self.program);
            self.gl.delete_framebuffer(self.copy_fb);
            for t in self.targets.drain(..) {
                self.gl.delete_framebuffer(t.fb);
                self.gl.delete_renderbuffer(t.stencil);
            }
        }
    }
}

#[cfg(all(test, windows))]
#[path = "gl_tests.rs"]
mod tests;
