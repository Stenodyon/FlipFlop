mod board;
mod viewport;
mod wire;

use crate::board::{Board, BoardRenderer};
use crate::viewport::Viewport;
use crate::wire::{Wire, WireRenderer, WireState};
use anyhow::Context;
use cgmath::Vector2;
use futures_executor::block_on;
use std::time::{Duration, Instant};
use wgpu_glyph::ab_glyph::FontArc;
use wgpu_glyph::{GlyphBrushBuilder, Section, Text};
use winit::event::{ElementState, Event, VirtualKeyCode, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::WindowBuilder;

const RENDER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

const FPS_UPDATE_INTERVAL: Duration = Duration::from_millis(200);

struct State {
    window: winit::window::Window,
    instance: wgpu::Instance,
    surface: wgpu::Surface,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    swap_chain: wgpu::SwapChain,
    depth_texture: wgpu::Texture,
    depth_texture_view: wgpu::TextureView,
    glyph_brush: wgpu_glyph::GlyphBrush<()>,
    staging_belt: wgpu::util::StagingBelt,
    local_pool: futures_executor::LocalPool,
    local_spawner: futures_executor::LocalSpawner,
    viewport: Viewport,
    board_renderer: BoardRenderer,
    wire_renderer: WireRenderer,
    preview_wire: wire::Handle,
    frames_since: Instant,
    frame_count: usize,
    fps: f32,
    should_close: bool,
    last_update: Instant,
}

fn create_swap_chain(
    device: &wgpu::Device,
    surface: &wgpu::Surface,
    window: &winit::window::Window,
) -> wgpu::SwapChain {
    device.create_swap_chain(
        &surface,
        &wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::RENDER_ATTACHMENT,
            format: RENDER_FORMAT,
            width: window.inner_size().width,
            height: window.inner_size().height,
            present_mode: wgpu::PresentMode::Fifo,
        },
    )
}
fn create_depth_texture(device: &wgpu::Device, window: &winit::window::Window) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth_texture"),
        size: wgpu::Extent3d {
            width: window.inner_size().width,
            height: window.inner_size().height,
            depth: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsage::RENDER_ATTACHMENT | wgpu::TextureUsage::SAMPLED,
    })
}

