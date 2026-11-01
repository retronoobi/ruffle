//! Shared Naga -> desktop GLSL compiler. No graphics API other than OpenGL.
use glow::HasContext;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub struct TextureBinding {
    pub location: Option<glow::UniformLocation>,
    pub texture: u32,
    pub sampler: Option<u32>,
    pub unit: u32,
}
pub struct Program {
    pub id: glow::Program,
    pub textures: Vec<TextureBinding>,
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
}
impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenGLProgram")
            .field("id", &self.id)
            .finish()
    }
}
impl Drop for Program {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed) {
            unsafe {
                self.gl.delete_program(self.id);
            }
        }
    }
}
struct Stage {
    source: String,
    reflection: naga::back::glsl::ReflectionInfo,
}
fn stage(module: &naga::Module, kind: naga::ShaderStage, entry: &str) -> Result<Stage, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("Naga validation: {e:?}"))?;
    let options = naga::back::glsl::Options {
        version: naga::back::glsl::Version::Desktop(330),
        ..Default::default()
    };
    let pipeline = naga::back::glsl::PipelineOptions {
        shader_stage: kind,
        entry_point: entry.into(),
        multiview: None,
    };
    let mut source = String::new();
    let reflection = naga::back::glsl::Writer::new(
        &mut source,
        module,
        &info,
        &options,
        &pipeline,
        Default::default(),
    )
    .map_err(|e| e.to_string())?
    .write()
    .map_err(|e| e.to_string())?;
    Ok(Stage { source, reflection })
}
pub fn compile(
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
    vertex: &naga::Module,
    vertex_entry: &str,
    fragment: &naga::Module,
    fragment_entry: &str,
) -> Result<Program, String> {
    unsafe {
        let vs = stage(vertex, naga::ShaderStage::Vertex, vertex_entry)?;
        let fs = stage(fragment, naga::ShaderStage::Fragment, fragment_entry)?;
        let id = gl.create_program()?;
        for (kind, source) in [
            (glow::VERTEX_SHADER, &vs.source),
            (glow::FRAGMENT_SHADER, &fs.source),
        ] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let e = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                gl.delete_program(id);
                return Err(format!("GLSL: {e}\n{source}"));
            }
            gl.attach_shader(id, shader);
            gl.delete_shader(shader);
        }
        for (name, location) in &vs.reflection.varying {
            gl.bind_attrib_location(id, location.location, name);
        }
        gl.link_program(id);
        if !gl.get_program_link_status(id) {
            let e = gl.get_program_info_log(id);
            gl.delete_program(id);
            return Err(format!("GLSL link: {e}"));
        }
        let mut textures = vec![];
        for (module, reflection) in [(vertex, &vs.reflection), (fragment, &fs.reflection)] {
            for (handle, name) in &reflection.uniforms {
                if let Some(binding) = &module.global_variables[*handle].binding
                    && let Some(index) = gl.get_uniform_block_index(id, name)
                {
                    gl.uniform_block_binding(id, index, binding.binding);
                }
            }
            for (name, mapping) in &reflection.texture_mapping {
                let texture = module.global_variables[mapping.texture]
                    .binding
                    .as_ref()
                    .ok_or("texture binding missing")?
                    .binding;
                let sampler = mapping.sampler.and_then(|h| {
                    module.global_variables[h]
                        .binding
                        .as_ref()
                        .map(|b| b.binding)
                });
                let unit = textures.len() as u32;
                textures.push(TextureBinding {
                    location: gl.get_uniform_location(id, name),
                    texture,
                    sampler,
                    unit,
                });
            }
        }
        if textures.len() > gl.get_parameter_i32(glow::MAX_TEXTURE_IMAGE_UNITS) as usize {
            gl.delete_program(id);
            return Err("shader exceeds OpenGL texture-unit limit".into());
        }
        Ok(Program {
            id,
            textures,
            gl,
            alive,
        })
    }
}
