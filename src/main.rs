pub mod board;
pub mod circuit;
pub mod counter;
pub mod viewport;
pub mod wire;

use crate::board::{Board, BoardRenderer};
use crate::circuit::Circuit;
use crate::counter::Counter;
use crate::viewport::Viewport;
use crate::wire::{Pin, Wire, WireRenderer};
use anyhow::Context;
use futures_executor::block_on;
use glam::{IVec2, Vec2};
use std::sync::Arc;
use std::time::Instant;
use wgpu_glyph::ab_glyph::FontArc;
use wgpu_glyph::{GlyphBrushBuilder, Section, Text};
use winit::event::{
    ElementState, Event, MouseButton, MouseScrollDelta, VirtualKeyCode, WindowEvent,
};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::{CursorIcon, Window, WindowBuilder};

enum CursorMode {
    Normal,
    Pan {
        last_position: Vec2,
    },
    Place {
        start_position: IVec2,
        end_position: IVec2,
        start_pin: wire::Handle,
        end_pin: wire::Handle,
        wire: wire::Handle,
    },
}

pub type GraphicsContext = Arc<GraphicsContextInner>;

pub struct GraphicsContextInner {
    pub window: Window,
    pub surface: wgpu::Surface,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,

    pub render_format: wgpu::TextureFormat,
    pub depth_format: wgpu::TextureFormat,
}

impl GraphicsContextInner {
    async fn new(window: Window) -> anyhow::Result<Self> {
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

        // XXX does this produce incompatible formats on different backends?
        let render_format = adapter.get_swap_chain_preferred_format(&surface).unwrap();
        let depth_format = wgpu::TextureFormat::Depth32Float;

        Ok(Self {
            window,
            surface,
            device,
            queue,
            render_format,
            depth_format,
        })
    }
}

struct State {
    gfx: GraphicsContext,
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
    frame_counter: Counter,
    should_close: bool,
    last_update: Instant,
    cursor_mode: CursorMode,
    circuit: Circuit,
}

fn create_swap_chain(gfx: &GraphicsContext) -> wgpu::SwapChain {
    gfx.device.create_swap_chain(
        &gfx.surface,
        &wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::RENDER_ATTACHMENT,
            format: gfx.render_format,
            width: gfx.window.inner_size().width,
            height: gfx.window.inner_size().height,
            //present_mode: wgpu::PresentMode::Fifo,
            present_mode: wgpu::PresentMode::Mailbox,
        },
    )
}
fn create_depth_texture(gfx: &GraphicsContext) -> wgpu::Texture {
    gfx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth_texture"),
        size: wgpu::Extent3d {
            width: gfx.window.inner_size().width,
            height: gfx.window.inner_size().height,
            ..Default::default()
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: gfx.depth_format,
        usage: wgpu::TextureUsage::RENDER_ATTACHMENT | wgpu::TextureUsage::SAMPLED,
    })
}

