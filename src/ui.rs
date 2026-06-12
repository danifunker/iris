use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::Mutex;
use winit::{
    event::{ElementState, Event, KeyEvent, WindowEvent, MouseButton},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowBuilder},
};
use glow::HasContext;
use crate::ps2::Ps2Controller;
use crate::rex3::{Rex3, Renderer};
use crate::disp::{Rex3Screen, StatusBar, StatusBarTexture, BarStats, STATUS_BAR_HEIGHT};
use crate::compositor::{Compositor, SwCompositor};
use crate::debug_overlay::DebugOverlay;
use crate::hptimer::{TimerManager, TimerReturn};
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, PossiblyCurrentContext};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{GlSurface, SwapInterval, WindowSurface, Surface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasRawWindowHandle;
use std::num::NonZeroU32;
use std::ffi::CString;

// Vertex layout: [pos_x, pos_y, tex_u, tex_v] × 4 vertices = 64 bytes
const VBO_SIZE: i32 = 64;

struct GlState {
    gl: glow::Context,
    context: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    // Shader program: one program for all draw passes (texelFetch + y-flip)
    program: glow::Program,
    viewport_info_loc: Option<glow::UniformLocation>,
    scale_factor_loc:  Option<glow::UniformLocation>,
    // Shared VAO + two VBOs: emulator quad and status-bar quad
    vao:        glow::VertexArray,
    main_vbo:   glow::Buffer,
    status_vbo: glow::Buffer,
}

struct GlRenderer {
    window:      Arc<Window>,
    gl_config:   glutin::config::Config,
    window_size: Arc<Mutex<Option<(u32, u32)>>>,
    state:       Option<GlState>,
    compositor:  SwCompositor,
    current_w:   usize,
    current_h:   usize,
    scale:       u32,
}

// Safety: GlRenderer is sent to the refresh thread where it owns and uses the GL context.
// No other thread touches these fields.
unsafe impl Send for GlRenderer {}

impl GlRenderer {
    fn init_gl(&mut self) {
        let raw_window_handle = self.window.raw_window_handle();
        let gl_display = self.gl_config.display();

        let context_attributes = ContextAttributesBuilder::new().build(Some(raw_window_handle));
        let not_current_gl_context = unsafe {
            gl_display
                .create_context(&self.gl_config, &context_attributes)
                .expect("failed to create context")
        };

        let attrs = self.window.build_surface_attributes(Default::default());
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&self.gl_config, &attrs)
                .unwrap()
        };

        let gl_context = not_current_gl_context.make_current(&gl_surface).unwrap();

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                gl_display.get_proc_address(&CString::new(s).unwrap())
            })
        };

        let _ = gl_surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZeroU32::new(1).unwrap()));

        let (program, viewport_info_loc, scale_factor_loc, vao, main_vbo, status_vbo) = unsafe {
            let vs_src = "
                #version 150
                in vec2 position;
                in vec2 tex_coord;
                out vec2 v_tex_coord;
                void main() {
                    gl_Position = vec4(position, 0.0, 1.0);
                    v_tex_coord = tex_coord;
                }
            ";

            let program = gl.create_program().unwrap();
            let vs = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(vs, vs_src);
            gl.compile_shader(vs);
            if !gl.get_shader_compile_status(vs) {
                panic!("Vertex shader compilation failed: {}", gl.get_shader_info_log(vs));
            }

            let fs = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            // Try GLSL 1.50 first (texelFetch, Core Profile compatible)
            gl.shader_source(fs, "
                #version 150
                in vec2 v_tex_coord;
                out vec4 color;
                uniform sampler2D tex;
                uniform ivec2 viewport_info[2];
                uniform int scale_factor;
                void main() {
                    int tex_h      = viewport_info[0].y;
                    int win_y_base = viewport_info[1].y;
                    int scale      = max(scale_factor, 1);
                    int x = int(gl_FragCoord.x) / scale;
                    int y = (tex_h - 1) - ((int(gl_FragCoord.y) - win_y_base) / scale);
                    color = texelFetch(tex, ivec2(x, y), 0);
                }
            ");
            gl.compile_shader(fs);

            let mut linked = false;
            if gl.get_shader_compile_status(fs) {
                gl.attach_shader(program, vs);
                gl.attach_shader(program, fs);
                gl.link_program(program);
                if gl.get_program_link_status(program) {
                    linked = true;
                } else {
                    gl.detach_shader(program, fs);
                }
            }

            if !linked {
                gl.shader_source(fs, "
                    #version 150
                    in vec2 v_tex_coord;
                    out vec4 color;
                    uniform sampler2D tex;
                    void main() {
                        color = texture(tex, v_tex_coord);
                    }
                ");
                gl.compile_shader(fs);
                if !gl.get_shader_compile_status(fs) {
                    panic!("Fragment shader compilation failed: {}", gl.get_shader_info_log(fs));
                }
                gl.attach_shader(program, vs);
                gl.attach_shader(program, fs);
                gl.link_program(program);
                if !gl.get_program_link_status(program) {
                    panic!("Program linking failed: {}", gl.get_program_info_log(program));
                }
            }

            let viewport_info_loc = gl.get_uniform_location(program, "viewport_info");
            let scale_factor_loc  = gl.get_uniform_location(program, "scale_factor");

            let vao = gl.create_vertex_array().unwrap();
            gl.bind_vertex_array(Some(vao));
            gl.use_program(Some(program));

            let main_vbo = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(main_vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, VBO_SIZE, glow::DYNAMIC_DRAW);
            Self::bind_vbo_attribs(&gl, program, main_vbo);

            let status_vbo = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(status_vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, VBO_SIZE, glow::DYNAMIC_DRAW);
            Self::bind_vbo_attribs(&gl, program, status_vbo);

            (program, viewport_info_loc, scale_factor_loc, vao, main_vbo, status_vbo)
        };

        self.state = Some(GlState {
            gl,
            context: gl_context,
            surface: gl_surface,
            program,
            viewport_info_loc,
            scale_factor_loc,
            vao,
            main_vbo,
            status_vbo,
        });
    }

    // Upload a quad covering NDC [x0..x1] × [y0..y1] with UV [u0..u1] × [v0..v1].
    unsafe fn upload_quad(gl: &glow::Context, vbo: glow::Buffer,
        x0: f32, y0: f32, x1: f32, y1: f32,
        u0: f32, v0: f32, u1: f32, v1: f32)
    {
        let vertices: [f32; 16] = [
            x0, y0,  u0, v1,
            x1, y0,  u1, v1,
            x0, y1,  u0, v0,
            x1, y1,  u1, v0,
        ];
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let u8_slice = std::slice::from_raw_parts(vertices.as_ptr() as *const u8, vertices.len() * 4);
        gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, u8_slice);
    }

    unsafe fn set_viewport_info(gl: &glow::Context, loc: Option<&glow::UniformLocation>,
        tex_w: i32, tex_h: i32, win_y_base: i32)
    {
        let info = [tex_w, tex_h, 0, win_y_base];
        gl.uniform_2_i32_slice(loc, &info);
    }

    unsafe fn set_scale_factor(gl: &glow::Context, loc: Option<&glow::UniformLocation>, scale: u32) {
        if let Some(loc) = loc {
            gl.uniform_1_i32(Some(loc), scale as i32);
        }
    }

    unsafe fn bind_vbo_attribs(gl: &glow::Context, program: glow::Program, vbo: glow::Buffer) {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let pos_loc = gl.get_attrib_location(program, "position").unwrap();
        gl.enable_vertex_attrib_array(pos_loc);
        gl.vertex_attrib_pointer_f32(pos_loc, 2, glow::FLOAT, false, 16, 0);
        if let Some(tex_loc) = gl.get_attrib_location(program, "tex_coord") {
            gl.enable_vertex_attrib_array(tex_loc);
            gl.vertex_attrib_pointer_f32(tex_loc, 2, glow::FLOAT, false, 16, 8);
        }
    }

    // Draw a texture onto a quad, setting the appropriate viewport_info uniform.
    unsafe fn draw_tex(
        gl: &glow::Context,
        program: glow::Program,
        vao: glow::VertexArray,
        vbo: glow::Buffer,
        viewport_info_loc: Option<&glow::UniformLocation>,
        scale_factor_loc: Option<&glow::UniformLocation>,
        tex: glow::Texture,
        tex_w: i32, tex_h: i32,
        win_y_base: i32,
        scale: u32,
    ) {
        gl.use_program(Some(program));
        gl.bind_vertex_array(Some(vao));
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        Self::bind_vbo_attribs(gl, program, vbo);
        Self::set_viewport_info(gl, viewport_info_loc, tex_w, tex_h, win_y_base);
        Self::set_scale_factor(gl, scale_factor_loc, scale);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }
}

