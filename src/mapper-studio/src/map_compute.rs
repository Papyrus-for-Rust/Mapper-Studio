//! Optional GPU cartography. The map's generation data always stays unchanged.
use crate::{generation_resources::Limits, map_colors::Material};
use eframe::{
    egui::{Color32, ColorImage},
    wgpu,
};
use papyrus_terrain::native_layers::Layers;
use std::sync::Mutex;
use wgpu::util::DeviceExt;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    name: String,
}
static GPU: Mutex<Option<Result<Gpu, String>>> = Mutex::new(None);
type SharedDevice = (wgpu::Device, wgpu::Queue, String);
static SHARED: Mutex<Option<SharedDevice>> = Mutex::new(None);
/// Reuse the desktop renderer's device. Pipeline creation remains on the image
/// worker; registering does not submit work or block the UI on shader compilation.
pub fn register_renderer(state: &eframe::egui_wgpu::RenderState) {
    if state.adapter.get_info().device_type != wgpu::DeviceType::Cpu {
        *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = Some((
            state.device.clone(),
            state.queue.clone(),
            state.adapter.get_info().name,
        ));
    }
}
impl Gpu {
    fn new() -> Result<Self, String> {
        if let Some((device, queue, name)) = SHARED.lock().unwrap_or_else(|e| e.into_inner()).take()
        {
            return Self::from_device(device, queue, name);
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| format!("GPU adapter unavailable: {e}"))?;
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Err("No hardware compute adapter available".into());
        }
        let name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Mapper image compute"),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|e| format!("GPU compute device unavailable: {e}"))?;
        Self::from_device(device, queue, name)
    }
    fn from_device(device: wgpu::Device, queue: wgpu::Queue, name: String) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mapper cartography"),
            source: wgpu::ShaderSource::Wgsl(include_str!("map_compute.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Mapper cartography"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        Ok(Self {
            device,
            queue,
            pipeline,
            name,
        })
    }
}
/// Public for headless verification; callers must validate input dimensions.
/// GPU and image limits cover explicit buffers, excluding driver/viewport memory.
#[allow(clippy::too_many_arguments)]
pub fn render(
    layers: &Layers,
    ocean: Option<&[u8]>,
    water: Option<&[i16]>,
    output: usize,
    material: &Material,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_with_height(
        layers,
        &layers.height,
        ocean,
        water,
        output,
        material,
        limits,
        cancelled,
    )
}
/// Borrow the final accepted surface-height grid without copying terrain layers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_with_height(
    layers: &Layers,
    height: &[i16],
    ocean: Option<&[u8]>,
    water: Option<&[i16]>,
    output: usize,
    material: &Material,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    render_with_surface(
        layers, height, ocean, water, output, material, None, limits, cancelled,
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_with_surface(
    layers: &Layers,
    height: &[i16],
    ocean: Option<&[u8]>,
    water: Option<&[i16]>,
    output: usize,
    material: &Material,
    surface: Option<&crate::map_surface::Grid>,
    limits: Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<ColorImage>, String> {
    limits.validate()?;
    material.validate()?;
    let (hr, sr) = papyrus_terrain::native_layers::dimensions(layers.size)?;
    if !(2..=4096).contains(&output)
        || layers.height_resolution != hr
        || layers.surface_resolution != sr
        || layers.height.len() != hr * hr
        || height.len() != hr * hr
        || layers.splat.len() != 8 * sr * sr
        || height.iter().any(|&h| !(0..=32766).contains(&h))
        || ocean.is_some_and(|o| o.len() != sr * sr)
        || water.is_some_and(|w| w.len() != hr * hr || w.iter().any(|&h| !(0..=32766).contains(&h)))
    {
        return Err("Invalid GPU map inputs".into());
    }
    if surface.is_some_and(|g| {
        g.resolution() != (hr - 1) / 2
            || g.normal().is_some_and(|n| n.resolution() != g.resolution())
            || g.shore().is_some_and(|s| s.resolution() != g.resolution())
    }) {
        return Err("Invalid GPU coarse surface dimensions".into());
    }
    let words = 18
        + hr * hr
        + (8 * sr * sr).div_ceil(4)
        + water.map_or(0, |w| w.len())
        + ocean.map_or(0, |o| o.len().div_ceil(4))
        + surface.map_or(0, |g| g.pixels().len())
        + surface
            .and_then(|g| g.normal())
            .map_or(0, |n| n.all_pixels().len());
    let words = words
        + surface
            .and_then(|g| g.shore())
            .map_or(0, |s| s.texels().len());
    let input_bytes = words * 4;
    let output_bytes = output * output * 4;
    let material_bytes = 53 * 4;
    let gpu_bytes = input_bytes + output_bytes * 2 + material_bytes;
    // Conservative host allowance: packed upload, mapped copy, final pixels.
    let host_bytes = input_bytes * 2
        + output_bytes * 2
        + material_bytes
        + surface.map_or(0, |s| s.retained_bytes());
    if gpu_bytes > limits.gpu_memory_mib * 1024 * 1024
        || host_bytes > limits.image_memory_mib * 1024 * 1024
    {
        return Err("GPU image buffers exceed Settings memory budget".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if gpu.is_none() {
        *gpu = Some(Gpu::new());
    }
    let gpu = gpu.as_ref().unwrap().as_ref().map_err(Clone::clone)?;
    let maximum = gpu.device.limits().max_storage_buffer_binding_size as usize;
    if input_bytes > maximum || output_bytes > maximum {
        return Err("Image exceeds GPU adapter buffer limits".into());
    }
    if cancelled() {
        return Ok(None);
    }
    let mut input = Vec::with_capacity(words);
    input.extend([
        hr as u32,
        sr as u32,
        output as u32,
        layers.size,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    input.extend(height.iter().map(|&h| h as u32));
    input[4] = input.len() as u32;
    pack(&mut input, &layers.splat);
    if let Some(w) = water {
        input[5] = input.len() as u32;
        input[7] |= 2;
        input.extend(w.iter().map(|&h| h as u32));
    }
    if let Some(o) = ocean {
        input[6] = input.len() as u32;
        input[7] |= 1;
        pack(&mut input, o);
    }
    let mut normal_mip_blend = 0.;
    if let Some(grid) = surface {
        input[8] = input.len() as u32;
        input[9] = grid.resolution() as u32;
        input.extend(
            grid.pixels()
                .iter()
                .map(|p| u32::from(p[0]) | u32::from(p[1]) << 16),
        );
        if let Some(normal) = grid.normal() {
            input[10] = input.len() as u32;
            input[11] = normal.resolution() as u32;
            let (low, high, blend) =
                normal.mip_selection(normal.lod_for_footprint(1. / output as f32));
            input[12] = low.offset as u32;
            input[13] = low.resolution as u32;
            input[14] = high.offset as u32;
            input[15] = high.resolution as u32;
            normal_mip_blend = blend;
            input.extend(normal.all_pixels().iter().map(|p| u32::from_le_bytes(*p)));
        }
        if let Some(shore) = grid.shore() {
            input[16] = input.len() as u32;
            input[17] = shore.resolution() as u32;
            input.extend(
                shore
                    .texels()
                    .iter()
                    .map(|p| u32::from(p[2]) | u32::from(p[3]) << 16),
            );
        }
    }
    let mut colors: Vec<f32> = material.colors.iter().flatten().copied().collect();
    colors.extend(material.sun_direction);
    colors.extend([material.sun_power, material.brightness, material.contrast]);
    colors.push(if material.water_uses_contrast() {
        1.
    } else {
        0.
    });
    colors.push(if material.slope_uses_normal() { 1. } else { 0. });
    colors.push(normal_mip_blend);
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let oom = gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let buffer = |label, bytes: &[u8]| {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE,
            })
    };
    let source = buffer("Map source grids", bytemuck::cast_slice(&input));
    let palette = buffer("Source PaperMap material", bytemuck::cast_slice(&colors));
    let output_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Map pixels"),
        size: output_bytes as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Map readback"),
        size: output_bytes as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bindings = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Map compute inputs"),
        layout: &gpu.pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: source.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: palette.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output_buffer.as_entire_binding(),
            },
        ],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Cartography"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups((output as u32).div_ceil(8), (output as u32).div_ceil(8), 1);
    }
    encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging, 0, output_bytes as u64);
    if let Some(e) = pollster::block_on(oom.pop()) {
        return Err(e.to_string());
    }
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(e.to_string());
    }
    if cancelled() {
        return Ok(None);
    }
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    // A bounded wait; submitted GPU dispatches cannot be forcibly rolled back.
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(1))
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if cancelled() {
        staging.unmap();
        return Ok(None);
    }
    let pixels = staging
        .slice(..)
        .get_mapped_range()
        .map_err(|e| e.to_string())?
        .chunks_exact(4)
        .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
        .collect();
    staging.unmap();
    crate::generation_resources::report(format!(
        "GPU map images · {} · {} MiB buffers",
        gpu.name,
        gpu_bytes.div_ceil(1024 * 1024)
    ));
    Ok(Some(ColorImage::new([output, output], pixels)))
}
fn pack(dst: &mut Vec<u32>, bytes: &[u8]) {
    dst.extend(bytes.chunks(4).map(|chunk| {
        let mut b = [0; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        u32::from_le_bytes(b)
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a hardware compute adapter"]
    fn raised_surface_upload_matches_cpu_water_and_lighting() {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![encode(10.); hr * hr],
            biome: vec![0; 5 * cells],
            splat: vec![0; 8 * cells],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.splat[7 * cells..].fill(255);
        let original = layers.height.clone();
        let mut surface = original.clone();
        for (z, row) in surface.chunks_mut(hr).enumerate() {
            row[hr / 3..2 * hr / 3].fill(encode(20. + z as f32 / hr as f32 * 15.));
        }
        let water = vec![encode(15.); hr * hr];
        let mut material = crate::map_colors::tests::fixture();
        material.shader_blob_sha256 =
            Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07".into());
        let actual = render_with_height(
            &layers,
            &surface,
            None,
            Some(&water),
            64,
            &material,
            Limits::default(),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(layers.height, original);
        let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
        doc["cartography"] = serde_json::to_value(material).unwrap();
        let palette = crate::terrain_palette::Palette::decode(
            &serde_json::to_vec(&doc).unwrap(),
            &installation,
        )
        .unwrap();
        layers.height = surface;
        let expected = crate::map_image::render_with_water(
            &layers,
            None,
            None,
            Some(&water),
            64,
            Some(&palette),
            || false,
        )
        .unwrap()
        .unwrap();
        let mut maximum = 0;
        for (a, b) in actual.pixels.iter().zip(&expected.pixels) {
            for (a, b) in a.to_array().into_iter().zip(b.to_array()) {
                maximum = maximum.max(a.abs_diff(b));
            }
        }
        assert!(maximum <= 1, "CPU/GPU channel delta: {maximum}");
        println!("Raised surface CPU/GPU maximum channel delta: {maximum}");
    }

    #[test]
    #[ignore = "requires a hardware compute adapter"]
    fn filtered_surface_matches_cpu_for_land_and_shallow_water() {
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let cells = sr * sr;
        let encode = |h: f32| ((h + 500.) / 1000. * 32766.).round() as i16;
        let mut layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: (0..hr * hr)
                .map(|i| {
                    encode(if (i % hr / 8) % 2 == 0 {
                        // Moderate slopes survive the normal producer's
                        // 50 to 70 degree flattening and remain visible in water.
                        10. + ((i / hr) as f32 / 12.).sin() * 24.
                    } else {
                        100. + ((i / hr) as f32 / 35.).sin() * 24.
                            + ((i % hr) as f32 / 41.).sin() * 5.
                    })
                })
                .collect(),
            biome: vec![0; 5 * cells],
            splat: vec![0; 8 * cells],
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        layers.biome[..cells].fill(255);
        layers.splat[7 * cells..].fill(255);
        let water = vec![encode(11.); hr * hr];
        let mut material = crate::map_colors::tests::fixture();
        material.shader_blob_sha256 =
            Some("9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07".into());
        let grid = crate::map_surface_frame::generate(
            &layers.height,
            crate::map_surface_frame::settings(&layers, 8 * 1024 * 1024),
            crate::map_surface_frame::Profile::Legacy,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        let normal_colors = grid
            .normal()
            .unwrap()
            .pixels()
            .iter()
            .step_by(97)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            normal_colors.len() > 16,
            "GPU fixture did not exercise varying packed normals"
        );
        // Synthetic texture input shared by both verified shader programs.
        for shader in [
            "9ff936729f90baad098b7a69f0a3244868345f99e3a2542e00063cc004821e07",
            "60b55c7b11da98c2a67f2162a29f1e1aba3ce66d698f8d017f1fe5e5f9a6b8b1",
        ] {
            material.shader_blob_sha256 = Some(shader.into());
            let mut grid = if material.slope_uses_normal() {
                crate::map_surface_frame::generate(
                    &layers.height,
                    crate::map_surface_frame::settings(&layers, 8 * 1024 * 1024),
                    crate::map_surface_frame::Profile::Current,
                    None,
                    || false,
                )
                .unwrap()
                .unwrap()
            } else {
                grid.clone()
            };
            if material.slope_uses_normal() {
                crate::map_surface_frame::attach_shore(
                    &mut grid,
                    &layers,
                    &layers.height,
                    Some(&water),
                    64 * 1024 * 1024,
                    &|| false,
                )
                .unwrap()
                .unwrap();
            }
            for output in [128, 97] {
                let actual = render_with_surface(
                    &layers,
                    &layers.height,
                    None,
                    Some(&water),
                    output,
                    &material,
                    Some(&grid),
                    Limits::default(),
                    || false,
                )
                .unwrap()
                .unwrap();
                let baseline = render_with_height(
                    &layers,
                    &layers.height,
                    None,
                    Some(&water),
                    output,
                    &material,
                    Limits::default(),
                    || false,
                )
                .unwrap()
                .unwrap();
                let (installation, mut doc) = crate::terrain_palette::tests::fixture(5);
                doc["cartography"] = serde_json::to_value(&material).unwrap();
                let mut palette = crate::terrain_palette::Palette::decode(
                    &serde_json::to_vec(&doc).unwrap(),
                    &installation,
                )
                .unwrap();
                if material.slope_uses_normal() {
                    palette.test_current_surface_profile();
                } else {
                    palette.test_surface_profile();
                }
                let expected = crate::map_image::render_with_water(
                    &layers,
                    None,
                    None,
                    Some(&water),
                    output,
                    Some(&palette),
                    || false,
                )
                .unwrap()
                .unwrap();
                let mut maximum = 0;
                let mut changed = [0; 2];
                for (i, (a, b)) in actual.pixels.iter().zip(&expected.pixels).enumerate() {
                    for (a, b) in a.to_array().into_iter().zip(b.to_array()) {
                        maximum = maximum.max(a.abs_diff(b));
                    }
                    if *a != baseline.pixels[i] {
                        let x = ((i % output) as f32 + 0.5) / output as f32;
                        let index = (x * (hr - 1) as f32).round() as usize;
                        changed[usize::from(layers.height[index] > water[index])] += 1;
                    }
                }
                assert!(maximum <= 1, "CPU/GPU channel delta: {maximum}");
                assert!(
                    changed.iter().all(|v| *v > 0),
                    "No slope response in one medium: {changed:?}"
                );
                println!(
                    "Filtered surface CPU/GPU delta: {maximum}; changed shallow water/land pixels: {changed:?}"
                );
            }
        }
    }
}