impl State {
    async fn new(window: Window) -> anyhow::Result<Self> {
        let gfx = Arc::new(GraphicsContextInner::new(window).await?);
        let swap_chain = create_swap_chain(&gfx);
        let depth_texture = create_depth_texture(&gfx);
        let depth_texture_view = depth_texture.create_view(&Default::default());

        let fira_sans = FontArc::try_from_slice(include_bytes!("fonts/FiraSans-Regular.ttf"))?;
        let glyph_brush =
            GlyphBrushBuilder::using_font(fira_sans).build(&gfx.device, gfx.render_format);
        let staging_belt = wgpu::util::StagingBelt::new(1024);
        let local_pool = futures_executor::LocalPool::new();
        let local_spawner = local_pool.spawner();

        let viewport = Viewport::new(gfx.clone());

        let mut board_renderer = BoardRenderer::new(gfx.clone(), &viewport);
        board_renderer.insert(&Board {
            position: IVec2::new(0, 0),
            size: IVec2::new(2, 2),
            color: [0.2, 0.3, 0.1, 1.0],
            z_index: 1,
        });
        board_renderer.insert(&Board {
            position: IVec2::new(-4, -2),
            size: IVec2::new(2, 1),
            color: [0.3, 0.1, 0.2, 1.0],
            z_index: 1,
        });
        board_renderer.insert(&Board {
            position: IVec2::new(0, -4),
            size: IVec2::new(2, 2),
            color: [0.3, 0.2, 0.1, 1.0],
            z_index: 1,
        });
        board_renderer.insert(&Board {
            position: IVec2::new(-10_000, -10_000),
            size: IVec2::new(20_000, 20_000),
            color: [0.1, 0.1, 0.1, 1.0],
            z_index: 0,
        });

        let mut wire_renderer = WireRenderer::new(gfx.clone(), &viewport);

        wire_renderer.insert(
            &Pin {
                position: IVec2::new(0, 0),
                is_powered: true,
            }
            .into(),
        );
        wire_renderer.insert(
            &Wire {
                start: IVec2::new(0, 0),
                end: IVec2::new(1, 0),
                is_powered: true,
            }
            .into(),
        );
        wire_renderer.insert(
            &Pin {
                position: IVec2::new(1, 0),
                is_powered: true,
            }
            .into(),
        );
        wire_renderer.insert(
            &Wire {
                start: IVec2::new(0, 0),
                end: IVec2::new(0, -2),
                is_powered: true,
            }
            .into(),
        );
        wire_renderer.insert(
            &Pin {
                position: IVec2::new(0, -2),
                is_powered: true,
            }
            .into(),
        );

        wire_renderer.insert(
            &Pin {
                position: IVec2::new(0, 2),
                is_powered: false,
            }
            .into(),
        );
        wire_renderer.insert(
            &Wire {
                start: IVec2::new(0, 2),
                end: IVec2::new(-2, 2),
                is_powered: false,
            }
            .into(),
        );
        wire_renderer.insert(
            &Pin {
                position: IVec2::new(-2, 2),
                is_powered: false,
            }
            .into(),
        );
        let circuit = Circuit::new(gfx.clone(), &viewport);

        Ok(Self {
            gfx,
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
            frame_counter: Counter::new(),
            should_close: false,
            last_update: Instant::now(),
            cursor_mode: CursorMode::Normal,
            circuit,
        })
    }

