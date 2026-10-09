//! Klip test game: a wgpu window that draws a barcoded frame counter (klip-testkit
//! layout), a moving bar and scrolling checker for motion, and a white flash once per
//! second. Prints one JSON line `{"event":"ready",...}` with its pid and window handle
//! after the first frame, so tests can point the recorder at it.
//!
//! Dev tool, so `arithmetic_side_effects` is allowed (ADR 0001).

#![allow(clippy::arithmetic_side_effects)]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use klip_testkit::barcode::{self, CELLS, LAYOUT};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Fullscreen, Window, WindowId};

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum Mode {
    Windowed,
    Borderless,
    Exclusive,
}

/// Test game for Klip clip checks.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Client-area width in pixels.
    #[arg(long, default_value_t = 1280)]
    width: u32,
    /// Client-area height in pixels.
    #[arg(long, default_value_t = 720)]
    height: u32,
    /// Target frames per second (frame pacing by the game, vsync off).
    #[arg(long, default_value_t = 120)]
    fps: u32,
    #[arg(long, value_enum, default_value_t = Mode::Windowed)]
    mode: Mode,
    /// Quit after this long (e.g. 60s). Runs until closed if omitted.
    #[arg(long, value_parser = parse_dur)]
    duration: Option<Duration>,
    /// Write `counter,nanoseconds_since_start` for every presented frame.
    #[arg(long)]
    frames_log: Option<PathBuf>,
    /// Window title.
    #[arg(long, default_value = "Klip Test Game")]
    title: String,
}

fn parse_dur(s: &str) -> Result<Duration, String> {
    let ns = klip_media::parse_duration(s).ok_or_else(|| format!("bad duration {s:?}"))?;
    Ok(Duration::from_nanos(
        u64::try_from(ns).map_err(|e| e.to_string())?,
    ))
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    bits: [u32; 4],
    band: [f32; 4],
    misc: [f32; 4],
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    /// Backend and present mode, reported in the ready line.
    describe: String,
}

impl Gfx {
    fn new(window: Arc<Window>) -> Result<Gfx> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        // Most Windows games are Direct3D; capture behaviour should match theirs.
        if cfg!(windows) {
            desc.backends = wgpu::Backends::DX12;
        }
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        let caps = surface.get_capabilities(&adapter);
        // A non-sRGB format so the shader's values land unchanged in the frame.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| anyhow!("surface has no formats"))?;
        let present_mode = [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
            .into_iter()
            .find(|m| caps.present_modes.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("testgame"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("testgame"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let describe = format!("{:?} {:?}", adapter.get_info().backend, present_mode);
        Ok(Gfx {
            describe,
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            uniforms,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn render(&mut self, counter: u32, fps: u32) -> Result<()> {
        let flash = counter.is_multiple_of(fps);
        let u = Uniforms {
            bits: [barcode::encode(counter), u32::from(flash), counter, CELLS],
            band: [LAYOUT.x0, LAYOUT.y0, LAYOUT.x1, LAYOUT.y1],
            misc: [
                LAYOUT.margin,
                self.config.width as f32,
                self.config.height as f32,
                (counter % 120) as f32 / 120.0,
            ],
        };
        self.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.surface.configure(&self.device, &self.config);
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            other => return Err(anyhow!("surface unusable: {other:?}")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        Ok(())
    }
}

struct App {
    args: Args,
    gfx: Option<Gfx>,
    counter: u32,
    start: Instant,
    next_frame: Instant,
    period: Duration,
    log: Option<BufWriter<File>>,
    announced: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn fail(&mut self, el: &ActiveEventLoop, e: anyhow::Error) {
        self.error = Some(e);
        el.exit();
    }

    fn create_window(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let mut attrs = Window::default_attributes()
            .with_title(self.args.title.clone())
            .with_inner_size(PhysicalSize::new(self.args.width, self.args.height))
            .with_resizable(false);
        match self.args.mode {
            Mode::Windowed => {}
            Mode::Borderless => attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None))),
            Mode::Exclusive => {
                let monitor = el
                    .primary_monitor()
                    .ok_or_else(|| anyhow!("no primary monitor"))?;
                let mode = monitor
                    .video_modes()
                    .filter(|m| m.size() == PhysicalSize::new(self.args.width, self.args.height))
                    .max_by_key(|m| m.refresh_rate_millihertz())
                    .ok_or_else(|| {
                        anyhow!("no {}x{} video mode", self.args.width, self.args.height)
                    })?;
                attrs = attrs.with_fullscreen(Some(Fullscreen::Exclusive(mode)));
            }
        }
        let window = Arc::new(el.create_window(attrs)?);
        self.gfx = Some(Gfx::new(window)?);
        Ok(())
    }

    fn announce(&mut self) -> Result<()> {
        let Some(gfx) = &self.gfx else { return Ok(()) };
        let hwnd: u64 = match gfx.window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get() as u64,
            _ => 0,
        };
        let size = gfx.window.inner_size();
        let line = serde_json::json!({
            "event": "ready",
            "pid": std::process::id(),
            "hwnd": hwnd,
            "width": size.width,
            "height": size.height,
            "fps": self.args.fps,
            "gfx": gfx.describe,
        });
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}")?;
        out.flush()?;
        Ok(())
    }

    fn frame(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let counter = self.counter;
        let Some(gfx) = &mut self.gfx else {
            return Ok(());
        };
        gfx.render(counter, self.args.fps)?;
        let t = self.start.elapsed();
        if let Some(log) = &mut self.log {
            writeln!(log, "{counter},{}", t.as_nanos())?;
        }
        self.counter = self.counter.wrapping_add(1) & barcode::COUNTER_MASK;
        if !self.announced {
            self.announced = true;
            self.announce()?;
        }
        if let Some(d) = self.args.duration
            && t >= d
        {
            el.exit();
        }
        // Fixed cadence; if we fell far behind, resynchronise instead of bursting.
        self.next_frame += self.period;
        let now = Instant::now();
        if self.next_frame + self.period * 2 < now {
            self.next_frame = now + self.period;
        }
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_none()
            && let Err(e) = self.create_window(el)
        {
            self.fail(el, e);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = &mut self.gfx {
                    g.resize(size);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        el.set_control_flow(ControlFlow::Poll);
        // Pace frames here instead of via request_redraw (WM_PAINT is low priority and
        // made the game miss its rate): sleep most of the gap, spin the rest, render.
        let now = Instant::now();
        if now < self.next_frame {
            let gap = self.next_frame - now;
            if gap > Duration::from_millis(2) {
                std::thread::sleep(gap - Duration::from_millis(2));
            }
            while Instant::now() < self.next_frame {
                std::hint::spin_loop();
            }
        }
        if let Err(e) = self.frame(el) {
            self.fail(el, e);
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.fps == 0 || args.fps > 1000 {
        return Err(anyhow!("--fps must be 1..=1000"));
    }
    let log = match &args.frames_log {
        Some(p) => Some(BufWriter::new(File::create(p).context("frames log")?)),
        None => None,
    };
    let period = Duration::from_secs_f64(1.0 / f64::from(args.fps));
    let now = Instant::now();
    let mut app = App {
        args,
        gfx: None,
        counter: 0,
        start: now,
        next_frame: now,
        period,
        log,
        announced: false,
        error: None,
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    if let Some(log) = &mut app.log {
        log.flush()?;
    }
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
