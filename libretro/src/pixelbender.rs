use glow::HasContext;
use ruffle_render::{error::Error, pixel_bender::*, pixel_bender_support::*};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub struct Shader {
    pub program: crate::glsl::Program,
    pub shader: PixelBenderShader,
    pub floats: glow::Buffer,
    pub ints: glow::Buffer,
    pub mode: glow::Buffer,
    pub samplers: [glow::Sampler; 3],
    pub vao: glow::VertexArray,
    pub vb: glow::Buffer,
    pub float_size: usize,
    pub int_size: usize,
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
}
impl std::fmt::Debug for Shader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenGLPixelBender")
            .field("name", &self.shader.name)
            .finish()
    }
}
impl PixelBenderShaderImpl for Shader {
    fn parsed_shader(&self) -> &PixelBenderShader {
        &self.shader
    }
}
impl Drop for Shader {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed) {
            unsafe {
                for b in [self.floats, self.ints, self.mode, self.vb] {
                    self.gl.delete_buffer(b);
                }
                for s in self.samplers {
                    self.gl.delete_sampler(s);
                }
                self.gl.delete_vertex_array(self.vao);
            }
        }
    }
}
pub fn error(s: impl Into<String>) -> Error {
    Error::Unimplemented(s.into().into())
}
pub fn compile(
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
    shader: PixelBenderShader,
) -> Result<PixelBenderShaderHandle, Error> {
    unsafe {
        if !shader
            .output_channels()
            .is_some_and(|n| (1..=4).contains(&n))
        {
            return Err(error("PixelBender output must have 1-4 channels"));
        }
        let modules =
            naga_pixelbender::ShaderBuilder::build(&shader).map_err(|e| error(e.to_string()))?;
        let program = crate::glsl::compile(
            gl.clone(),
            alive.clone(),
            &modules.vertex,
            naga_pixelbender::VERTEX_SHADER_ENTRYPOINT,
            &modules.fragment,
            naga_pixelbender::FRAGMENT_SHADER_ENTRYPOINT,
        )
        .map_err(error)?;
        let float_size = (modules.float_parameters_buffer_size as usize).max(16);
        let int_size = (modules.int_parameters_buffer_size as usize).max(16);
        let create = |size: usize| -> Result<glow::Buffer, Error> {
            let b = gl.create_buffer().map_err(error)?;
            gl.bind_buffer(glow::UNIFORM_BUFFER, Some(b));
            gl.buffer_data_u8_slice(glow::UNIFORM_BUFFER, &vec![0; size], glow::DYNAMIC_DRAW);
            Ok(b)
        };
        let floats = create(float_size)?;
        let ints = create(int_size)?;
        let mode = create(16)?;
        let samplers = [
            gl.create_sampler().map_err(error)?,
            gl.create_sampler().map_err(error)?,
            gl.create_sampler().map_err(error)?,
        ];
        for (i, s) in samplers.iter().enumerate() {
            let filter = if i == 0 { glow::NEAREST } else { glow::LINEAR };
            gl.sampler_parameter_i32(*s, glow::TEXTURE_MIN_FILTER, filter as i32);
            gl.sampler_parameter_i32(*s, glow::TEXTURE_MAG_FILTER, filter as i32);
            gl.sampler_parameter_i32(*s, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.sampler_parameter_i32(*s, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        }
        let vao = gl.create_vertex_array().map_err(error)?;
        let vb = gl.create_buffer().map_err(error)?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vb));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(&[
                0f32, 0., 0., 0., 1., 0., 1., 0., 0., 1., 0., 1., 1., 1., 1., 1.,
            ]),
            glow::STATIC_DRAW,
        );
        for (i, offset) in [(0, 0), (1, 8)] {
            gl.enable_vertex_attrib_array(i);
            gl.vertex_attrib_pointer_f32(i, 2, glow::FLOAT, false, 16, offset);
        }
        Ok(PixelBenderShaderHandle(Arc::new(Shader {
            program,
            shader,
            floats,
            ints,
            mode,
            samplers,
            vao,
            vb,
            float_size,
            int_size,
            gl,
            alive,
        })))
    }
}
pub fn parameters(
    shader: &Shader,
    args: &[PixelBenderShaderArgument],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut floats = vec![];
    let mut ints = vec![];
    for (index, param) in shader.shader.params.iter().enumerate() {
        let PixelBenderParam::Normal {
            qualifier: PixelBenderParamQualifier::Input,
            name,
            param_type,
            ..
        } = param
        else {
            continue;
        };
        if name == OUT_COORD_NAME {
            continue;
        }
        let value = args.iter().find_map(|arg| match arg {
            PixelBenderShaderArgument::ValueInput { index: i, value } if *i as usize == index => {
                Some(value)
            }
            _ => None,
        });
        if let Some(value) = value {
            pack(value, &mut floats, &mut ints)?;
        } else {
            let count = match param_type {
                PixelBenderTypeOpcode::TFloat3x3 => 3,
                PixelBenderTypeOpcode::TFloat4x4 => 4,
                _ => 1,
            };
            let target = if matches!(
                param_type,
                PixelBenderTypeOpcode::TInt
                    | PixelBenderTypeOpcode::TInt2
                    | PixelBenderTypeOpcode::TInt3
                    | PixelBenderTypeOpcode::TInt4
                    | PixelBenderTypeOpcode::TBool
                    | PixelBenderTypeOpcode::TBool2
                    | PixelBenderTypeOpcode::TBool3
                    | PixelBenderTypeOpcode::TBool4
            ) {
                &mut ints
            } else {
                &mut floats
            };
            target.resize(target.len() + count * 16, 0);
        }
    }
    if floats.len() > shader.float_size || ints.len() > shader.int_size {
        return Err(error("PixelBender parameter layout mismatch"));
    }
    floats.resize(shader.float_size, 0);
    ints.resize(shader.int_size, 0);
    Ok((floats, ints))
}
fn pack(value: &PixelBenderType, floats: &mut Vec<u8>, ints: &mut Vec<u8>) -> Result<(), Error> {
    use PixelBenderType::*;
    let mut f = vec![];
    let mut i = vec![];
    match value {
        TFloat(a) => f.extend([*a, 0., 0., 0.]),
        TFloat2(a, b) => f.extend([*a, *b, 0., 0.]),
        TFloat3(a, b, c) => f.extend([*a, *b, *c, 0.]),
        TFloat4(a, b, c, d) => f.extend([*a, *b, *c, *d]),
        TFloat2x2(a) => f.extend(a),
        TFloat3x3(a) => {
            for c in a.as_chunks::<3>().0 {
                f.extend([c[0], c[1], c[2], 0.]);
            }
        }
        TFloat4x4(a) => f.extend(a),
        TInt(a) | TBool(a) => i.extend([*a as i32, 0, 0, 0]),
        TInt2(a, b) | TBool2(a, b) => i.extend([*a as i32, *b as i32, 0, 0]),
        TInt3(a, b, c) | TBool3(a, b, c) => i.extend([*a as i32, *b as i32, *c as i32, 0]),
        TInt4(a, b, c, d) | TBool4(a, b, c, d) => {
            i.extend([*a as i32, *b as i32, *c as i32, *d as i32])
        }
        TString(_) => return Err(error("string shader parameters are not executable")),
    }
    floats.extend_from_slice(bytemuck::cast_slice(&f));
    ints.extend_from_slice(bytemuck::cast_slice(&i));
    Ok(())
}
