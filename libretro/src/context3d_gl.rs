//! Stage3D command backend using desktop OpenGL and the upstream AGAL compiler.
use super::*;
use naga_agal::{SamplerConfig, VertexAttributeFormat as Attribute};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

struct Buffer {
    id: Option<glow::Buffer>,
    bytes: usize,
    stride: usize,
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
}
impl IndexBuffer for Buffer {}
impl VertexBuffer for Buffer {}
impl Drop for Buffer {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed)
            && let Some(id) = self.id
        {
            unsafe {
                self.gl.delete_buffer(id);
            }
        }
    }
}
#[derive(Debug)]
struct Image {
    bitmap: BitmapHandle,
    w: u32,
    h: u32,
    cube: bool,
    format: Context3DTextureFormat,
}
impl ruffle_render::backend::Texture for Image {
    fn width(&self) -> u32 {
        self.w
    }
    fn height(&self) -> u32 {
        self.h
    }
}
impl Image {
    fn id(&self) -> glow::Texture {
        (&*self.bitmap.0 as &dyn std::any::Any)
            .downcast_ref::<super::Texture>()
            .unwrap()
            .id
    }
    fn target(&self) -> u32 {
        if self.cube {
            glow::TEXTURE_CUBE_MAP
        } else {
            glow::TEXTURE_2D
        }
    }
}
struct Shaders {
    vertex: naga_agal::ParsedBytecode,
    fragment: naga_agal::ParsedBytecode,
    samplers: [Option<SamplerConfig>; 8],
    programs: RefCell<HashMap<[Option<Attribute>; 8], Rc<crate::glsl::Program>>>,
}
impl ShaderModule for Shaders {}
#[derive(Clone, Copy)]
struct Stencil {
    compare: u32,
    fail: u32,
    depth_fail: u32,
    pass: u32,
}
impl Default for Stencil {
    fn default() -> Self {
        Self {
            compare: glow::ALWAYS,
            fail: glow::KEEP,
            depth_fail: glow::KEEP,
            pass: glow::KEEP,
        }
    }
}
struct Target {
    owner: Rc<dyn ruffle_render::backend::Texture>,
    fb: glow::Framebuffer,
    resolve: Option<glow::Framebuffer>,
    color: Option<glow::Renderbuffer>,
    depth: Option<glow::Renderbuffer>,
    w: u32,
    h: u32,
    samples: u32,
    layer: u32,
    depth_enabled: bool,
}
impl Target {
    unsafe fn new(
        gl: &glow::Context,
        owner: Rc<dyn ruffle_render::backend::Texture>,
        layer: u32,
        depth_enabled: bool,
        anti_alias: u32,
    ) -> Result<Self, String> {
        unsafe {
            let image = (owner.as_ref() as &dyn std::any::Any)
                .downcast_ref::<Image>()
                .ok_or("invalid Stage3D texture")?;
            let (w, h) = (image.w, image.h);
            let samples = anti_alias
                .min(gl.get_parameter_i32(glow::MAX_SAMPLES) as u32)
                .max(1);
            let target = if image.cube {
                glow::TEXTURE_CUBE_MAP_POSITIVE_X + layer.min(5)
            } else {
                glow::TEXTURE_2D
            };
            let fb = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            let (mut resolve, mut color) = (None, None);
            if samples > 1 {
                let rb = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
                gl.renderbuffer_storage_multisample(
                    glow::RENDERBUFFER,
                    samples as i32,
                    glow::RGBA8,
                    w as i32,
                    h as i32,
                );
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::RENDERBUFFER,
                    Some(rb),
                );
                color = Some(rb);
                let dst = gl.create_framebuffer()?;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dst));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    target,
                    Some(image.id()),
                    0,
                );
                resolve = Some(dst);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            } else {
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    target,
                    Some(image.id()),
                    0,
                );
            }
            let depth = if depth_enabled {
                let rb = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
                if samples > 1 {
                    gl.renderbuffer_storage_multisample(
                        glow::RENDERBUFFER,
                        samples as i32,
                        glow::DEPTH24_STENCIL8,
                        w as i32,
                        h as i32,
                    );
                } else {
                    gl.renderbuffer_storage(
                        glow::RENDERBUFFER,
                        glow::DEPTH24_STENCIL8,
                        w as i32,
                        h as i32,
                    );
                }
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(rb),
                );
                Some(rb)
            } else {
                None
            };
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                return Err("Stage3D framebuffer is incomplete".into());
            }
            Ok(Self {
                owner,
                fb,
                resolve,
                color,
                depth,
                w,
                h,
                samples,
                layer,
                depth_enabled,
            })
        }
    }
    fn resolve(&self, gl: &glow::Context) {
        if let Some(dst) = self.resolve {
            unsafe {
                gl.disable(glow::SCISSOR_TEST);
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fb));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(dst));
                gl.blit_framebuffer(
                    0,
                    0,
                    self.w as i32,
                    self.h as i32,
                    0,
                    0,
                    self.w as i32,
                    self.h as i32,
                    glow::COLOR_BUFFER_BIT,
                    glow::NEAREST,
                );
            }
        }
    }
    fn dispose(self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fb);
            if let Some(f) = self.resolve {
                gl.delete_framebuffer(f);
            }
            for rb in [self.color, self.depth].into_iter().flatten() {
                gl.delete_renderbuffer(rb);
            }
        }
    }
}
type VertexBinding = (Rc<dyn VertexBuffer>, Context3DVertexBufferFormat, u32);
pub struct Context {
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
    profile: Context3DProfile,
    front: BitmapHandle,
    back: Option<Target>,
    targets: Vec<Target>,
    active: Option<usize>,
    visible: bool,
    cleared: bool,
    vao: glow::VertexArray,
    constants: [glow::Buffer; 2],
    shaders: Option<Rc<dyn ShaderModule>>,
    vertices: [Option<VertexBinding>; 8],
    textures: [Option<Rc<dyn ruffle_render::backend::Texture>>; 8],
    samplers: [glow::Sampler; 8],
    sampler_config: [SamplerConfig; 8],
    culling: Context3DTriangleFace,
    color_mask: (bool, bool, bool, bool),
    depth_mask: bool,
    depth_compare: Context3DCompareMode,
    blend: (Context3DBlendFactor, Context3DBlendFactor),
    scissor: Option<swf::Rectangle<swf::Twips>>,
    stencil: [Stencil; 2],
    reference: u32,
    read_mask: u32,
    write_mask: u32,
}
fn compare(mode: Context3DCompareMode) -> u32 {
    match mode {
        Context3DCompareMode::Never => glow::NEVER,
        Context3DCompareMode::Less => glow::LESS,
        Context3DCompareMode::Equal => glow::EQUAL,
        Context3DCompareMode::LessEqual => glow::LEQUAL,
        Context3DCompareMode::Greater => glow::GREATER,
        Context3DCompareMode::NotEqual => glow::NOTEQUAL,
        Context3DCompareMode::GreaterEqual => glow::GEQUAL,
        Context3DCompareMode::Always => glow::ALWAYS,
    }
}
fn action(mode: Context3DStencilAction) -> u32 {
    match mode {
        Context3DStencilAction::DecrementSaturate => glow::DECR,
        Context3DStencilAction::DecrementWrap => glow::DECR_WRAP,
        Context3DStencilAction::IncrementSaturate => glow::INCR,
        Context3DStencilAction::IncrementWrap => glow::INCR_WRAP,
        Context3DStencilAction::Invert => glow::INVERT,
        Context3DStencilAction::Keep => glow::KEEP,
        Context3DStencilAction::Set => glow::REPLACE,
        Context3DStencilAction::Zero => glow::ZERO,
    }
}
fn factor(mode: Context3DBlendFactor) -> u32 {
    match mode {
        Context3DBlendFactor::DestinationAlpha => glow::DST_ALPHA,
        Context3DBlendFactor::DestinationColor => glow::DST_COLOR,
        Context3DBlendFactor::One => glow::ONE,
        Context3DBlendFactor::OneMinusDestinationAlpha => glow::ONE_MINUS_DST_ALPHA,
        Context3DBlendFactor::OneMinusDestinationColor => glow::ONE_MINUS_DST_COLOR,
        Context3DBlendFactor::OneMinusSourceAlpha => glow::ONE_MINUS_SRC_ALPHA,
        Context3DBlendFactor::OneMinusSourceColor => glow::ONE_MINUS_SRC_COLOR,
        Context3DBlendFactor::SourceAlpha => glow::SRC_ALPHA,
        Context3DBlendFactor::SourceColor => glow::SRC_COLOR,
        Context3DBlendFactor::Zero => glow::ZERO,
    }
}
fn attribute(f: Context3DVertexBufferFormat) -> Attribute {
    match f {
        Context3DVertexBufferFormat::Float1 => Attribute::Float1,
        Context3DVertexBufferFormat::Float2 => Attribute::Float2,
        Context3DVertexBufferFormat::Float3 => Attribute::Float3,
        Context3DVertexBufferFormat::Float4 => Attribute::Float4,
        Context3DVertexBufferFormat::Bytes4 => Attribute::Bytes4,
    }
}
impl Context {
    pub fn new(
        gl: Arc<glow::Context>,
        alive: Arc<AtomicBool>,
        profile: Context3DProfile,
    ) -> Result<Self, Error> {
        unsafe {
            let front = Self::image(&gl, &alive, 1, 1, Context3DTextureFormat::Bgra, false)?
                .bitmap
                .clone();
            let vao = gl
                .create_vertex_array()
                .map_err(crate::pixelbender::error)?;
            let constants = [
                gl.create_buffer().map_err(crate::pixelbender::error)?,
                gl.create_buffer().map_err(crate::pixelbender::error)?,
            ];
            for buffer in constants {
                gl.bind_buffer(glow::UNIFORM_BUFFER, Some(buffer));
                gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, &vec![0; 4096], glow::DYNAMIC_DRAW);
            }
            let mut samplers = vec![];
            for _ in 0..8 {
                samplers.push(gl.create_sampler().map_err(crate::pixelbender::error)?);
            }
            let samplers = samplers.try_into().map_err(|_| Error::UnknownType)?;
            Ok(Self {
                gl,
                alive,
                profile,
                front,
                back: None,
                targets: vec![],
                active: None,
                visible: false,
                cleared: false,
                vao,
                constants,
                shaders: None,
                vertices: std::array::from_fn(|_| None),
                textures: std::array::from_fn(|_| None),
                samplers,
                sampler_config: [SamplerConfig::default(); 8],
                culling: Context3DTriangleFace::None,
                color_mask: (true, true, true, true),
                depth_mask: true,
                depth_compare: Context3DCompareMode::LessEqual,
                blend: (Context3DBlendFactor::One, Context3DBlendFactor::Zero),
                scissor: None,
                stencil: [Stencil::default(); 2],
                reference: 0,
                read_mask: u32::MAX,
                write_mask: u32::MAX,
            })
        }
    }
    fn image(
        gl: &Arc<glow::Context>,
        alive: &Arc<AtomicBool>,
        w: u32,
        h: u32,
        format: Context3DTextureFormat,
        cube: bool,
    ) -> Result<Rc<Image>, Error> {
        unsafe {
            if w == 0
                || h == 0
                || w > gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32
                || h > gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32
                || w as u64 * h as u64 * 8 > 512 * 1024 * 1024
            {
                return Err(Error::TooLarge);
            }
            let id = gl.create_texture().map_err(crate::pixelbender::error)?;
            let target = if cube {
                glow::TEXTURE_CUBE_MAP
            } else {
                glow::TEXTURE_2D
            };
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(target, Some(id));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            let compressed = match format {
                Context3DTextureFormat::Compressed => Some((0x83F1, 8)),
                Context3DTextureFormat::CompressedAlpha => Some((0x83F3, 16)),
                _ => None,
            };
            for face in 0..if cube { 6 } else { 1 } {
                let t = if cube {
                    glow::TEXTURE_CUBE_MAP_POSITIVE_X + face
                } else {
                    glow::TEXTURE_2D
                };
                if let Some((format, size)) = compressed {
                    let bytes = vec![0; w.div_ceil(4) as usize * h.div_ceil(4) as usize * size];
                    gl.compressed_tex_image_2d(
                        t,
                        0,
                        format,
                        w as i32,
                        h as i32,
                        0,
                        bytes.len() as i32,
                        &bytes,
                    );
                } else {
                    gl.tex_image_2d(
                        t,
                        0,
                        if matches!(format, Context3DTextureFormat::RgbaHalfFloat) {
                            glow::RGBA16F
                        } else {
                            glow::RGBA8
                        } as i32,
                        w as i32,
                        h as i32,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(&vec![0; w as usize * h as usize * 4])),
                    );
                }
            }
            gl.tex_parameter_i32(target, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(target, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.generate_mipmap(target);
            let bitmap = BitmapHandle(Arc::new(super::Texture {
                id,
                w,
                h,
                gl: gl.clone(),
                alive: alive.clone(),
                sampling: std::cell::Cell::new((glow::LINEAR, glow::CLAMP_TO_EDGE)),
                pool: None,
            }));
            Ok(Rc::new(Image {
                bitmap,
                w,
                h,
                cube,
                format,
            }))
        }
    }
    fn buffer(&self, bytes: usize, stride: usize, usage: BufferUsage) -> Buffer {
        let id = if bytes == 0 || bytes > 256 * 1024 * 1024 {
            None
        } else {
            unsafe {
                self.gl.create_buffer().ok().inspect(|id| {
                    self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(*id));
                    self.gl.buffer_data_u8_slice(
                        glow::ARRAY_BUFFER,
                        &vec![0; bytes],
                        match usage {
                            BufferUsage::StaticDraw => glow::STATIC_DRAW,
                            BufferUsage::DynamicDraw => glow::DYNAMIC_DRAW,
                        },
                    );
                })
            }
        };
        Buffer {
            id,
            bytes,
            stride,
            gl: self.gl.clone(),
            alive: self.alive.clone(),
        }
    }
    fn target(&self) -> Option<&Target> {
        self.active
            .and_then(|i| self.targets.get(i))
            .or(self.back.as_ref())
    }
    fn bind_target(&self) -> bool {
        let Some(t) = self.target() else {
            return false;
        };
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fb));
            self.gl.viewport(0, 0, t.w as i32, t.h as i32);
        }
        true
    }
    fn resolve_active(&self) {
        if let Some(t) = self.target() {
            t.resolve(&self.gl);
        }
    }
    fn draw(&mut self, index: &dyn IndexBuffer, first: usize, count: isize) {
        let Some(index) = (index as &dyn std::any::Any).downcast_ref::<Buffer>() else {
            return;
        };
        let Some(id) = index.id else {
            return;
        };
        let available = (index.bytes / 2).saturating_sub(first);
        let count = if count < 0 {
            available
        } else {
            (count as usize).saturating_mul(3).min(available)
        };
        if count == 0 || !self.bind_target() {
            return;
        }
        let Some(shaders) = self
            .shaders
            .as_ref()
            .and_then(|h| (h.as_ref() as &dyn std::any::Any).downcast_ref::<Shaders>())
        else {
            return;
        };
        let formats =
            std::array::from_fn(|i| self.vertices[i].as_ref().map(|(_, f, _)| attribute(*f)));
        let cached = { shaders.programs.borrow().get(&formats).cloned() };
        let program = if let Some(p) = cached {
            p
        } else {
            let result = (|| {
                let vertex = naga_agal::agal_to_naga(&shaders.vertex, &formats)
                    .map_err(|e| format!("{e:?}"))?;
                let fragment = naga_agal::agal_to_naga(&shaders.fragment, &formats)
                    .map_err(|e| format!("{e:?}"))?;
                crate::glsl::compile(
                    self.gl.clone(),
                    self.alive.clone(),
                    &vertex,
                    "main",
                    &fragment,
                    "main",
                )
                .map(Rc::new)
            })();
            match result {
                Ok(p) => {
                    shaders.programs.borrow_mut().insert(formats, p.clone());
                    p
                }
                Err(e) => {
                    crate::log(format!("Stage3D shader: {e}"));
                    return;
                }
            }
        };
        unsafe {
            let g = &self.gl;
            g.use_program(Some(program.id));
            g.bind_vertex_array(Some(self.vao));
            g.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(id));
            for i in 0..8 {
                g.disable_vertex_attrib_array(i);
            }
            for (i, binding) in self.vertices.iter().enumerate() {
                if let Some((buffer, format, offset)) = binding {
                    let Some(b) = (buffer.as_ref() as &dyn std::any::Any).downcast_ref::<Buffer>()
                    else {
                        continue;
                    };
                    if let Some(id) = b.id {
                        g.bind_buffer(glow::ARRAY_BUFFER, Some(id));
                        let (size, ty, normalized) = match format {
                            Context3DVertexBufferFormat::Float1 => (1, glow::FLOAT, false),
                            Context3DVertexBufferFormat::Float2 => (2, glow::FLOAT, false),
                            Context3DVertexBufferFormat::Float3 => (3, glow::FLOAT, false),
                            Context3DVertexBufferFormat::Float4 => (4, glow::FLOAT, false),
                            Context3DVertexBufferFormat::Bytes4 => (4, glow::UNSIGNED_BYTE, true),
                        };
                        g.enable_vertex_attrib_array(i as u32);
                        g.vertex_attrib_pointer_f32(
                            i as u32,
                            size,
                            ty,
                            normalized,
                            b.stride as i32,
                            (*offset * 4) as i32,
                        );
                    }
                }
            }
            for (i, buffer) in self.constants.iter().enumerate() {
                g.bind_buffer_base(glow::UNIFORM_BUFFER, i as u32, Some(*buffer));
            }
            for binding in &program.textures {
                let Some(slot) = binding
                    .texture
                    .checked_sub(naga_agal::TEXTURE_START_BIND_INDEX)
                    .map(|n| n as usize)
                    .filter(|n| *n < 8)
                else {
                    return;
                };
                let Some(image) = self.textures[slot]
                    .as_ref()
                    .and_then(|h| (h.as_ref() as &dyn std::any::Any).downcast_ref::<Image>())
                else {
                    return;
                };
                let config = self.sampler_config[slot];
                let sampler = self.samplers[slot];
                let filter = if matches!(config.filter, naga_agal::Filter::Nearest) {
                    glow::NEAREST
                } else {
                    glow::LINEAR
                };
                let min = match config.mipmap {
                    naga_agal::Mipmap::Disable => filter,
                    naga_agal::Mipmap::Nearest => {
                        if filter == glow::NEAREST {
                            glow::NEAREST_MIPMAP_NEAREST
                        } else {
                            glow::LINEAR_MIPMAP_NEAREST
                        }
                    }
                    naga_agal::Mipmap::Linear => {
                        if filter == glow::NEAREST {
                            glow::NEAREST_MIPMAP_LINEAR
                        } else {
                            glow::LINEAR_MIPMAP_LINEAR
                        }
                    }
                };
                let (u, v) = match config.wrapping {
                    naga_agal::Wrapping::Clamp => (glow::CLAMP_TO_EDGE, glow::CLAMP_TO_EDGE),
                    naga_agal::Wrapping::Repeat => (glow::REPEAT, glow::REPEAT),
                    naga_agal::Wrapping::ClampURepeatV => (glow::CLAMP_TO_EDGE, glow::REPEAT),
                    naga_agal::Wrapping::RepeatUClampV => (glow::REPEAT, glow::CLAMP_TO_EDGE),
                };
                g.sampler_parameter_i32(sampler, glow::TEXTURE_MIN_FILTER, min as i32);
                g.sampler_parameter_i32(sampler, glow::TEXTURE_MAG_FILTER, filter as i32);
                g.sampler_parameter_i32(sampler, glow::TEXTURE_WRAP_S, u as i32);
                g.sampler_parameter_i32(sampler, glow::TEXTURE_WRAP_T, v as i32);
                g.active_texture(glow::TEXTURE0 + binding.unit);
                g.bind_texture(image.target(), Some(image.id()));
                g.bind_sampler(binding.unit, Some(sampler));
                g.uniform_1_i32(binding.location.as_ref(), binding.unit as i32);
            }
            let depth_enabled = self.target().is_some_and(|t| t.depth_enabled);
            if depth_enabled {
                g.enable(glow::DEPTH_TEST);
                g.enable(glow::STENCIL_TEST);
            } else {
                g.disable(glow::DEPTH_TEST);
                g.disable(glow::STENCIL_TEST);
            }
            g.depth_mask(self.depth_mask);
            g.depth_func(compare(self.depth_compare));
            g.color_mask(
                self.color_mask.0,
                self.color_mask.1,
                self.color_mask.2,
                self.color_mask.3,
            );
            g.enable(glow::BLEND);
            g.blend_equation(glow::FUNC_ADD);
            g.blend_func(factor(self.blend.0), factor(self.blend.1));
            g.front_face(glow::CW);
            match self.culling {
                Context3DTriangleFace::None => g.disable(glow::CULL_FACE),
                face => {
                    g.enable(glow::CULL_FACE);
                    g.cull_face(match face {
                        Context3DTriangleFace::Front => glow::FRONT,
                        Context3DTriangleFace::Back => glow::BACK,
                        _ => glow::FRONT_AND_BACK,
                    });
                }
            }
            if let Some(rect) = self.scissor {
                g.enable(glow::SCISSOR_TEST);
                g.scissor(
                    rect.x_min.to_pixels().round() as i32,
                    rect.y_min.to_pixels().round() as i32,
                    (rect.x_max - rect.x_min).to_pixels().max(0.).round() as i32,
                    (rect.y_max - rect.y_min).to_pixels().max(0.).round() as i32,
                );
            } else {
                g.disable(glow::SCISSOR_TEST);
            }
            g.stencil_mask(self.write_mask);
            for (i, face) in [glow::FRONT, glow::BACK].into_iter().enumerate() {
                let s = self.stencil[i];
                g.stencil_func_separate(face, s.compare, self.reference as i32, self.read_mask);
                g.stencil_op_separate(face, s.fail, s.depth_fail, s.pass);
            }
            g.draw_elements(
                glow::TRIANGLES,
                count as i32,
                glow::UNSIGNED_SHORT,
                (first * 2) as i32,
            );
            for binding in &program.textures {
                g.bind_sampler(binding.unit, None);
            }
            g.active_texture(glow::TEXTURE0);
        }
    }
}
impl Context3D for Context {
    fn profile(&self) -> Context3DProfile {
        self.profile
    }
    fn bitmap_handle(&self) -> BitmapHandle {
        self.front.clone()
    }
    fn back_buffer_handle(&self) -> Option<BitmapHandle> {
        let back = self.back.as_ref()?;
        back.resolve(&self.gl);
        let image = (back.owner.as_ref() as &dyn std::any::Any).downcast_ref::<Image>()?;
        Some(image.bitmap.clone())
    }
    fn back_buffer_size(&self) -> Option<(u32, u32)> {
        self.back.as_ref().map(|t| (t.w, t.h))
    }
    fn buffers_cleared(&self) -> bool {
        self.cleared
    }
    fn should_render(&self) -> bool {
        self.visible
    }
    fn disposed_index_buffer_handle(&self) -> Rc<dyn IndexBuffer> {
        Rc::new(self.buffer(0, 0, BufferUsage::StaticDraw))
    }
    fn disposed_vertex_buffer_handle(&self) -> Rc<dyn VertexBuffer> {
        Rc::new(self.buffer(0, 0, BufferUsage::StaticDraw))
    }
    fn create_index_buffer(
        &mut self,
        usage: BufferUsage,
        num_indices: u32,
    ) -> Box<dyn IndexBuffer> {
        Box::new(self.buffer(num_indices as usize * 2, 0, usage))
    }
    fn create_vertex_buffer(
        &mut self,
        usage: BufferUsage,
        num_vertices: u32,
        data_32_per_vertex: u8,
    ) -> Rc<dyn VertexBuffer> {
        Rc::new(self.buffer(
            num_vertices as usize * data_32_per_vertex as usize * 4,
            data_32_per_vertex as usize * 4,
            usage,
        ))
    }
    fn create_texture(
        &mut self,
        w: u32,
        h: u32,
        format: Context3DTextureFormat,
        _: bool,
        _: u32,
    ) -> Result<Rc<dyn ruffle_render::backend::Texture>, Error> {
        Ok(Self::image(&self.gl, &self.alive, w, h, format, false)?)
    }
    fn create_cube_texture(
        &mut self,
        size: u32,
        format: Context3DTextureFormat,
        _: bool,
        _: u32,
    ) -> Result<Rc<dyn ruffle_render::backend::Texture>, Error> {
        Ok(Self::image(
            &self.gl,
            &self.alive,
            size,
            size,
            format,
            true,
        )?)
    }
    fn upload_shaders(
        &mut self,
        module: &RefCell<Option<Rc<dyn ShaderModule>>>,
        vertex: Vec<u8>,
        fragment: Vec<u8>,
    ) -> Result<(), naga_agal::AgalError> {
        if vertex.get(6) != Some(&0) || fragment.get(6) != Some(&1) {
            return Err(naga_agal::AgalError::InvalidShaderType);
        }
        let vertex = naga_agal::parse_bytecode(&vertex)?;
        let fragment = naga_agal::parse_bytecode(&fragment)?;
        let samplers = naga_agal::extract_sampler_configs(&fragment)?;
        *module.borrow_mut() = Some(Rc::new(Shaders {
            vertex,
            fragment,
            samplers,
            programs: RefCell::new(HashMap::new()),
        }));
        Ok(())
    }
    fn process_command(&mut self, command: Context3DCommand<'_>) {
        unsafe {
            match command {
                Context3DCommand::ConfigureBackBuffer {
                    width,
                    height,
                    anti_alias,
                    depth_and_stencil,
                    ..
                } => {
                    let result = (|| {
                        let image = Self::image(
                            &self.gl,
                            &self.alive,
                            width,
                            height,
                            Context3DTextureFormat::Bgra,
                            false,
                        )
                        .map_err(|e| e.to_string())?;
                        let front = Self::image(
                            &self.gl,
                            &self.alive,
                            width,
                            height,
                            Context3DTextureFormat::Bgra,
                            false,
                        )
                        .map_err(|e| e.to_string())?;
                        let target =
                            Target::new(&self.gl, image, 0, depth_and_stencil, anti_alias)?;
                        Ok::<_, String>((target, front.bitmap.clone()))
                    })();
                    match result {
                        Ok((target, front)) => {
                            if let Some(old) = self.back.replace(target) {
                                old.dispose(&self.gl);
                            }
                            self.front = front;
                            self.active = None;
                            self.cleared = false;
                        }
                        Err(e) => crate::log(format!("Stage3D configureBackBuffer: {e}")),
                    }
                }
                Context3DCommand::SetRenderToTexture {
                    texture,
                    enable_depth_and_stencil,
                    anti_alias,
                    surface_selector,
                } => {
                    self.resolve_active();
                    let Some(image) =
                        (texture.as_ref() as &dyn std::any::Any).downcast_ref::<Image>()
                    else {
                        return;
                    };
                    let found = self.targets.iter().position(|t| {
                        let i = (t.owner.as_ref() as &dyn std::any::Any)
                            .downcast_ref::<Image>()
                            .unwrap();
                        i.id() == image.id()
                            && t.layer == surface_selector
                            && t.depth_enabled == enable_depth_and_stencil
                            && t.samples
                                == anti_alias
                                    .min(self.gl.get_parameter_i32(glow::MAX_SAMPLES) as u32)
                                    .max(1)
                    });
                    if let Some(i) = found {
                        self.active = Some(i);
                    } else {
                        match Target::new(
                            &self.gl,
                            texture,
                            surface_selector,
                            enable_depth_and_stencil,
                            anti_alias,
                        ) {
                            Ok(t) => {
                                if self.targets.len() >= 16 {
                                    let index = (0..self.targets.len())
                                        .find(|i| Some(*i) != self.active)
                                        .unwrap_or(0);
                                    let old = std::mem::replace(&mut self.targets[index], t);
                                    old.dispose(&self.gl);
                                    self.active = Some(index);
                                } else {
                                    self.targets.push(t);
                                    self.active = Some(self.targets.len() - 1);
                                }
                            }
                            Err(e) => crate::log(format!("Stage3D setRenderToTexture: {e}")),
                        }
                    }
                }
                Context3DCommand::SetRenderToBackBuffer => {
                    self.resolve_active();
                    self.active = None;
                }
                Context3DCommand::Clear {
                    red,
                    green,
                    blue,
                    alpha,
                    depth,
                    stencil,
                    mask,
                } => {
                    if !self.bind_target() {
                        return;
                    }
                    let g = &self.gl;
                    g.disable(glow::SCISSOR_TEST);
                    g.color_mask(true, true, true, true);
                    g.depth_mask(true);
                    g.stencil_mask(u32::MAX);
                    g.clear_color(red as f32, green as f32, blue as f32, alpha as f32);
                    g.clear_depth_f64(depth);
                    g.clear_stencil(stencil as i32);
                    let mut bits = 0;
                    if mask & 1 != 0 {
                        bits |= glow::COLOR_BUFFER_BIT;
                    }
                    if self.target().is_some_and(|t| t.depth_enabled) {
                        if mask & 2 != 0 {
                            bits |= glow::DEPTH_BUFFER_BIT;
                        }
                        if mask & 4 != 0 {
                            bits |= glow::STENCIL_BUFFER_BIT;
                        }
                    }
                    g.clear(bits);
                    if self.active.is_none() {
                        self.cleared = true;
                    }
                }
                Context3DCommand::UploadToIndexBuffer {
                    buffer,
                    start_offset,
                    data,
                } => {
                    if let Some(b) = (buffer as &mut dyn std::any::Any).downcast_mut::<Buffer>()
                        && let Some(id) = b.id
                    {
                        let offset = start_offset.saturating_mul(2);
                        if offset.saturating_add(data.len()) <= b.bytes {
                            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(id));
                            self.gl.buffer_sub_data_u8_slice(
                                glow::ARRAY_BUFFER,
                                offset as i32,
                                data,
                            );
                        }
                    }
                }
                Context3DCommand::UploadToVertexBuffer {
                    buffer,
                    start_vertex,
                    data32_per_vertex,
                    data,
                } => {
                    if let Some(b) =
                        (buffer.as_ref() as &dyn std::any::Any).downcast_ref::<Buffer>()
                        && let Some(id) = b.id
                    {
                        let offset = start_vertex.saturating_mul(data32_per_vertex as usize * 4);
                        if offset.saturating_add(data.len()) <= b.bytes {
                            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(id));
                            self.gl.buffer_sub_data_u8_slice(
                                glow::ARRAY_BUFFER,
                                offset as i32,
                                data,
                            );
                        }
                    }
                }
                Context3DCommand::DrawTriangles {
                    index_buffer,
                    first_index,
                    num_triangles,
                } => self.draw(index_buffer, first_index, num_triangles),
                Context3DCommand::SetVertexBufferAt {
                    index,
                    buffer,
                    buffer_offset,
                } => {
                    if (index as usize) < 8 {
                        self.vertices[index as usize] = buffer.map(|(b, f)| (b, f, buffer_offset));
                    }
                }
                Context3DCommand::SetShaders { module } => {
                    if let Some(shader) = module
                        .as_ref()
                        .and_then(|h| (h.as_ref() as &dyn std::any::Any).downcast_ref::<Shaders>())
                    {
                        for (i, config) in shader.samplers.iter().enumerate() {
                            if let Some(config) = config {
                                self.sampler_config[i] = *config;
                            }
                        }
                    }
                    self.shaders = module;
                }
                Context3DCommand::SetProgramConstants {
                    program_type,
                    first_register,
                    matrix_raw_data_column_major,
                } => {
                    let index = if matches!(program_type, ProgramType::Vertex) {
                        0
                    } else {
                        1
                    };
                    let bytes = bytemuck::cast_slice(matrix_raw_data_column_major);
                    let offset = first_register as usize * 16;
                    if offset.saturating_add(bytes.len()) <= 4096 {
                        self.gl
                            .bind_buffer(glow::UNIFORM_BUFFER, Some(self.constants[index]));
                        self.gl.buffer_sub_data_u8_slice(
                            glow::UNIFORM_BUFFER,
                            offset as i32,
                            bytes,
                        );
                    }
                }
                Context3DCommand::SetCulling { face } => self.culling = face,
                Context3DCommand::CopyBitmapToTexture {
                    source,
                    source_width,
                    source_height,
                    dest,
                    layer,
                } => {
                    if let Some(image) =
                        (dest.as_ref() as &dyn std::any::Any).downcast_ref::<Image>()
                    {
                        let g = &self.gl;
                        g.active_texture(glow::TEXTURE0);
                        g.bind_texture(image.target(), Some(image.id()));
                        g.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                        let face = if image.cube {
                            glow::TEXTURE_CUBE_MAP_POSITIVE_X + layer.min(5)
                        } else {
                            glow::TEXTURE_2D
                        };
                        let pixels = source_width as usize * source_height as usize;
                        let compressed = match image.format {
                            Context3DTextureFormat::Compressed => Some(0x83F1),
                            Context3DTextureFormat::CompressedAlpha => Some(0x83F3),
                            _ => None,
                        };
                        if let Some(format) = compressed.filter(|_| source.len() != pixels * 4) {
                            g.compressed_tex_sub_image_2d(
                                face,
                                0,
                                0,
                                0,
                                source_width as i32,
                                source_height as i32,
                                format,
                                glow::CompressedPixelUnpackData::Slice(source),
                            );
                        } else {
                            let ty =
                                if matches!(image.format, Context3DTextureFormat::RgbaHalfFloat)
                                    && source.len() == pixels * 8
                                {
                                    glow::HALF_FLOAT
                                } else {
                                    glow::UNSIGNED_BYTE
                                };
                            if source.len() >= pixels * 4 {
                                g.tex_sub_image_2d(
                                    face,
                                    0,
                                    0,
                                    0,
                                    source_width as i32,
                                    source_height as i32,
                                    glow::RGBA,
                                    ty,
                                    glow::PixelUnpackData::Slice(Some(source)),
                                );
                            }
                        }
                        g.generate_mipmap(image.target());
                    }
                }
                Context3DCommand::SetTextureAt {
                    sampler, texture, ..
                } => {
                    if (sampler as usize) < 8 {
                        self.textures[sampler as usize] = texture;
                    }
                }
                Context3DCommand::SetColorMask {
                    red,
                    green,
                    blue,
                    alpha,
                } => self.color_mask = (red, green, blue, alpha),
                Context3DCommand::SetDepthTest {
                    depth_mask,
                    pass_compare_mode,
                } => {
                    self.depth_mask = depth_mask;
                    self.depth_compare = pass_compare_mode;
                }
                Context3DCommand::SetBlendFactors {
                    source_factor,
                    destination_factor,
                } => self.blend = (source_factor, destination_factor),
                Context3DCommand::SetSamplerStateAt {
                    sampler,
                    wrap,
                    filter,
                } => {
                    if sampler < 8 {
                        let config = &mut self.sampler_config[sampler as usize];
                        config.wrapping = match wrap {
                            Context3DWrapMode::Clamp => naga_agal::Wrapping::Clamp,
                            Context3DWrapMode::Repeat => naga_agal::Wrapping::Repeat,
                            Context3DWrapMode::ClampURepeatV => naga_agal::Wrapping::ClampURepeatV,
                            Context3DWrapMode::RepeatUClampV => naga_agal::Wrapping::RepeatUClampV,
                        };
                        config.filter = match filter {
                            Context3DTextureFilter::Nearest => naga_agal::Filter::Nearest,
                            Context3DTextureFilter::Linear => naga_agal::Filter::Linear,
                            Context3DTextureFilter::Anisotropic2X => {
                                naga_agal::Filter::Anisotropic2x
                            }
                            Context3DTextureFilter::Anisotropic4X => {
                                naga_agal::Filter::Anisotropic4x
                            }
                            Context3DTextureFilter::Anisotropic8X => {
                                naga_agal::Filter::Anisotropic8x
                            }
                            Context3DTextureFilter::Anisotropic16X => {
                                naga_agal::Filter::Anisotropic16x
                            }
                        };
                        config.mipmap = naga_agal::Mipmap::Disable;
                    }
                }
                Context3DCommand::SetScissorRectangle { rect } => self.scissor = rect,
                Context3DCommand::SetStencilActions {
                    triangle_face,
                    compare_mode,
                    on_both_pass,
                    on_depth_fail,
                    on_depth_pass_stencil_fail,
                } => {
                    let s = Stencil {
                        compare: compare(compare_mode),
                        pass: action(on_both_pass),
                        depth_fail: action(on_depth_fail),
                        fail: action(on_depth_pass_stencil_fail),
                    };
                    match triangle_face {
                        Context3DTriangleFace::Front => self.stencil[0] = s,
                        Context3DTriangleFace::Back => self.stencil[1] = s,
                        _ => self.stencil = [s; 2],
                    }
                }
                Context3DCommand::SetStencilReferenceValue {
                    reference_value,
                    read_mask,
                    write_mask,
                } => {
                    self.reference = reference_value;
                    self.read_mask = read_mask;
                    self.write_mask = write_mask;
                }
            }
        }
    }
    fn present(&mut self) {
        let Some(back) = self.back.as_ref() else {
            return;
        };
        back.resolve(&self.gl);
        unsafe {
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.bind_framebuffer(
                glow::READ_FRAMEBUFFER,
                Some(back.resolve.unwrap_or(back.fb)),
            );
            let front = (&*self.front.0 as &dyn std::any::Any)
                .downcast_ref::<super::Texture>()
                .unwrap();
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(front.id));
            self.gl.copy_tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                0,
                0,
                back.w as i32,
                back.h as i32,
            );
        }
        self.visible = true;
        self.cleared = false;
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed) {
            unsafe {
                if let Some(back) = self.back.take() {
                    back.dispose(&self.gl);
                }
                for target in self.targets.drain(..) {
                    target.dispose(&self.gl);
                }
                for sampler in self.samplers {
                    self.gl.delete_sampler(sampler);
                }
                for buffer in self.constants {
                    self.gl.delete_buffer(buffer);
                }
                self.gl.delete_vertex_array(self.vao);
            }
        }
    }
}
