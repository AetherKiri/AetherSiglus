//! Optional, retained Metal render targets supplied by the embedded host.
//! The host owns IOSurface allocation and consumer retirement; wgpu owns a
//! retained reference to each imported texture until clear(). No pixels cross
//! the FFI boundary during presentation. Explicit screenshots still read back.

use anyhow::{ensure, Context, Result};
use foreign_types::{ForeignType, ForeignTypeRef};
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{atomic::{AtomicBool, Ordering}, Arc},
};

#[derive(Debug, Default)]
pub struct SharedMetalPresenter {
    pub enabled: bool,
    active: Option<usize>,
    targets: HashMap<usize, SharedTarget>,
    pipeline: Option<wgpu::RenderPipeline>,
}

#[derive(Debug)]
struct SharedTarget {
    texture: wgpu::Texture,
    /// Set by wgpu's completion callback. A target is not selected again
    /// until its producer queue has finished writing it.
    ready: Arc<AtomicBool>,
}

impl SharedMetalPresenter {
    pub fn device_ptr(device: &wgpu::Device) -> *mut c_void {
        // The renderer keeps the borrowed MTLDevice alive for its whole lifetime.
        unsafe {
            device
                .as_hal::<wgpu::hal::api::Metal, _, _>(|hal| {
                    hal.map(|hal| hal.raw_device().lock().as_ptr().cast())
                        .unwrap_or(std::ptr::null_mut())
                })
                .unwrap_or(std::ptr::null_mut())
        }
    }

    /// `texture` must be a live, initialized BGRA8 Metal texture from this
    /// device. A null pointer parks presentation while all host buffers retire.
    pub unsafe fn select(
        &mut self,
        device: &wgpu::Device,
        texture: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<()> {
        // Progress completion callbacks without waiting for the GPU. The
        // three-target pool can continue rendering while an older target is
        // still consumed by Godot or finishing on the producer queue.
        device.poll(wgpu::Maintain::Poll);
        self.active = None;
        if texture.is_null() {
            self.enabled = true;
            return Ok(());
        }
        let native = metal::TextureRef::from_ptr(texture.cast());
        ensure!(
            native.width() == width as u64 && native.height() == height as u64,
            "shared Metal target dimensions do not match"
        );
        ensure!(
            native.pixel_format() == metal::MTLPixelFormat::BGRA8Unorm,
            "shared Metal target must be BGRA8Unorm"
        );
        ensure!(
            native.device().as_ptr().cast::<c_void>() == Self::device_ptr(device),
            "shared Metal target belongs to a different device"
        );
        let key = texture as usize;
        if !self.targets.contains_key(&key) {
            ensure!(
                self.targets.len() < 8,
                "clear shared targets before replacing the host pool"
            );
            let size = wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            };
            let hal = wgpu::hal::metal::Device::texture_from_raw(
                native.to_owned(),
                wgpu::TextureFormat::Bgra8Unorm,
                metal::MTLTextureType::D2,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
            );
            let imported = device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some("siglus-host-iosurface"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                },
            );
            self.targets.insert(key, SharedTarget {
                texture: imported,
                ready: Arc::new(AtomicBool::new(true)),
            });
        }
        self.enabled = true;
        self.active = Some(key);
        Ok(())
    }

    pub fn clear(&mut self, device: &wgpu::Device) {
        device.poll(wgpu::Maintain::Wait);
        *self = Self::default();
    }

    pub fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
    ) -> Result<()> {
        let Some(key) = self.active else {
            return Ok(());
        };
        let target = self
            .targets
            .get(&key)
            .context("shared target was released")?;
        if self.pipeline.is_none() {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("siglus-iosurface-present"),
                source: wgpu::ShaderSource::Wgsl(
                    r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(corners[id], 0.0, 1.0);
}
@fragment fn fs(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source, vec2<i32>(pixel.xy), 0);
}
"#
                    .into(),
                ),
            });
            self.pipeline = Some(
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("siglus-iosurface-present"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: "vs",
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: "fs",
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Bgra8Unorm,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    multiview: None,
                }),
            );
        }
        let pipeline = self.pipeline.as_ref().unwrap();
        let source_view = source.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("siglus-iosurface-present"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            }],
        });
        let view = target.texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("siglus-iosurface-present"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("siglus-iosurface-present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        // Do not wait for the producer queue here. SharedFrame keeps the
        // previous published target visible until this target's completion
        // callback fires; select() polls callbacks before choosing a target.
        // This removes the cross-API GPU wait from the story frame's critical
        // path while preserving the no-race publication rule.
        let ready = target.ready.clone();
        ready.store(false, Ordering::Release);
        queue.submit([encoder.finish()]);
        queue.on_submitted_work_done(move || ready.store(true, Ordering::Release));
        device.poll(wgpu::Maintain::Poll);
        Ok(())
    }

    /// Whether the target selected for the current frame has completed its
    /// producer submission. Zero means no target or still pending.
    pub fn target_ready(&self, texture: *mut c_void) -> bool {
        self.targets
            .get(&(texture as usize))
            .is_some_and(|target| target.ready.load(Ordering::Acquire))
    }
}