    fn handle_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.should_close = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let position = Vec2::new(position.x as f32, position.y as f32);
                match &self.cursor_mode {
                    CursorMode::Pan { last_position } => {
                        let mut delta = position - *last_position;
                        delta.y = -delta.y;
                        let camera = self.viewport.camera_mut();
                        camera.pan -= delta / camera.zoom;

                        self.cursor_mode = CursorMode::Pan {
                            last_position: position,
                        };
                    }
                    _ => {}
                }
                self.viewport.cursor_moved(position);
            }
            WindowEvent::MouseInput { button, state, .. } => match (button, state) {
                (MouseButton::Middle, ElementState::Pressed) => {
                    self.cursor_mode = CursorMode::Pan {
                        last_position: self.viewport.cursor().screen_position,
                    };
                    self.gfx.window.set_cursor_icon(CursorIcon::Grabbing);
                }
                (MouseButton::Middle, ElementState::Released) => match self.cursor_mode {
                    CursorMode::Pan { .. } => {
                        self.cursor_mode = CursorMode::Normal;
                        self.gfx.window.set_cursor_icon(CursorIcon::Default);
                    }
                    _ => {}
                },
                (MouseButton::Left, ElementState::Pressed) => {
                    let start_position = self.viewport.cursor().tile();
                    let start_pin = self.wire_renderer.insert(
                        &Pin {
                            position: start_position,
                            is_powered: false,
                        }
                        .into(),
                    );
                    let end_pin = self.wire_renderer.insert(
                        &Pin {
                            position: start_position,
                            is_powered: false,
                        }
                        .into(),
                    );
                    let wire = self.wire_renderer.insert(
                        &Wire {
                            start: start_position,
                            end: start_position,
                            is_powered: false,
                        }
                        .into(),
                    );
                    self.cursor_mode = CursorMode::Place {
                        start_position,
                        end_position: start_position,
                        start_pin,
                        end_pin,
                        wire,
                    };
                }
                (MouseButton::Left, ElementState::Released) => match &self.cursor_mode {
                    &CursorMode::Place {
                        start_position,
                        end_position,
                        ref start_pin,
                        ref end_pin,
                        ref wire,
                    } => {
                        self.wire_renderer.remove(start_pin);
                        self.wire_renderer.remove(end_pin);
                        self.wire_renderer.remove(wire);

                        if start_position == end_position {
                            self.circuit.place_pin(start_position);
                        } else {
                            self.circuit.place_wire(start_position, end_position);
                        }

                        self.cursor_mode = CursorMode::Normal;
                    }
                    _ => {}
                },
                (MouseButton::Right, ElementState::Pressed) => match &self.cursor_mode {
                    &CursorMode::Normal => {
                        let position = self.viewport.cursor().tile();
                        self.circuit.delete_all_at(position);
                    }
                    _ => {}
                },
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => match &self.cursor_mode {
                CursorMode::Normal => {
                    let delta = match delta {
                        MouseScrollDelta::LineDelta(_x, y) => y,
                        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 16.0,
                    };
                    let camera = self.viewport.camera_mut();
                    camera.set_zoom(camera.zoom * camera.zoom_step.powf(delta));
                }
                _ => {}
            },
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
        self.viewport.update(dt);

        match &mut self.cursor_mode {
            &mut CursorMode::Place {
                start_position,
                ref mut end_position,
                ref start_pin,
                ref end_pin,
                ref wire,
            } => {
                let delta = self.viewport.cursor().tile() - start_position;

                let size;
                if delta.x.abs() > delta.y.abs() {
                    size = delta * IVec2::X;
                } else {
                    size = delta * IVec2::Y;
                }
                *end_position = start_position + size;

                self.wire_renderer.update(
                    start_pin,
                    &Pin {
                        position: start_position,
                        is_powered: false,
                    }
                    .into(),
                );
                self.wire_renderer.update(
                    end_pin,
                    &Pin {
                        position: *end_position,
                        is_powered: false,
                    }
                    .into(),
                );
                self.wire_renderer.update(
                    wire,
                    &Wire {
                        start: start_position,
                        end: *end_position,
                        is_powered: false,
                    }
                    .into(),
                );
            }
            _ => {}
        }
    }

    fn redraw(&mut self) -> anyhow::Result<()> {
        self.frame_counter.tick();

        let frame = loop {
            match self.swap_chain.get_current_frame() {
                Ok(frame) => break frame.output,
                Err(wgpu::SwapChainError::Lost) | Err(wgpu::SwapChainError::Outdated) => {
                    self.swap_chain = create_swap_chain(&self.gfx);

                    self.depth_texture = create_depth_texture(&self.gfx);
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

        let mut encoder = self.gfx.device.create_command_encoder(&Default::default());

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[wgpu::RenderPassColorAttachment {
                    view: &frame.view,
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
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_texture_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: true,
                    }),
                    stencil_ops: None,
                }),
            });
            self.board_renderer.draw(&self.viewport, &mut render_pass);
            self.circuit.draw(&self.viewport, &mut render_pass);
            self.wire_renderer.draw(&self.viewport, &mut render_pass);
        }

        let size = self.gfx.window.inner_size();
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
                &self.gfx.device,
                &mut self.staging_belt,
                &mut encoder,
                &frame.view,
                size.width,
                size.height,
            )
            .expect("Text draw error");
        self.staging_belt.finish();

        self.gfx.queue.submit(std::iter::once(encoder.finish()));

        use futures_util::task::SpawnExt;
        self.local_spawner
            .spawn(self.staging_belt.recall())
            .expect("Recall error");
        self.local_pool.run_until_stalled();

        Ok(())
    }

    fn debug_text(&self) -> String {
        let tile = self.circuit.tile(self.viewport.cursor().tile());
        format!(
            "FPS: {:.0}\nCursor: {:.0?}\nWorld: {:.2?}\nTile: {:?}\nPin: {:?}\nWires: {:?}\nWire count: {}",
            self.frame_counter.rate(),
            <(f32, f32)>::from(self.viewport.cursor().screen_position),
            <(f32, f32)>::from(self.viewport.cursor().world_position),
            <(i32, i32)>::from(self.viewport.cursor().tile()),
            tile.and_then(|tile| tile.pin),
            tile.map(|tile| tile.wires),
            self.wire_renderer.wire_count(),
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
                state.gfx.window.request_redraw();
            }
            _ => {}
        }
        if state.should_close {
            *control_flow = ControlFlow::Exit;
        }
    });
}
