//! Versioned GPU erosion kernel. Recipes explicitly bind numerical device identity.
use eframe::wgpu::{self, util::DeviceExt};
use papyrus_terrain::erosion_grid::{Input, Limits, Output, Policy, Report};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

mod calibration;
pub use calibration::{Recommendation, recommend};

pub const VERSION: &str = "papyrus-erosion-gpu-wgsl-1";
const BATCH: usize = 16;
const MIB: u64 = 1024 * 1024;

fn buffer_bytes(cells: u64) -> u64 {
    cells * 56 + 64
}

pub fn required_memory_mib(resolution: usize) -> Option<u64> {
    (3..=4097).contains(&resolution)
        .then(|| buffer_bytes((resolution as u64).pow(2)).div_ceil(MIB))
}

pub(crate) fn check_buffer_allowance(resolution: usize, gpu_bytes: u64) -> Result<u64, String> {
    if !(3..=4097).contains(&resolution) {
        return Err("Invalid GPU erosion resolution".into());
    }
    let cells = (resolution as u64).pow(2);
    let bytes = buffer_bytes(cells);
    if bytes > gpu_bytes {
        return Err(format!(
            "GPU erosion requires {} MiB of GPU buffer allowance",
            bytes.div_ceil(MIB)
        ));
    }
    Ok(cells)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub engine: String,
    pub backend: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub name: String,
    pub driver: String,
    pub driver_info: String,
}

impl Identity {
    fn from_info(info: wgpu::AdapterInfo) -> Result<Self, String> {
        if matches!(info.device_type, wgpu::DeviceType::Cpu) {
            return Err("GPU erosion requires a hardware compute adapter".into());
        }
        let identity = Self {
            engine: VERSION.into(),
            backend: format!("{:?}", info.backend),
            vendor_id: info.vendor,
            device_id: info.device,
            name: info.name,
            driver: info.driver,
            driver_info: info.driver_info,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION
            || !matches!(self.backend.as_str(), "Vulkan" | "Dx12" | "Metal")
            || self.name.is_empty()
            || self.name.len() > 256
            || self.driver.len() > 256
            || self.driver_info.len() > 512
        {
            return Err("Invalid GPU erosion recipe identity".into());
        }
        Ok(())
    }
}

/// Captures the current adapter for an explicit GPU recipe edit.
pub fn detect() -> Result<Identity, String> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|err| err.to_string())?;
    Identity::from_info(adapter.get_info())
}

fn matching_adapter(identity: &Identity) -> Result<wgpu::Adapter, String> {
    identity.validate()?;
    let backend = match identity.backend.as_str() {
        "Vulkan" => wgpu::Backends::VULKAN,
        "Dx12" => wgpu::Backends::DX12,
        "Metal" => wgpu::Backends::METAL,
        _ => return Err("Unsupported GPU backend".into()),
    };
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backend;
    let instance = wgpu::Instance::new(descriptor);
    pollster::block_on(instance.enumerate_adapters(backend))
        .into_iter()
        .find(|adapter| {
            Identity::from_info(adapter.get_info()).is_ok_and(|actual| actual == *identity)
        })
        .ok_or_else(|| {
            format!(
                "Recipe requires {} on {} with driver {} {}",
                identity.name, identity.backend, identity.driver, identity.driver_info
            )
        })
}

pub struct Run {
    pub output: Output,
    pub explicit_buffer_bytes: u64,
    pub setup_seconds: f64,
    pub simulation_seconds: f64,
    pub readback_seconds: f64,
    pub verification_seconds: f64,
}

pub(crate) struct PreparedAdapter {
    resolution: usize,
    adapter: wgpu::Adapter,
    limits: wgpu::Limits,
}

pub(crate) fn prepare_adapter(
    identity: &Identity,
    resolution: usize,
    gpu_bytes: u64,
) -> Result<PreparedAdapter, String> {
    let cells = check_buffer_allowance(resolution, gpu_bytes)?;
    let adapter = matching_adapter(identity)?;
    let limits = wgpu::Limits {
        max_storage_buffers_per_shader_stage: 9,
        max_storage_buffer_binding_size: cells * 16,
        max_buffer_size: cells * 16,
        ..wgpu::Limits::downlevel_defaults()
    };
    if !limits.check_limits(&adapter.limits()) {
        return Err("GPU adapter cannot fit the erosion grid or binding set".into());
    }
    Ok(PreparedAdapter { resolution, adapter, limits })
}

