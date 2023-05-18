mod globals;
mod image;
mod repeat;
mod window;

use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::time::Instant;

use crate::image::Image;
use globals::Globals;
use repeat::RepeatState;
use wayrs_client::global::{Global, GlobalExt};
use window::Window;

use wayrs_client::connection::Connection;
use wayrs_client::protocol::*;
use wayrs_client::proxy::Proxy;
use wayrs_client::IoMode;

use wayrs_utils::cursor::{CursorImage, CursorShape, CursorTheme, ThemedPointer};
use wayrs_utils::keyboard::{Keyboard, KeyboardEvent, KeyboardHandler};
use wayrs_utils::seats::{SeatHandler, Seats};
use wayrs_utils::shm_alloc::ShmAlloc;

use anyhow::{bail, Result};
use clap::Parser;

use nix::poll::{poll, PollFd, PollFlags};

/// Simple native Wayland image viewer that works
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct CliArgs {
    /// The path of the image
    file: String,
}

fn main() -> Result<()> {
    let cli_args = CliArgs::parse();
    let backend = Image::from_file(&cli_args.file)?;

    let mut conn = Connection::connect()?;
    let wl_globals = conn.blocking_collect_initial_globals()?;
    conn.add_registry_cb(wl_registry_cb);

    let globals = Globals::bind(&mut conn, &wl_globals)?;
    let shm_alloc = ShmAlloc::bind(&mut conn, &wl_globals)?;
    let window = Window::new(&mut conn, &globals);

    let cursor_theme = CursorTheme::new(&mut conn, &wl_globals);

    let mut state = State {
        globals,
        shm_alloc,
        backend,

        default_cursor: cursor_theme.get_image(CursorShape::Default)?,
        move_cursor: cursor_theme.get_image(CursorShape::Move)?,
        cursor_theme,

        seats: Seats::bind(&mut conn, &wl_globals),
        outputs: Vec::new(),

        keyboards: Vec::new(),
        pointers: Vec::new(),

        window,

        img_transform: ImageTransform {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
        },

        move_transaction: None,
        kbd_repeat: RepeatState::None,
    };

    wl_globals
        .iter()
        .filter(|g| g.is::<WlOutput>())
        .for_each(|g| state.bind_output(&mut conn, g));

    conn.flush(IoMode::Blocking)?;

    let mut poll_fds = [PollFd::new(conn.as_raw_fd(), PollFlags::POLLIN)];

    while !state.window.closed {
        poll(&mut poll_fds, state.kbd_repeat.timeout() as i32)?;

        if let Some(action) = state.kbd_repeat.tick() {
            state.handle_action(&mut conn, action);
        }

        if poll_fds[0].any().unwrap_or(true) {
            match conn.recv_events(IoMode::NonBlocking) {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                Err(e) => bail!(e),
            }
        }

        conn.dispatch_events(&mut state);
        conn.flush(IoMode::Blocking)?;
    }

    Ok(())
}

pub struct State {
    pub globals: Globals,
    pub shm_alloc: ShmAlloc,
    pub backend: Image,

    pub cursor_theme: CursorTheme,
    pub default_cursor: CursorImage,
    pub move_cursor: CursorImage,

    pub seats: Seats,
    pub outputs: Vec<Output>,

    pub keyboards: Vec<Keyboard>,
    pub pointers: Vec<Pointer>,

    window: Window,

    img_transform: ImageTransform,

    move_transaction: Option<MoveTransaction>,
    kbd_repeat: RepeatState,
}

impl State {
    pub fn handle_action(&mut self, conn: &mut Connection<Self>, action: Action) {
        match action {
            Action::MoveLeft => self.img_transform.x += self.window.width as f64 * 0.05,
            Action::MoveRight => self.img_transform.x -= self.window.width as f64 * 0.05,
            Action::MoveUp => self.img_transform.y += self.window.height as f64 * 0.05,
            Action::MoveDown => self.img_transform.y -= self.window.height as f64 * 0.05,
            Action::Zoom { x, y, val } => {
                // When zooming we want to move the image in such a way that the pointer's
                // coordinates in image lacal coordinates do not change. This can be expressed as
                // (x_ptr - x_img) / scale = (x_ptr - x_img_new) / scale_new,
                // where all coordinates are in surface-localal system. Similar for the y coordinate.
                let prev_scale = self.img_transform.scale;
                let delta_scale = val * prev_scale * -0.01;
                self.img_transform.x += (self.img_transform.x - x) * delta_scale / prev_scale;
                self.img_transform.y += (self.img_transform.y - y) * delta_scale / prev_scale;
                self.img_transform.scale += delta_scale;
            }
            Action::ToggleFullscreen => self.window.toggle_fullscreen(conn),
        }
        self.window.request_frame(conn);
    }