impl Renderer for GlRenderer {
    fn present(
        &mut self,
        screen:  &mut Rex3Screen,
        overlay: &mut DebugOverlay,
        status:  &mut StatusBar,
        sbtex:   &mut StatusBarTexture,
        stats:   &BarStats,
    ) {
        if self.state.is_none() {
            self.init_gl();
        }

        let width  = screen.width;
        let height = screen.height;
        if width == 0 || height == 0 { return; }

        let state = self.state.as_mut().unwrap();
        let gl    = &state.gl;

        // Handle window resize
        let win_h = if let Some((w, h)) = self.window_size.lock().take() {
            state.surface.resize(
                &state.context,
                NonZeroU32::new(w).unwrap(),
                NonZeroU32::new(h).unwrap(),
            );
            unsafe { gl.viewport(0, 0, w as i32, h as i32); }
            h as usize
        } else {
            (height + STATUS_BAR_HEIGHT) * self.scale as usize
        };

        // Recompute quads when display resolution changes
        if width != self.current_w || height != self.current_h {
            self.current_w = width;
            self.current_h = height;
            let max_u      = width  as f32 / 2048.0;
            let max_v_main = height as f32 / 1024.0;
            let sb_ndc_top = -1.0 + 2.0 * (STATUS_BAR_HEIGHT * self.scale as usize) as f32 / win_h as f32;
            unsafe {
                Self::upload_quad(gl, state.main_vbo,
                    -1.0, sb_ndc_top, 1.0, 1.0,
                    0.0, 0.0, max_u, max_v_main);
                Self::upload_quad(gl, state.status_vbo,
                    -1.0, -1.0, 1.0, sb_ndc_top,
                    0.0, 0.0, max_u, 1.0);
            }
        }

        unsafe {
            gl.use_program(Some(state.program));
            gl.bind_vertex_array(Some(state.vao));

            // ── Pass 1: compositor → main texture (opaque) ──────────────────
            gl.disable(glow::BLEND);
            let src      = screen.compositor_source();
            let main_tex = self.compositor.compose(&src, gl);
            // Write back CPU pixels for screenshots / CI reads
            if let Some(pixels) = self.compositor.read_pixels() {
                screen.rgba.copy_from_slice(pixels);
            }
            Self::draw_tex(
                gl, state.program, state.vao, state.main_vbo,
                state.viewport_info_loc.as_ref(), state.scale_factor_loc.as_ref(),
                main_tex,
                width as i32, height as i32,
                (STATUS_BAR_HEIGHT as u32 * self.scale) as i32,
                self.scale,
            );

            // ── Pass 2: debug overlay (alpha-blended) ────────────────────────
            if overlay.active() {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                let ov_src   = screen.overlay_source();
                let ov_tex   = overlay.render(&ov_src, gl);
                Self::draw_tex(
                    gl, state.program, state.vao, state.main_vbo,
                    state.viewport_info_loc.as_ref(), state.scale_factor_loc.as_ref(),
                    ov_tex,
                    width as i32, height as i32,
                    (STATUS_BAR_HEIGHT as u32 * self.scale) as i32,
                    self.scale,
                );
                gl.disable(glow::BLEND);
            }

            // ── Pass 3: status bar (opaque, bottom of window) ────────────────
            let sb_tex = sbtex.render_and_upload(status, stats, width, gl);
            Self::draw_tex(
                gl, state.program, state.vao, state.status_vbo,
                state.viewport_info_loc.as_ref(), state.scale_factor_loc.as_ref(),
                sb_tex,
                width as i32, STATUS_BAR_HEIGHT as i32,
                0,
                self.scale,
            );

            state.surface.swap_buffers(&state.context).unwrap();
        }
    }