impl State {
    async fn new(window: winit::window::Window) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::BackendBit::PRIMARY);
        let surface = unsafe { instance.create_surface(&window) };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: Default::default(),
                compatible_surface: Some(&surface),
            })
            .await
            .context("Failed to find a suitable adapter")?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    features: Default::default(),
                    limits: Default::default(),
                },
                None,
            )
            .await
            .context("Failed to open device")?;
        let swap_chain = create_swap_chain(&device, &surface, &window);
        let depth_texture = create_depth_texture(&device, &window);
        let depth_texture_view = depth_texture.create_view(&Default::default());

        let fira_sans = FontArc::try_from_slice(include_bytes!("fonts/FiraSans-Regular.ttf"))?;
        let glyph_brush = GlyphBrushBuilder::using_font(fira_sans).build(&device, RENDER_FORMAT);
        let staging_belt = wgpu::util::StagingBelt::new(1024);
        let local_pool = futures_executor::LocalPool::new();
        let local_spawner = local_pool.spawner();

        let viewport = Viewport::new(&device);

        let mut board_renderer = BoardRenderer::new(&device, &queue, RENDER_FORMAT, &viewport);
        board_renderer.insert(&Board {
            position: [0.0, 0.0],
            size: [2.0, 2.0],
            color: [0.2, 0.3, 0.1, 1.0],
            z_index: 1,
        });
        board_renderer.insert(&Board {
            position: [-4.0, -2.0],
            size: [2.0, 1.0],
            color: [0.3, 0.1, 0.2, 1.0],
            z_index: 1,
        });
        board_renderer.insert(&Board {
            position: [0.0, -4.0],
            size: [2.0, 2.0],
            color: [0.3, 0.2, 0.1, 1.0],
            z_index: 1,
        });
        board_renderer.insert(&Board {
            position: [-1.0e4, -1.0e4],
            size: [2.0e4, 2.0e4],
            color: [0.1, 0.1, 0.1, 1.0],
            z_index: 0,
        });

        let mut wire_renderer = WireRenderer::new(&device, RENDER_FORMAT, &viewport);

        wire_renderer.insert(&Wire {
            cluster_index: 1,
            position: [0.0, 0.0],
            size: [1.0, 0.0],
        });
        wire_renderer.insert(&Wire {
            cluster_index: 2,
            position: [0.0, 2.0],
            size: [-2.0, 0.0],
        });
        wire_renderer.insert(&Wire {
            cluster_index: 1,
            position: [0.0, 0.0],
            size: [0.0, -2.0],
        });
        let preview_wire = wire_renderer.insert(&Wire {
            cluster_index: 0,
            position: [0.0, 0.0],
            size: [0.0, 0.0],
        });
        let mut wire_state = WireState::default();
        wire_state.states[0] = 0x00000002;
        wire_renderer.update_wire_state(&queue, &wire_state);

        Ok(Self {
            window,
            instance,
            surface,
            adapter,
            device,
            queue,
            swap_chain,
            depth_texture,
            depth_texture_view,
            glyph_brush,
            staging_belt,
            local_pool,
            local_spawner,
            viewport,
            board_renderer,
            wire_renderer,
            preview_wire,
            frames_since: Instant::now(),
            frame_count: 0,
            fps: 0.0,
            should_close: false,
            last_update: Instant::now(),
        })
    }

    fn handle_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.should_close = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let position = Vector2 {
                    x: position.x as f32,
                    y: position.y as f32,
                };
                self.viewport.cursor_moved(position);
            }
            WindowEvent::KeyboardInput { input, .. } => {
                if let Some(keycode) = input.virtual_keycode {
                    let pressed = match input.state {
                        ElementState::Pressed => true,
                        ElementState::Released => false,
                    };

                    match keycode {
                        VirtualKeyCode::Up => {
                            self.viewport.camera_mut().pan_up = pressed;
                        }
                        VirtualKeyCode::Down => {
                            self.viewport.camera_mut().pan_down = pressed;
                        }
                        VirtualKeyCode::Left => {
                            self.viewport.camera_mut().pan_left = pressed;
                        }
                        VirtualKeyCode::Right => {
                            self.viewport.camera_mut().pan_right = pressed;
                        }
                        VirtualKeyCode::PageUp => {
                            self.viewport.camera_mut().zoom_in = pressed;
                        }
                        VirtualKeyCode::PageDown => {
                            self.viewport.camera_mut().zoom_out = pressed;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn update(&mut self) {
        let now = Instant::now();
        let dt = now - self.last_update;
        self.last_update = now;
        self.viewport.update(dt, &self.window, &self.queue);
        self.wire_renderer.update(
            &self.preview_wire,
            &Wire {
                cluster_index: 0,
                position: self.viewport.cursor().tile().into(),
                size: [0.0, 0.0],
            },
        );
    }

    fn redraw(&mut self) -> anyhow::Result<()> {
        let this_render = Instant::now();
        let interval = this_render - self.frames_since;
        if interval >= FPS_UPDATE_INTERVAL {
            self.frames_since = this_render;
            self.fps = (self.frame_count as f32) / interval.as_secs_f32();
            self.frame_count = 0;
        }
        self.frame_count += 1;

        let frame = loop {
            match self.swap_chain.get_current_frame() {
                Ok(frame) => break frame.output,
                Err(wgpu::SwapChainError::Lost) | Err(wgpu::SwapChainError::Outdated) => {
                    self.swap_chain = create_swap_chain(&self.device, &self.surface, &self.window);

                    self.depth_texture = create_depth_texture(&self.device, &self.window);
                    self.depth_texture_view = self.depth_texture.create_view(&Default::default());
                }
                Err(wgpu::SwapChainError::Timeout) => {
                    return Ok(());
                }
                Err(err) => {
                    return Err(err.into());
                }
            }
        };

        let mut encoder = self.device.create_command_encoder(&Default::default());

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[wgpu::RenderPassColorAttachmentDescriptor {
                    attachment: &frame.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.2,
                            b: 0.3,
                            a: 1.0,
                        }),
                        store: true,
                    },
                }],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachmentDescriptor {
                    attachment: &self.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: true,
                    }),
                    stencil_ops: None,
                }),
            });
            self.board_renderer
                .draw(&self.viewport, &self.queue, &mut render_pass);
            self.wire_renderer
                .draw(&self.viewport, &self.queue, &mut render_pass);
        }

        let size = self.window.inner_size();
        self.glyph_brush.queue(Section {
            screen_position: (0.0, 0.0),
            bounds: (size.width as f32, size.height as f32),
            text: vec![Text::new(&self.debug_text())
                .with_color([1.0, 1.0, 1.0, 1.0])
                .with_scale(24.0)],
            ..Default::default()
        });
        self.glyph_brush
            .draw_queued(
                &self.device,
                &mut self.staging_belt,
                &mut encoder,
                &frame.view,
                size.width,
                size.height,
            )
            .expect("Text draw error");
        self.staging_belt.finish();

        self.queue.submit(std::iter::once(encoder.finish()));

        use futures_util::task::SpawnExt;
        self.local_spawner
            .spawn(self.staging_belt.recall())
            .expect("Recall error");
        self.local_pool.run_until_stalled();

        Ok(())
    }

    fn debug_text(&self) -> String {
        format!(
            "FPS: {:.0}\nCursor: {:.0?}\nWorld: {:.2?}\nTile: {:.0?}",
            self.fps,
            // this would be less ugly if cgmath vectors implemented From<[_; _]>
            // instead of Into<[_; _]>
            <_ as Into<[f32; 2]>>::into(self.viewport.cursor().screen_position),
            <_ as Into<[f32; 2]>>::into(self.viewport.cursor().world_position),
            <_ as Into<[f32; 2]>>::into(self.viewport.cursor().tile()),
        )
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("FlipFlop")
        .build(&event_loop)?;

    let mut state = block_on(State::new(window))?;

    event_loop.run(move |event, _, control_flow| {
        match event {
            Event::RedrawRequested(..) => {
                state.update();
                state.redraw().unwrap();
            }
            Event::WindowEvent { event, .. } => {
                state.handle_window_event(event);
            }
            Event::MainEventsCleared => {
                state.window.request_redraw();
            }
            _ => {}
        }
        if state.should_close {
            *control_flow = ControlFlow::Exit;
        }
    });
}