pub fn run(
    identity: &Identity,
    input: Input<'_>,
    policy: Policy,
    limits: Limits,
    gpu_bytes: u64,
    cancelled: impl Fn() -> bool,
    progress: impl FnMut(usize, usize),
) -> Result<Option<Run>, String> {
    if cancelled() {
        return Ok(None);
    }
    let setup = Instant::now();
    let adapter = prepare_adapter(identity, input.resolution, gpu_bytes)?;
    let adapter_seconds = setup.elapsed().as_secs_f64();
    run_prepared(adapter, input, policy, limits, cancelled, progress).map(|result| {
        result.map(|mut run| {
            run.setup_seconds += adapter_seconds;
            run
        })
    })
}

pub(crate) fn run_prepared(
    adapter: PreparedAdapter,
    input: Input<'_>,
    policy: Policy,
    limits: Limits,
    cancelled: impl Fn() -> bool,
    mut progress: impl FnMut(usize, usize),
) -> Result<Option<Run>, String> {
    let Input {
        resolution,
        heights: original,
        minimum_heights: floor,
    } = input;
    if resolution != adapter.resolution {
        return Err("GPU adapter was prepared for a different erosion grid".into());
    }
    let n = resolution as u32;
    policy.validate()?;
    let cells = u64::from(n) * u64::from(n);
    let stride = cells * 4;
    let bytes = buffer_bytes(cells);
    let work = cells * (policy.iterations as u64 * 6 + 2);
    // Two host f32 outputs and the mapped readback coexist during validation.
    let host_bytes = (cells * 12) as usize;
    if host_bytes > limits.maximum_bytes
        || work > limits.maximum_work
        || original.len() != cells as usize
        || floor.len() != cells as usize
    {
        return Err("GPU erosion exceeds its grid, memory or work bounds".into());
    }
    for (heights, floors) in original.chunks(65536).zip(floor.chunks(65536)) {
        if cancelled() {
            return Ok(None);
        }
        if heights.iter().zip(floors).any(|(&h, &f)| {
            !h.is_finite() || !f.is_finite() || h.abs() > 65536. || f.abs() > 65536. || f > h
        }) {
            return Err("Invalid erosion height or floor values".into());
        }
    }
    let setup = Instant::now();
    let (device, queue) = pollster::block_on(adapter.adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Mapper erosion diagnostics"),
        required_limits: adapter.limits,
        ..Default::default()
    }))
    .map_err(|err| err.to_string())?;
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let allocation = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    // Match the scalar layout of Policy in grid.wgsl, padded to a 16-byte boundary.
    let mut params = [0_u32; 16];
    params[0] = n;
    for (out, value) in params[1..].iter_mut().zip([
        policy.time_step,
        policy.water_fill_rate * policy.time_step,
        1. - policy.evaporation_rate * policy.time_step,
        policy.gravity,
        policy.pipe_area,
        policy.pipe_length,
        policy.cell_area,
        policy.normal_y,
        policy.ocean_level,
        policy.minimum_angle,
        policy.sediment_capacity,
        policy.dissolve_rate,
        policy.depth_fade,
    ]) {
        *out = value.to_bits();
    }
    let upload = |label, contents, usage| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    };
    let buffer = |label, size, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        })
    };
    let storage = wgpu::BufferUsages::STORAGE;
    let height = storage | wgpu::BufferUsages::COPY_SRC;
    let fields = [
        upload(
            "Erosion policy",
            bytemuck::cast_slice(&params),
            wgpu::BufferUsages::UNIFORM,
        ),
        upload("Original height", bytemuck::cast_slice(original), storage),
        upload("Minimum height", bytemuck::cast_slice(floor), storage),
        buffer("Height A", stride, height),
        buffer("Height B", stride, height),
        buffer("Water depth", stride, storage),
        buffer("Velocity", stride * 2, storage),
        buffer("Directional flux", stride * 4, storage),
        buffer("Sediment A", stride, storage),
        buffer("Sediment B", stride, storage),
    ];
    let readback = buffer(
        "Final height readback",
        stride,
        wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Hydraulic erosion"),
        source: wgpu::ShaderSource::Wgsl(include_str!("erosion_compute.wgsl").into()),
    });
    let stages: [(&str, &[usize]); 5] = [
        ("initialize", &[0, 1, 3, 5]),
        ("fill_flux", &[0, 3, 5, 7]),
        ("lower_height", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        ("finish_height", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        ("validate", &[0, 3, 4, 5, 6, 7, 8]),
    ];
    let stages = stages.map(|(label, indices)| {
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: None,
            module: &shader,
            entry_point: Some(label),
            compilation_options: Default::default(),
            cache: None,
        });
        let layout = pipeline.get_bind_group_layout(0);
        let groups = [
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            [0, 1, 2, 4, 3, 5, 6, 7, 9, 8],
        ]
        .map(|fields_by_binding| {
            let entries = indices
                .iter()
                .map(|&binding| wgpu::BindGroupEntry {
                    binding: binding as u32,
                    resource: fields[fields_by_binding[binding]].as_entire_binding(),
                })
                .collect::<Vec<_>>();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &layout,
                entries: &entries,
            })
        });
        (pipeline, groups)
    });
    let errors = [
        pollster::block_on(allocation.pop()),
        pollster::block_on(validation.pop()),
    ];
    if let Some(err) = errors.into_iter().flatten().next() {
        return Err(err.to_string());
    }
    let setup_seconds = setup.elapsed().as_secs_f64();
    let simulation = Instant::now();
    let wait = |submission| {
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(30)),
            })
            .map_err(|err| err.to_string())
    };
    progress(0, policy.iterations);
    for first in (0..policy.iterations).step_by(BATCH) {
        if cancelled() {
            return Ok(None);
        }
        let end = (first + BATCH).min(policy.iterations);
        let last = end == policy.iterations;
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            let mut dispatch = |stage: usize, parity: usize| {
                let (pipeline, groups) = &stages[stage];
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &groups[parity], &[]);
                pass.dispatch_workgroups(n.div_ceil(16), n.div_ceil(8), 1);
            };
            if first == 0 {
                dispatch(0, 0);
            }
            for step in first..end {
                dispatch(1, step % 2);
                dispatch(if step + 1 == policy.iterations { 3 } else { 2 }, step % 2);
            }
            if last {
                dispatch(4, end % 2);
            }
        }
        if last {
            encoder.copy_buffer_to_buffer(&fields[4 - end % 2], 0, &readback, 0, stride);
        }
        wait(queue.submit([encoder.finish()]))?;
        if cancelled() {
            return Ok(None);
        }
        progress(end, policy.iterations);
    }
    let simulation_seconds = simulation.elapsed().as_secs_f64();
    if cancelled() {
        return Ok(None);
    }
    let read_started = Instant::now();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .map_err(|err| err.to_string())?;
    receive
        .recv_timeout(Duration::from_secs(1))
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())?;
    let mapped = readback
        .slice(..)
        .get_mapped_range()
        .map_err(|err| err.to_string())?;
    let readback_seconds = read_started.elapsed().as_secs_f64();
    let verify = Instant::now();
    let mut height_delta = Vec::new();
    height_delta
        .try_reserve_exact(cells as usize)
        .map_err(|e| e.to_string())?;
    let mut heights = Vec::new();
    heights
        .try_reserve_exact(cells as usize)
        .map_err(|e| e.to_string())?;
    let mut maximum_lowering = 0_f32;
    for (index, ((sample, &initial), &minimum)) in
        mapped.chunks_exact(4).zip(original).zip(floor).enumerate()
    {
        if index % 65536 == 0 && cancelled() {
            return Ok(None);
        }
        let final_height = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
        if !final_height.is_finite() || final_height < minimum || final_height > initial {
            return Err("GPU erosion produced an invalid simulation state".into());
        }
        let delta = initial - final_height;
        maximum_lowering = maximum_lowering.max(delta);
        heights.push(final_height);
        height_delta.push(delta);
    }
    drop(mapped);
    readback.unmap();
    Ok((!cancelled()).then(|| Run {
        output: Output {
            heights,
            height_delta,
            report: Report {
                engine: VERSION,
                iterations: policy.iterations,
                cells: cells as usize,
                buffer_bytes: host_bytes,
                work,
                maximum_lowering,
                maximum_raise: 0.,
            },
        },
        explicit_buffer_bytes: bytes,
        setup_seconds,
        simulation_seconds,
        readback_seconds,
        verification_seconds: verify.elapsed().as_secs_f64(),
    }))
}