    fn resize(&mut self, width: usize, height: usize) {
        let _ = self.window.request_inner_size(winit::dpi::PhysicalSize::new(
            width as u32 * self.scale,
            (height + STATUS_BAR_HEIGHT) as u32 * self.scale,
        ));
    }

    fn stop(&mut self) {
        if let Some(state) = self.state.take() {
            self.compositor.destroy(&state.gl);
        }
        self.current_w = 0;
        self.current_h = 0;
    }

    fn screenshot_pixels(&self) -> Option<Vec<u32>> {
        self.compositor.read_pixels().map(|s| s.to_vec())
    }
}

struct MouseDelta {
    accum: (f64, f64),
    wheel: f64,
    buttons: u8,
}

/// UI Manager handling Window, OpenGL context, and Input
pub struct Ui {
    ps2: Arc<Ps2Controller>,
    rex3: Arc<Rex3>,
    window: Arc<Window>,
    window_size: Arc<Mutex<Option<(u32, u32)>>>,
    timer_manager: Arc<TimerManager>,
    scale: u32,
    scroll_pixels_per_line: f64,
}

impl Ui {
    pub fn new(ps2: Arc<Ps2Controller>, rex3: Arc<Rex3>, timer_manager: Arc<TimerManager>, event_loop: &EventLoop<()>, scale: u32, scroll_pixels_per_line: f64) -> Self {
        let w = 1024 * scale;
        let h = (768 + STATUS_BAR_HEIGHT as u32) * scale;
        let window_builder = WindowBuilder::new()
            .with_title(crate::machine::emulator_name())
            .with_resizable(false)
            .with_enabled_buttons(winit::window::WindowButtons::CLOSE | winit::window::WindowButtons::MINIMIZE)
            .with_inner_size(winit::dpi::PhysicalSize::new(w, h));

        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_transparency(false);

        let display_builder = DisplayBuilder::new().with_window_builder(Some(window_builder));

        let (window, gl_config) = display_builder
            .build(event_loop, template, |configs| {
                configs
                    .reduce(|accum, config| {
                        if config.num_samples() > accum.num_samples() { config } else { accum }
                    })
                    .unwrap()
            })
            .unwrap();

        let window      = Arc::new(window.unwrap());
        let window_size = Arc::new(Mutex::new(None));

        let renderer = GlRenderer {
            window:      window.clone(),
            gl_config,
            window_size: window_size.clone(),
            state:       None,
            compositor:  SwCompositor::new(),
            current_w:   0,
            current_h:   0,
            scale,
        };

        *rex3.renderer.lock() = Some(Box::new(renderer));

        Self { ps2, rex3, window, window_size, timer_manager, scale, scroll_pixels_per_line }
    }

