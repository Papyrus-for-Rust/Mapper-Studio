//! Experimental GPU evaluator used by the slope benchmark.
use crate::slope_compute::Threshold;
use eframe::wgpu::{self, util::DeviceExt};
use papyrus_terrain::terrain_slope;
use std::time::Duration;

const WORKGROUP: usize = 128;

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    pub adapter: wgpu::AdapterInfo,
}

impl Gpu {
    pub fn new() -> Result<Self, String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err("A hardware GPU is required".into());
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Mapper slope compute"),
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Encoded slope filter"),
            source: wgpu::ShaderSource::Wgsl(include_str!("slope_compute.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Encoded slope filter"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(error) = pollster::block_on(validation.pop()) {
            return Err(error.to_string());
        }
        Ok(Self {
            device,
            queue,
            pipeline,
            adapter: info,
        })
    }

    pub fn run(
        &self,
        input: &terrain_slope::Input<'_>,
        threshold: &Threshold,
        maximum_bytes: usize,
    ) -> Result<Vec<u32>, String> {
        let n = input.resolution;
        if !(3..=4097).contains(&n)
            || input.heights.len() != n * n
            || input.heights.iter().any(|&h| !(0..=32766).contains(&h))
            || cfg!(target_endian = "big")
        {
            return Err("Invalid GPU slope input".into());
        }
        let cells = n * n;
        let words = cells.div_ceil(16);
        let output_bytes = (words * 4) as u64;
        let input_bytes = (cells.div_ceil(2) * 4) as u64;
        let groups = words.div_ceil(WORKGROUP) as u32;
        let limits = self.device.limits();
        if input_bytes + output_bytes * 2 + 16 > maximum_bytes as u64
            || input_bytes.max(output_bytes) > u64::from(limits.max_storage_buffer_binding_size)
            || groups > limits.max_compute_workgroups_per_dimension
        {
            return Err("GPU slope buffers exceed the compute allowance".into());
        }
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let upload = |label, bytes: &[u8], usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytes,
                    usage,
                })
        };
        let height = upload(
            "Encoded terrain",
            bytemuck::cast_slice(input.heights),
            wgpu::BufferUsages::STORAGE,
        );
        let params = [n as u32, cells as u32, threshold.low, threshold.high];
        let params = upload(
            "Slope threshold",
            bytemuck::cast_slice(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        let buffer = |usage| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Slope decisions"),
                size: output_bytes,
                usage,
                mapped_at_creation: false,
            })
        };
        let output = buffer(wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
        let readback = buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
        let entries = [&params, &height, &output].map(|b| b.as_entire_binding());
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Slope input and output"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &std::array::from_fn::<_, 3, _>(|i| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: entries[i].clone(),
            }),
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output_bytes);
        let errors = [
            pollster::block_on(oom.pop()),
            pollster::block_on(validation.pop()),
        ];
        if let Some(error) = errors.into_iter().flatten().next() {
            return Err(error.to_string());
        }
        let submission = self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(10)),
            })
            .map_err(|error| error.to_string())?;
        rx.recv_timeout(Duration::from_secs(1))
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let words = {
            let bytes = readback
                .slice(..)
                .get_mapped_range()
                .map_err(|error| error.to_string())?;
            bytemuck::cast_slice::<_, u32>(&bytes).to_vec()
        };
        readback.unmap();
        Ok(words)
    }
}
