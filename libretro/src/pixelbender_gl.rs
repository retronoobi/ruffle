use super::*;
use crate::pixelbender::{Shader, error};
use ruffle_render::pixel_bender_support::ImageInputTexture;

impl GlRenderer {
    pub(super) fn shader_filter(
        &mut self,
        source: BitmapHandle,
        point: (u32, u32),
        size: (u32, u32),
        dest: BitmapHandle,
        at: (i32, i32),
        filter: &ruffle_render::filters::ShaderFilter,
    ) -> Result<Box<dyn SyncHandle>, Error> {
        let src = (&*source.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .ok_or(Error::UnknownType)?;
        let input = if point == (0, 0) && size == (src.w, src.h) {
            source
        } else {
            let input = self.transient(size.0, size.1);
            let mut commands = CommandList::new();
            commands.render_bitmap(
                source,
                Transform::default(),
                false,
                PixelSnapping::Never,
                PixelRegion {
                    x_min: point.0,
                    y_min: point.1,
                    x_max: point.0 + size.0,
                    y_max: point.1 + size.1,
                },
            );
            self.render_texture(&input, commands, Color::TRANSPARENT, false);
            input
        };
        let mut args = filter.shader_args.clone();
        for arg in &mut args {
            if let PixelBenderShaderArgument::ImageInput { texture, .. } = arg {
                *texture = Some(ImageInputTexture::Bitmap(input.clone()));
                break;
            }
        }
        let output = self.transient(size.0, size.1);
        self.run_pb(
            filter.shader.clone(),
            &args,
            &PixelBenderTarget::Bitmap(output.clone()),
            true,
        )?;
        self.copy_bitmap_region(&output, &dest, at)?;
        Ok(Box::new(TextureReadback { handle: dest }))
    }
    fn copy_bitmap_region(
        &self,
        source: &BitmapHandle,
        dest: &BitmapHandle,
        at: (i32, i32),
    ) -> Result<(), Error> {
        unsafe {
            let s = (&*source.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .ok_or(Error::UnknownType)?;
            let d = (&*dest.0 as &dyn std::any::Any)
                .downcast_ref::<Texture>()
                .ok_or(Error::UnknownType)?;
            let sx = at.0.min(0).unsigned_abs();
            let sy = at.1.min(0).unsigned_abs();
            let dx = at.0.max(0) as u32;
            let dy = at.1.max(0) as u32;
            let w = s.w.saturating_sub(sx).min(d.w.saturating_sub(dx));
            let h = s.h.saturating_sub(sy).min(d.h.saturating_sub(dy));
            if w == 0 || h == 0 {
                return Ok(());
            }
            self.gl
                .bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.copy_fb));
            self.gl.framebuffer_texture_2d(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(s.id),
                0,
            );
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(d.id));
            self.gl.copy_tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                dx as i32,
                dy as i32,
                sx as i32,
                sy as i32,
                w as i32,
                h as i32,
            );
            self.restore_target();
            Ok(())
        }
    }
    pub(super) fn blend_pixelbender(
        &mut self,
        commands: CommandList,
        shader: PixelBenderShaderHandle,
    ) {
        let source = self.group(commands);
        let (w, h) = (self.dimensions.width, self.dimensions.height);
        let background = self.transient(w, h);
        let t = (&*background.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .unwrap();
        self.copy_target_to(t);
        let output = self.transient(w, h);
        let args = [
            PixelBenderShaderArgument::ImageInput {
                index: 0,
                channels: 4,
                name: "background".into(),
                texture: Some(background.into()),
            },
            PixelBenderShaderArgument::ImageInput {
                index: 1,
                channels: 4,
                name: "foreground".into(),
                texture: Some(source.clone().into()),
            },
        ];
        let result = self.run_pb(
            shader,
            &args,
            &PixelBenderTarget::Bitmap(output.clone()),
            true,
        );
        let image = if let Err(error) = result {
            crate::log(format!("PixelBender blend: {error}"));
            source
        } else {
            output
        };
        self.render_bitmap(
            image,
            Transform::default(),
            false,
            PixelSnapping::Never,
            PixelRegion::for_whole_size(w, h),
        );
    }
    fn float_image(
        &self,
        w: u32,
        h: u32,
        channels: u32,
        data: Option<&[f32]>,
    ) -> Result<BitmapHandle, Error> {
        unsafe {
            if w == 0
                || h == 0
                || w > self.gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32
                || h > self.gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32
            {
                return Err(Error::TooLarge);
            }
            let (internal, format, storage_channels) = match channels {
                1 => (glow::R32F, glow::RED, 1),
                2 => (glow::RG32F, glow::RG, 2),
                3 | 4 => (glow::RGBA32F, glow::RGBA, 4),
                _ => return Err(error("invalid floating-point image channel count")),
            };
            let expected = w as usize * h as usize * storage_channels;
            if data.is_some_and(|b| b.len() != expected) {
                return Err(error(
                    "PixelBender image data length does not match dimensions",
                ));
            }
            let id = self.gl.create_texture().map_err(error)?;
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(id));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                internal as i32,
                w as i32,
                h as i32,
                0,
                format,
                glow::FLOAT,
                glow::PixelUnpackData::Slice(data.map(bytemuck::cast_slice)),
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            Ok(BitmapHandle(Arc::new(Texture {
                pool: None,
                id,
                w,
                h,
                gl: self.gl.clone(),
                alive: self.alive.clone(),
                sampling: std::cell::Cell::new((glow::NEAREST, glow::CLAMP_TO_EDGE)),
            })))
        }
    }
    pub(super) fn run_pb(
        &mut self,
        handle: PixelBenderShaderHandle,
        args: &[PixelBenderShaderArgument],
        target: &PixelBenderTarget,
        filter: bool,
    ) -> Result<PixelBenderOutput, Error> {
        let shader = (&*handle.0 as &dyn std::any::Any)
            .downcast_ref::<Shader>()
            .ok_or_else(|| error("not an OpenGL PixelBender shader"))?;
        let channels = shader
            .shader
            .output_channels()
            .ok_or_else(|| error("missing output channels"))?;
        let (w, h) = match target {
            PixelBenderTarget::Bitmap(h) => {
                let t = (&*h.0 as &dyn std::any::Any)
                    .downcast_ref::<Texture>()
                    .ok_or(Error::UnknownType)?;
                (t.w, t.h)
            }
            PixelBenderTarget::Bytes { width, height } => (*width, *height),
        };
        if w == 0 || h == 0 {
            return Err(Error::TooLarge);
        }
        let output = match target {
            PixelBenderTarget::Bitmap(_) => self.transient(w, h),
            PixelBenderTarget::Bytes { .. } => self.float_image(w, h, 4, None)?,
        };
        let output_texture = (&*output.0 as &dyn std::any::Any)
            .downcast_ref::<Texture>()
            .ok_or(Error::UnknownType)?;
        let mut owned = vec![];
        let mut images = std::collections::HashMap::new();
        for arg in args {
            if let PixelBenderShaderArgument::ImageInput { index, texture, .. } = arg {
                let id = match texture {
                    Some(ImageInputTexture::Bitmap(h)) => {
                        (&*h.0 as &dyn std::any::Any)
                            .downcast_ref::<Texture>()
                            .ok_or(Error::UnknownType)?
                            .id
                    }
                    Some(ImageInputTexture::TextureRef(t)) => {
                        (*t as &dyn std::any::Any)
                            .downcast_ref::<Texture>()
                            .ok_or(Error::UnknownType)?
                            .id
                    }
                    Some(ImageInputTexture::Floats {
                        width,
                        height,
                        data,
                    }) => {
                        let data_bytes = data.padded_data();
                        let h = self.float_image(
                            *width,
                            *height,
                            data.channel_count(),
                            Some(&data_bytes),
                        )?;
                        let id = (&*h.0 as &dyn std::any::Any)
                            .downcast_ref::<Texture>()
                            .ok_or(Error::UnknownType)?
                            .id;
                        owned.push(h);
                        id
                    }
                    None => return Err(error("PixelBender image input is missing")),
                };
                images.insert(*index as u32, id);
            }
        }
        let (floats, ints) = crate::pixelbender::parameters(shader, args)?;
        let (target_index, fb) = self.acquire_target(w, h);
        let result = (|| unsafe {
            let g = &self.gl;
            g.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            g.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(output_texture.id),
                0,
            );
            g.viewport(0, 0, w as i32, h as i32);
            g.disable(glow::SCISSOR_TEST);
            g.disable(glow::STENCIL_TEST);
            g.disable(glow::DEPTH_TEST);
            g.disable(glow::CULL_FACE);
            g.disable(glow::BLEND);
            g.color_mask(true, true, true, true);
            g.use_program(Some(shader.program.id));
            for (binding, buffer, bytes) in [
                (3, shader.floats, floats.as_slice()),
                (4, shader.ints, ints.as_slice()),
            ] {
                g.bind_buffer(glow::UNIFORM_BUFFER, Some(buffer));
                g.buffer_sub_data_u8_slice(glow::UNIFORM_BUFFER, 0, bytes);
                g.bind_buffer_base(glow::UNIFORM_BUFFER, binding, Some(buffer));
            }
            let mode = [if filter { 1f32 } else { 0. }; 4];
            g.bind_buffer(glow::UNIFORM_BUFFER, Some(shader.mode));
            g.buffer_sub_data_u8_slice(glow::UNIFORM_BUFFER, 0, bytemuck::cast_slice(&mode));
            g.bind_buffer_base(glow::UNIFORM_BUFFER, 5, Some(shader.mode));
            for binding in &shader.program.textures {
                let index = binding
                    .texture
                    .checked_sub(naga_pixelbender::TEXTURE_START_BIND_INDEX)
                    .ok_or_else(|| error("invalid shader texture binding"))?;
                let image = images
                    .get(&index)
                    .ok_or_else(|| error(format!("missing PixelBender image {index}")))?;
                g.active_texture(glow::TEXTURE0 + binding.unit);
                g.bind_texture(glow::TEXTURE_2D, Some(*image));
                g.bind_sampler(
                    binding.unit,
                    Some(shader.samplers[binding.sampler.unwrap_or(0).min(2) as usize]),
                );
                g.uniform_1_i32(binding.location.as_ref(), binding.unit as i32);
            }
            g.bind_vertex_array(Some(shader.vao));
            g.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            match target {
                PixelBenderTarget::Bytes { .. } => {
                    let count = (w as usize)
                        .checked_mul(h as usize)
                        .and_then(|n| n.checked_mul(4))
                        .filter(|n| *n <= 128 * 1024 * 1024)
                        .ok_or(Error::TooLarge)?;
                    let mut values = vec![0f32; count];
                    g.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    g.read_pixels(
                        0,
                        0,
                        w as i32,
                        h as i32,
                        glow::RGBA,
                        glow::FLOAT,
                        glow::PixelPackData::Slice(Some(bytemuck::cast_slice_mut(&mut values))),
                    );
                    let result: Vec<f32> = values
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| p[..channels].iter().copied())
                        .collect();
                    Ok(PixelBenderOutput::Bytes(
                        bytemuck::cast_slice(&result).to_vec(),
                    ))
                }
                PixelBenderTarget::Bitmap(dest) => {
                    let dest_texture = (&*dest.0 as &dyn std::any::Any)
                        .downcast_ref::<Texture>()
                        .ok_or(Error::UnknownType)?;
                    g.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fb));
                    g.active_texture(glow::TEXTURE0);
                    g.bind_texture(glow::TEXTURE_2D, Some(dest_texture.id));
                    g.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, 0, 0, w as i32, h as i32);
                    Ok(PixelBenderOutput::Bitmap(Box::new(TextureReadback {
                        handle: dest.clone(),
                    })))
                }
            }
        })();
        unsafe {
            for binding in &shader.program.textures {
                self.gl.bind_sampler(binding.unit, None);
            }
        }
        self.targets[target_index].busy = false;
        self.restore_target();
        result
    }
}