    /// Run the UI event loop (blocks the current thread)
    pub fn run(self, event_loop: EventLoop<()>) {
        let Ui { ps2, rex3, window, window_size, timer_manager, scale, scroll_pixels_per_line } = self;

        let mut mouse_grabbed = false;
        let mut rctrl_held = false;
        let mouse_delta = Arc::new(Mutex::new(MouseDelta { accum: (0.0, 0.0), wheel: 0.0, buttons: 0 }));

        {
            let ps2   = ps2.clone();
            let delta = mouse_delta.clone();
            timer_manager.add_recurring(
                Instant::now() + Duration::from_millis(10),
                Duration::from_millis(10),
                (ps2, delta),
                |(ps2, delta)| {
                    Self::flush_mouse_delta(ps2, delta, true);
                    TimerReturn::Continue
                },
            );
        }

        event_loop.set_control_flow(ControlFlow::Wait);
        event_loop.run(move |event, elwt| {
            match event {
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => { elwt.exit() },
                    WindowEvent::Resized(size) => {
                        if size.width != 0 && size.height != 0 {
                            *window_size.lock() = Some((size.width, size.height));
                        }
                    }
                    WindowEvent::KeyboardInput { event, .. } => {
                        Self::handle_keyboard(&ps2, &rex3, event, &mut mouse_grabbed, &mut rctrl_held, &window);
                    }
                    WindowEvent::MouseInput { state, button, .. } => {
                        if mouse_grabbed {
                            let pressed = state == ElementState::Pressed;
                            let mask = match button {
                                MouseButton::Left   => 1,
                                MouseButton::Right  => 2,
                                MouseButton::Middle => 4,
                                _ => 0,
                            };
                            if mask != 0 {
                                let mut md = mouse_delta.lock();
                                if pressed { md.buttons |= mask; } else { md.buttons &= !mask; }
                                drop(md);
                                Self::flush_mouse_delta(&ps2, &mouse_delta, false);
                            }
                        } else if state == ElementState::Pressed && button == MouseButton::Left {
                            mouse_grabbed = true;
                            if window.set_cursor_grab(winit::window::CursorGrabMode::Locked).is_err() {
                                let _ = window.set_cursor_grab(winit::window::CursorGrabMode::Confined);
                            }
                            window.set_cursor_visible(false);
                            mouse_delta.lock().accum = (0.0, 0.0);
                        }
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        if mouse_grabbed {
                            let lines = match delta {
                                winit::event::MouseScrollDelta::LineDelta(_, y) => y as f64,
                                winit::event::MouseScrollDelta::PixelDelta(p)  => p.y / scroll_pixels_per_line,
                            };
                            mouse_delta.lock().wheel += lines;
                        }
                    }
                    WindowEvent::Focused(false) => {
                        if mouse_grabbed {
                            mouse_grabbed = false;
                            let _ = window.set_cursor_grab(winit::window::CursorGrabMode::None);
                            window.set_cursor_visible(true);
                        }
                    }
                    WindowEvent::RedrawRequested => {
                        // Rendering is driven by the Rex3 refresh thread
                    }
                    _ => (),
                },
                Event::DeviceEvent { event: winit::event::DeviceEvent::MouseMotion { delta }, .. } => {
                    if mouse_grabbed {
                        let mut md = mouse_delta.lock();
                        md.accum.0 += delta.0 / scale as f64;
                        md.accum.1 += delta.1 / scale as f64;
                    }
                },
                _ => (),
            }
        }).unwrap();
    }

    fn flush_mouse_delta(ps2: &Ps2Controller, mouse_delta: &Mutex<MouseDelta>, require_motion: bool) {
        let mut md = mouse_delta.lock();
        let dx = md.accum.0 as i32;
        let dy = md.accum.1 as i32;
        let dz = md.wheel as i32;
        if require_motion && dx == 0 && dy == 0 && dz == 0 { return; }
        md.accum.0 -= dx as f64;
        md.accum.1 -= dy as f64;
        md.wheel   -= dz as f64;
        let buttons = md.buttons;
        drop(md);
        ps2.push_mouse_input(buttons, dx, dy, dz);
    }

    fn handle_keyboard(ps2: &Ps2Controller, rex3: &Rex3, input: KeyEvent, grabbed: &mut bool, rctrl_held: &mut bool, window: &Window) {
        use std::sync::atomic::Ordering;
        if let PhysicalKey::Code(keycode) = input.physical_key {
            let pressed = input.state == ElementState::Pressed;

            if keycode == KeyCode::ControlRight {
                *rctrl_held = pressed;
                if pressed && !input.repeat && *grabbed {
                    *grabbed = false;
                    let _ = window.set_cursor_grab(winit::window::CursorGrabMode::None);
                    window.set_cursor_visible(true);
                }
                return;
            }

            if keycode == KeyCode::PrintScreen && pressed && !input.repeat && *rctrl_held {
                rex3.screenshot_pending.store(true, Ordering::Relaxed);
                return;
            }

            ps2.push_kb(keycode, pressed);
        }
    }
}
