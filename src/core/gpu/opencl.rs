// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use ocl::*;
use ocl::core::{ ImageDescriptor, MemObjectType, GlTextureTarget };
use ocl::enums::ContextPropertyValue;
use parking_lot::RwLock;
use std::ops::DerefMut;
use super::*;
use crate::stabilization::{KernelParams, ComputeParams};

pub struct OclWrapper {
    kernel: Kernel,
    src: Buffer<u8>,
    dst: Buffer<u8>,

    // queue: Queue,

    image_src: Option<ocl::Image::<u8>>,
    image_dst: Option<ocl::Image::<u8>>,

    buf_params: Buffer<u8>,
    buf_drawing: Buffer<u8>,
    buf_matrices: Buffer<f32>,
}

pub struct CtxWrapper {
    pub device: Device,
    pub context: Context,
    pub platform: Platform,

    pub surface_checksum: u32
}

lazy_static::lazy_static! {
    static ref CONTEXT: RwLock<Option<CtxWrapper>> = RwLock::new(None);
}

const EXCLUSIONS: &[&'static str] = &["Microsoft Basic Render Driver"];

impl OclWrapper {
    fn get_properties(buffers: Option<&BufferDescription>) -> ocl::builders::ContextProperties {
        let mut props = ocl::builders::ContextProperties::new();
        if let Some(buffers) = buffers {
            match &buffers.buffers {
                BufferSource::OpenGL { context: _context, .. } => {
                    props = ocl_interop::get_properties_list();
                }
                BufferSource::DirectX { device, .. } => {
                    props.set_property_value(ContextPropertyValue::D3d11DeviceKhr(*device));
                },
                _ => { }
            }
        }
        props
    }

    pub fn list_devices() -> Vec<String> {
        let devices = std::panic::catch_unwind(|| -> Vec<String> {
            let mut ret = Vec::new();
            for p in Platform::list() {
                if let Ok(devs) = Device::list(p, Some(ocl::flags::DeviceType::new().gpu().accelerator())) {
                    ret.extend(devs.into_iter().filter_map(|x| Some(format!("{} {}: {}", p.name().ok()?, x.name().ok()?, x.version().ok()?))));
                }
            }
            ret.drain(..).filter(|x| !EXCLUSIONS.iter().any(|e| x.contains(e))).collect()
        });
        match devices {
            Ok(devices) => { return devices; },
            Err(e) => {
                if let Some(s) = e.downcast_ref::<&str>() {
                    log::error!("Failed to initialize OpenCL {}", s);
                } else if let Some(s) = e.downcast_ref::<String>() {
                    log::error!("Failed to initialize OpenCL {}", s);
                } else {
                    log::error!("Failed to initialize OpenCL {:?}", e);
                }
            }
        }
        Vec::new()
    }
    pub fn get_info() -> Option<String> {
        let lock = CONTEXT.read();
        if let Some(ref ctx) = *lock {
            ctx.device.name().ok()
        } else {
            None
        }
    }

    pub fn set_device(index: usize, buffers: &BufferDescription) -> ocl::Result<()> {
        let mut i = 0;
        for p in Platform::list() {
            if let Ok(devs) = Device::list(p, Some(ocl::flags::DeviceType::new().gpu().accelerator())) {
                for d in devs {
                    if EXCLUSIONS.iter().any(|x| d.name().unwrap_or_default().contains(x)) { continue; }
                    if i == index {
                        ::log::info!("OpenCL Platform: {}, Device: {} {}", p.name()?, d.vendor()?, d.name()?);

                        let context = Context::builder()
                            .properties(Self::get_properties(Some(buffers)))
                            .platform(p)
                            .devices(d)
                            .build()?;

                        *CONTEXT.write() = Some(CtxWrapper { device: d, context, platform: p, surface_checksum: buffers.buffers.get_checksum() });
                        return Ok(());
                    }
                    i += 1;
                }
            }
        }
        Err(ocl::BufferCmdError::MapUnavailable.into())
    }

    pub fn initialize_context(buffers: Option<&BufferDescription>) -> ocl::Result<(String, String)> {
        // List all devices
        Platform::list().iter().for_each(|p| {
            if let Ok(devs) = Device::list(p, Some(ocl::flags::DeviceType::new().gpu().accelerator())) {
                ::log::debug!("OpenCL devices: {:?} {:?} {:?}", p.name(), p.version(), devs.iter().filter_map(|x| x.name().ok()).collect::<Vec<String>>());
            }
        });

        let mut platform = None;
        let mut device = None;
        let preference = [ "nvidia", "radeon", "geforce", "firepro", "accelerated parallel processing", "graphics" ];
        'outer: for pref in preference {
            for p in Platform::list() {
                if let Ok(devs) = Device::list(p, Some(ocl::flags::DeviceType::new().gpu().accelerator())) {
                    for d in devs.iter() {
                        let name = format!("{} {}", p.name().unwrap_or_default(),  d.name().unwrap_or_default());
                        if name.to_ascii_lowercase().contains(pref) {
                            platform = Some(p);
                            device = Some(*d);
                            break 'outer;
                        }
                    }
                }
            }
        }
        if device.is_none() {
            // Try first GPU
            'outer2: for p in Platform::list() {
                if let Ok(devs) = Device::list(p, Some(ocl::flags::DeviceType::new().gpu().accelerator())) {
                    for d in devs.iter() {
                        if let Ok(ocl::core::DeviceInfoResult::Type(typ)) = d.info(ocl::core::DeviceInfo::Type) {
                            if typ == ocl::DeviceType::GPU {
                                platform = Some(p);
                                device = Some(*d);
                                break 'outer2;
                            }
                        }
                    }
                }
            }
        }
        if device.is_none() { return Err(ocl::BufferCmdError::MapUnavailable.into()); }
        let platform = platform.unwrap();
        let device = device.unwrap();
        ::log::info!("OpenCL Platform: {}, ext: {:?} Device: {} {}", platform.name()?, platform.extensions()?, device.vendor()?, device.name()?);

        let context = Context::builder()
            .properties(Self::get_properties(buffers))
            .platform(platform)
            .devices(device)
            .build()?;

        let name = format!("{} {}", device.vendor()?, device.name()?);
        let list_name = format!("[OpenCL] {} {}", platform.name()?, device.name()?);

        *CONTEXT.write() = Some(CtxWrapper { device, context, platform, surface_checksum: buffers.map(|x| x.buffers.get_checksum()).unwrap_or_default() });

        Ok((name, list_name))
    }

    pub fn new(params: &KernelParams, ocl_names: (&str, &str, &str, &str), compute_params: &ComputeParams, buffers: &BufferDescription, drawing_len: usize) -> ocl::Result<Self> {
        if params.height < 4 || params.output_height < 4 || params.stride < 1 { return Err(ocl::BufferCmdError::AlreadyMapped.into()); }

        let mut kernel = include_str!("opencl_undistort.cl").to_string();
        // let mut kernel = std::fs::read_to_string("D:/programowanie/projekty/Rust/gyroflow/src/core/gpu/opencl_undistort.cl").unwrap();

        let mut lens_model_functions = compute_params.distortion_model.opencl_functions().to_string();
        let default_digital_lens = "float2 digital_undistort_point(float2 uv, __global KernelParams *p) { return uv; }
                                        float2 digital_distort_point(float2 uv, __global KernelParams *p) { return uv; }";
        lens_model_functions.push_str(compute_params.digital_lens.as_ref().map(|x| x.opencl_functions()).unwrap_or(default_digital_lens));

        kernel = kernel.replace("LENS_MODEL_FUNCTIONS;", &lens_model_functions)
                       .replace("DATA_CONVERTF", ocl_names.3)
                       .replace("DATA_TYPEF", ocl_names.2)
                       .replace("DATA_CONVERT", ocl_names.1)
                       .replace("DATA_TYPE", ocl_names.0)
                       .replace("PIXEL_BYTES", &format!("{}", params.bytes_per_pixel))
                       .replace("INTERPOLATION", &format!("{}", params.interpolation));
        let mut image_src = None;
        let mut image_dst = None;

        {
            let ctx = CONTEXT.read();
            let context_initialized = ctx.is_some();
            if !context_initialized || ctx.as_ref().unwrap().surface_checksum != buffers.buffers.get_checksum() {
                drop(ctx);
                Self::initialize_context(Some(buffers))?;
            }
        }
        let mut lock = CONTEXT.write();
        if let Some(ref mut ctx) = *lock {
            let mut ocl_queue = Queue::new(&ctx.context, ctx.device, None)?;

            let in_desc  = ImageDescriptor::new(MemObjectType::Image2d, buffers.input_size.0,  buffers.input_size.1,  1, 1, buffers.input_size.2,  0, None);
            let out_desc = ImageDescriptor::new(MemObjectType::Image2d, buffers.output_size.0, buffers.output_size.1, 1, 1, buffers.output_size.2, 0, None);

            let (source_buffer, dest_buffer) =
                match &buffers.buffers {
                    BufferSource::Cpu { input, output } => {
                        (
                            Buffer::builder().queue(ocl_queue.clone()).len(input.len()).flags(MemFlags::new().read_only().host_write_only()).build()?,
                            Buffer::builder().queue(ocl_queue.clone()).len(output.len()).flags(MemFlags::new().write_only().host_read_only().alloc_host_ptr()).build()?
                        )
                    },
                    BufferSource::OpenCL { queue, .. } => {
                        if !queue.is_null() {
                            let queue_core = unsafe { core::CommandQueue::from_raw_copied_ptr(*queue) };
                            let device_core = queue_core.device()?;
                            let context_core = queue_core.context()?;
                            *ctx.device.deref_mut() = device_core;
                            *ctx.context.deref_mut() = context_core;

                            ocl_queue = Queue::new(&ctx.context, ctx.device, None)?;
                            *ocl_queue.deref_mut() = queue_core;
                        }

                        (
                            Buffer::builder().queue(ocl_queue.clone()).len(buffers.input_size.1 * buffers.input_size.2).flags(MemFlags::new().read_only().host_no_access()).build()?,
                            Buffer::builder().queue(ocl_queue.clone()).len(buffers.output_size.1 * buffers.output_size.2).flags(MemFlags::new().read_write().host_no_access()).build()?
                        )
                    },
                    BufferSource::OpenGL { input, output, .. } => {
                        image_src = Some(Image::from_gl_texture(ocl_queue.clone(), MemFlags::new().read_only(), in_desc, GlTextureTarget::GlTexture2d, 0, *input)?);
                        image_dst = Some(Image::from_gl_texture(ocl_queue.clone(), MemFlags::new().write_only(), out_desc, GlTextureTarget::GlTexture2d, 0, *output)?);

                        (
                            Buffer::builder().queue(ocl_queue.clone()).len(buffers.input_size.1 * buffers.input_size.2).flags(MemFlags::new().read_only().host_no_access()).build()?,
                            Buffer::builder().queue(ocl_queue.clone()).len(buffers.output_size.1 * buffers.output_size.2).flags(MemFlags::new().read_write().host_no_access()).build()?
                        )
                    },
                    BufferSource::DirectX { input, output, .. } => {
                        if input == output {
                            let img = Image::from_d3d11_texture2d(ocl_queue.clone(), MemFlags::new().read_write(), in_desc, *input, 0)?;
                            image_src = Some(img.clone());
                            image_dst = Some(img.clone());
                        } else {
                            image_src = Some(Image::from_d3d11_texture2d(ocl_queue.clone(), MemFlags::new().read_only(), in_desc, *input, 0)?);
                            image_dst = Some(Image::from_d3d11_texture2d(ocl_queue.clone(), MemFlags::new().write_only(), out_desc, *output, 0)?);
                        }

                        (
                            Buffer::builder().queue(ocl_queue.clone()).len(image_src.as_ref().unwrap().pixel_count() * params.bytes_per_pixel as usize).flags(MemFlags::new().read_only().host_no_access()).build()?,
                            Buffer::builder().queue(ocl_queue.clone()).len(image_dst.as_ref().unwrap().pixel_count() * params.bytes_per_pixel as usize).flags(MemFlags::new().read_write().host_no_access()).build()?
                        )
                    }
                };

            let program = Program::builder()
                .src(&kernel)
                .devices(ctx.device)
                .build(&ctx.context)?;

            let max_matrix_count = 9 * params.height;
            let flags = MemFlags::new().read_only().host_write_only();

            let buf_params   = Buffer::builder().queue(ocl_queue.clone()).flags(flags).len(std::mem::size_of::<KernelParams>()).build()?;
            let buf_drawing  = Buffer::builder().queue(ocl_queue.clone()).flags(flags).len(drawing_len).build()?;
            let buf_matrices = Buffer::builder().queue(ocl_queue.clone()).flags(flags).len(max_matrix_count).build()?;

            let mut builder = Kernel::builder();
            unsafe {
                builder.program(&program).name("undistort_image").queue(ocl_queue.clone())
                    .global_work_size((buffers.output_size.0, buffers.output_size.1))
                    .disable_arg_type_check()
                    .arg(&source_buffer)
                    .arg(&dest_buffer)
                    .arg(&buf_params)
                    .arg(&buf_matrices)
                    .arg(&buf_drawing);
            }

            let kernel = builder.build()?;

            Ok(Self {
                kernel,
                // queue: ocl_queue,
                src: source_buffer,
                dst: dest_buffer,
                image_src,
                image_dst,
                buf_params,
                buf_drawing,
                buf_matrices,
            })
        } else {
            Err(ocl::BufferCmdError::AlreadyMapped.into())
        }
    }

    pub fn undistort_image(&self, buffers: &mut BufferDescription, itm: &crate::stabilization::FrameTransform, drawing_buffer: &[u8]) -> ocl::Result<()> {
        let matrices = unsafe { std::slice::from_raw_parts(itm.matrices.as_ptr() as *const f32, itm.matrices.len() * 9 ) };

        if self.buf_matrices.len() < matrices.len() { log::error!("Buffer size mismatch matrices! {} vs {}", self.buf_matrices.len(), matrices.len()); return Ok(()); }

        if let Some(ref tex) = self.image_src {
            let len = tex.pixel_count() * itm.kernel_params.bytes_per_pixel as usize;
            if len != self.src.len() { log::error!("Buffer size mismatch image_src! {} vs {}", self.src.len(), len);  return Ok(()); }
        }
        if let Some(ref tex) = self.image_dst {
            let len = tex.pixel_count() * itm.kernel_params.bytes_per_pixel as usize;
            if len != self.dst.len() { log::error!("Buffer size mismatch image_dst! {} vs {}", self.dst.len(), len);  return Ok(()); }
        }

        if !drawing_buffer.is_empty() {
            if self.buf_drawing.len() != drawing_buffer.len() { log::error!("Buffer size mismatch drawing_buffer! {} vs {}", self.buf_drawing.len(), drawing_buffer.len()); return Ok(()); }
            self.buf_drawing.write(drawing_buffer).enq()?;
        }
        match buffers.buffers {
            BufferSource::Cpu { ref input, ref output } => {
                if self.src.len() != input.len()  { log::error!("Buffer size mismatch input! {} vs {}", self.src.len(), input.len());  return Ok(()); }
                if self.dst.len() != output.len() { log::error!("Buffer size mismatch output! {} vs {}", self.dst.len(), output.len()); return Ok(()); }

                self.src.write(input as &[u8]).enq()?;
            },
            BufferSource::OpenCL { input, output, .. } => {
                unsafe {
                    let siz = std::mem::size_of::<ocl::ffi::cl_mem>() as usize;
                    self.kernel.set_arg_unchecked(0, core::ArgVal::from_raw(siz, &input as *const _ as *const std::ffi::c_void, true))?;
                    self.kernel.set_arg_unchecked(1, core::ArgVal::from_raw(siz, &output as *const _ as *const std::ffi::c_void, true))?;
                }
            },
            BufferSource::OpenGL { .. } => {
                if let Some(ref tex) = self.image_src {
                    tex.cmd().gl_acquire().enq()?;
                    let _ = tex.cmd().copy_to_buffer(&self.src, 0).enq();
                    tex.cmd().gl_release().enq()?;
                }
            },
            BufferSource::DirectX { .. } => {
                if let Some(ref tex) = self.image_src {
                    tex.cmd().d3d11_acquire().enq()?;
                    let _ = tex.cmd().copy_to_buffer(&self.src, 0).enq();
                    tex.cmd().d3d11_release().enq()?;
                }
            }
        }

        self.buf_params.write(bytemuck::bytes_of(&itm.kernel_params)).enq()?;
        self.buf_matrices.write(matrices).enq()?;

        unsafe { self.kernel.enq()?; }

        match &mut buffers.buffers {
            BufferSource::Cpu { output, .. } => {
                self.dst.read(&mut **output).enq()?;
            },
            BufferSource::OpenGL { .. } => {
                if let Some(ref tex) = self.image_dst {
                    tex.cmd().gl_acquire().enq()?;
                    if let SpatialDims::Three(w, h, d) = tex.dims() {
                        let _ = self.dst.cmd().copy_to_image(&tex, [0, 0, 0], [*w, *h, *d]).enq();
                    }
                    tex.cmd().gl_release().enq()?;
                }
            },
            BufferSource::DirectX { .. } => {
                if let Some(ref tex) = self.image_dst {
                    tex.cmd().d3d11_acquire().enq()?;
                    if let SpatialDims::Three(w, h, d) = tex.dims() {
                        let _ = self.dst.cmd().copy_to_image(&tex, [0, 0, 0], [*w, *h, *d]).enq();
                    }
                    tex.cmd().d3d11_release().enq()?;
                }
            }
            _ => { }
        }

        // self.queue.finish();

        Ok(())
    }
}

pub fn is_buffer_supported(buffers: &BufferDescription) -> bool {
    match buffers.buffers {
        BufferSource::Cpu     { .. } => true,
        BufferSource::OpenGL  { .. } => true,
        BufferSource::DirectX { .. } => cfg!(target_os = "windows"),
        BufferSource::OpenCL  { .. } => true,
    }
}