    pub fn bind_output(&mut self, conn: &mut Connection<Self>, global: &Global) {
        self.outputs.push(Output {
            reg_name: global.name,
            wl: global.bind_with_cb(conn, 1..=4, wl_output_cb).unwrap(),
            scale: 1,
        });
    }
}

impl KeyboardHandler for State {
    fn get_keyboard(&mut self, wl_keyboard: WlKeyboard) -> &mut Keyboard {
        self.keyboards
            .iter_mut()
            .find(|k| k.wl_keyboard() == wl_keyboard)
            .unwrap()
    }

    fn key_presed(&mut self, conn: &mut Connection<Self>, event: KeyboardEvent) {
        let action = match event.xkb_state.key_get_utf8(event.keycode).as_str() {
            "h" => Action::MoveLeft,
            "l" => Action::MoveRight,
            "k" => Action::MoveUp,
            "j" => Action::MoveDown,
            "-" => Action::Zoom {
                x: self.window.width as f64 / 2.0,
                y: self.window.height as f64 / 2.0,
                val: 10.0,
            },
            "+" => Action::Zoom {
                x: self.window.width as f64 / 2.0,
                y: self.window.height as f64 / 2.0,
                val: -10.0,
            },
            "f" => Action::ToggleFullscreen,
            _ => return,
        };

        if let Some(info) = event.repeat_info {
            if event.xkb_state.get_keymap().key_repeats(event.keycode) {
                self.kbd_repeat = RepeatState::Delay {
                    info,
                    delay_will_end: Instant::now() + info.delay,
                    action,
                    key: event.keycode,
                };
            }
        }

        self.handle_action(conn, action);
    }

    fn key_released(&mut self, _: &mut Connection<Self>, event: KeyboardEvent) {
        if self.kbd_repeat.key() == Some(event.keycode) {
            self.kbd_repeat = RepeatState::None;
        }
    }
}

pub struct ImageTransform {
    /// Y-offset in surface local coordinates
    x: f64,
    /// Y-offset in surface local coordinates
    y: f64,
    /// Scale
    scale: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Zoom { x: f64, y: f64, val: f64 },
    ToggleFullscreen,
}

#[derive(Clone, Copy)]
struct MoveTransaction {
    wl_seat: WlSeat,
}

pub struct Output {
    reg_name: u32,
    wl: WlOutput,
    scale: u32,
}

pub struct Pointer {
    seat: WlSeat,
    wl: WlPointer,
    themed: ThemedPointer,
    enter_serial: u32,
    x: f64,
    y: f64,
}

impl SeatHandler for State {
    fn get_seats(&mut self) -> &mut Seats {
        &mut self.seats
    }

    fn keyboard_added(&mut self, conn: &mut Connection<Self>, seat: WlSeat) {
        self.keyboards.push(Keyboard::new(conn, seat));
    }

    fn keyboard_removed(&mut self, conn: &mut Connection<Self>, seat: WlSeat) {
        let i = self
            .keyboards
            .iter()
            .position(|k| k.seat() == seat)
            .unwrap();
        let kbd = self.keyboards.swap_remove(i);
        kbd.destroy(conn);
    }

    fn pointer_added(&mut self, conn: &mut Connection<Self>, seat: WlSeat) {
        let wl_pointer = seat.get_pointer_with_cb(conn, wl_pointer_cb);
        self.pointers.push(Pointer {
            seat,
            wl: wl_pointer,
            themed: self.cursor_theme.get_themed_pointer(conn, wl_pointer),
            enter_serial: 0,
            x: 0.0,
            y: 0.0,
        });
    }

    fn pointer_removed(&mut self, conn: &mut Connection<Self>, seat: WlSeat) {
        let i = self.pointers.iter().position(|p| p.seat == seat).unwrap();
        let ptr = self.pointers.swap_remove(i);
        if ptr.wl.version() >= 3 {
            ptr.wl.release(conn);
        }
        ptr.themed.destroy(conn);
    }
}

fn wl_registry_cb(conn: &mut Connection<State>, state: &mut State, event: &wl_registry::Event) {
    match event {
        wl_registry::Event::Global(g) if g.is::<WlOutput>() => {
            state.bind_output(conn, g);
        }
        wl_registry::Event::GlobalRemove(name) => {
            if let Some(output_i) = state.outputs.iter().position(|o| o.reg_name == *name) {
                let output = state.outputs.swap_remove(output_i).wl;
                state.window.outputs.remove(&output.id());
                if output.version() >= 3 {
                    output.release(conn);
                }
            }
        }
        _ => (),
    }
}

fn wl_output_cb(
    conn: &mut Connection<State>,
    state: &mut State,
    wl_output: WlOutput,
    event: wl_output::Event,
) {
    if let wl_output::Event::Scale(scale) = event {
        let output = state
            .outputs
            .iter_mut()
            .find(|o| o.wl == wl_output)
            .unwrap();
        output.scale = scale.try_into().unwrap();
        if state.window.outputs.contains(&wl_output.id()) {
            state.window.request_frame(conn);
        }
    }
}

fn wl_pointer_cb(
    conn: &mut Connection<State>,
    state: &mut State,
    wl_pointer: WlPointer,
    event: wl_pointer::Event,
) {
    const LEFT_PTR_BUTTON: u32 = 272;

    let cursor_scale = state.window.get_int_scale(state);

    let ptr = state
        .pointers
        .iter_mut()
        .find(|s| s.wl == wl_pointer)
        .unwrap();

    match event {
        wl_pointer::Event::Enter(args) => {
            assert_eq!(args.surface, state.window.surface.id());
            ptr.enter_serial = args.serial;
            ptr.x = args.surface_x.as_f64();
            ptr.y = args.surface_y.as_f64();
            ptr.themed.set_cursor(
                conn,
                &mut state.shm_alloc,
                &state.default_cursor,
                cursor_scale,
                ptr.enter_serial,
            );
        }
        wl_pointer::Event::Leave(args) => {
            assert_eq!(args.surface, state.window.surface.id());
            if let Some(mt) = &mut state.move_transaction {
                if mt.wl_seat == ptr.seat {
                    state.move_transaction = None;
                }
            }
        }
        wl_pointer::Event::Motion(args) => {
            let x = args.surface_x.as_f64();
            let y = args.surface_y.as_f64();
            let dx = x - ptr.x;
            let dy = y - ptr.y;
            ptr.x = x;
            ptr.y = y;
            if let Some(mt) = &mut state.move_transaction {
                if mt.wl_seat == ptr.seat {
                    state.img_transform.x += dx;
                    state.img_transform.y += dy;
                    state.window.request_frame(conn);
                }
            }
        }
        wl_pointer::Event::Button(args) => {
            match (args.button, args.state, &mut state.move_transaction) {
                (LEFT_PTR_BUTTON, wl_pointer::ButtonState::Pressed, None) => {
                    state.move_transaction = Some(MoveTransaction { wl_seat: ptr.seat });
                    ptr.themed.set_cursor(
                        conn,
                        &mut state.shm_alloc,
                        &state.move_cursor,
                        cursor_scale,
                        ptr.enter_serial,
                    );
                }
                (LEFT_PTR_BUTTON, wl_pointer::ButtonState::Released, Some(mt))
                    if mt.wl_seat == ptr.seat =>
                {
                    ptr.themed.set_cursor(
                        conn,
                        &mut state.shm_alloc,
                        &state.default_cursor,
                        cursor_scale,
                        ptr.enter_serial,
                    );
                    state.move_transaction = None;
                }
                _ => (),
            }
        }
        wl_pointer::Event::Axis(args) => {
            if args.axis == wl_pointer::Axis::VerticalScroll
                && state
                    .move_transaction
                    .map_or(true, |mt| mt.wl_seat == ptr.seat)
            {
                let (x, y) = (ptr.x, ptr.y);
                state.handle_action(
                    conn,
                    Action::Zoom {
                        x,
                        y,
                        val: args.value.as_f64(),
                    },
                );
            }
        }
        _ => (),
    }
}
