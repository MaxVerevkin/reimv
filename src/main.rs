#![allow(clippy::field_reassign_with_default)]

mod globals;
mod image;
mod window;

use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use crate::image::{Image, ImageTransform};
use globals::Globals;
use wayrs_utils::timer::Timer;
use window::Window;

use wayrs_client::global::{Global, GlobalExt};
use wayrs_client::protocol::*;
use wayrs_client::proxy::Proxy;
use wayrs_client::{Connection, IoMode};
use wayrs_protocols::pointer_gestures_unstable_v1::*;
use wayrs_utils::cursor::{CursorImage, CursorShape, CursorTheme, ThemedPointer};
use wayrs_utils::keyboard::{xkb, Keyboard, KeyboardEvent, KeyboardHandler};
use wayrs_utils::seats::{SeatHandler, Seats};
use wayrs_utils::shm_alloc::ShmAlloc;

use anyhow::{bail, Result};
use clap::Parser;

type EventCtx<'a, P> = wayrs_client::EventCtx<'a, State, P>;

/// Simple native Wayland image viewer that works
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct CliArgs {
    /// The path of the image
    file: String,
}

fn main() -> Result<()> {
    let cli_args = CliArgs::parse();

    let (mut conn, wl_globals) = Connection::connect_and_collect_globals()?;
    conn.add_registry_cb(wl_registry_cb);

    let globals = Globals::bind(&mut conn, &wl_globals)?;
    let mut shm_alloc = ShmAlloc::bind(&mut conn, &wl_globals)?;
    let window = Window::new(&mut conn, &globals, format!("{} - reimv", cli_args.file));

    let backend = Image::from_file(
        &cli_args.file,
        window.surface,
        &globals,
        &mut shm_alloc,
        &mut conn,
    )?;
    let cursor_theme = CursorTheme::new(&mut conn, &wl_globals, globals.wl_compositor);

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
        kbd_repeat: None,
    };

    wl_globals
        .iter()
        .filter(|g| g.is::<WlOutput>())
        .for_each(|g| state.bind_output(&mut conn, g));

    conn.flush(IoMode::Blocking)?;

    while !state.window.closed {
        let timeout = state.kbd_repeat.as_ref().map(|k| k.timer.sleep());
        poll(conn.as_raw_fd(), timeout)?;

        if let Some(repeat) = &mut state.kbd_repeat {
            if repeat.timer.tick() {
                let action = repeat.action;
                state.handle_action(&mut conn, action);
            }
        }

        match conn.recv_events(IoMode::NonBlocking) {
            Ok(()) => (),
            Err(e) if e.kind() == ErrorKind::WouldBlock => (),
            Err(e) => bail!(e),
        }

        conn.dispatch_events(&mut state);
        conn.flush(IoMode::Blocking)?;
    }

    Ok(())
}

fn poll(fd: RawFd, timeout: Option<Duration>) -> io::Result<()> {
    let mut fds = [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }];

    let result = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            1,
            timeout.map_or(-1, |t| t.as_secs() as _),
        )
    };

    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
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
    kbd_repeat: Option<RepeatState>,
}

pub struct RepeatState {
    key: xkb::Keycode,
    action: Action,
    timer: Timer,
}

impl State {
    pub fn handle_action(&mut self, conn: &mut Connection<Self>, action: Action) {
        match action {
            Action::MoveLeft => self.img_transform.x += self.window.width as f32 * 0.05,
            Action::MoveRight => self.img_transform.x -= self.window.width as f32 * 0.05,
            Action::MoveUp => self.img_transform.y += self.window.height as f32 * 0.05,
            Action::MoveDown => self.img_transform.y -= self.window.height as f32 * 0.05,
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
        Window::frame(self, conn);
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
                x: self.window.width as f32 / 2.0,
                y: self.window.height as f32 / 2.0,
                val: 10.0,
            },
            "+" => Action::Zoom {
                x: self.window.width as f32 / 2.0,
                y: self.window.height as f32 / 2.0,
                val: -10.0,
            },
            "f" => Action::ToggleFullscreen,
            _ => return,
        };

        if let Some(info) = event.repeat_info {
            if event.xkb_state.get_keymap().key_repeats(event.keycode) {
                self.kbd_repeat = Some(RepeatState {
                    key: event.keycode,
                    action,
                    timer: info.timer(),
                });
            }
        }

        self.handle_action(conn, action);
    }

    fn key_released(&mut self, _: &mut Connection<Self>, event: KeyboardEvent) {
        if self.kbd_repeat.as_ref().map(|r| r.key) == Some(event.keycode) {
            self.kbd_repeat = None;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Zoom { x: f32, y: f32, val: f32 },
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
    pinch_gesture: Option<PinchGesture>,
    enter_serial: u32,
    x: f32,
    y: f32,
}

struct PinchGesture {
    wl: ZwpPointerGesturePinchV1,
    state: Option<PinchGestureState>,
}

struct PinchGestureState {
    prev_scale: f32,
    fallback_transform: ImageTransform,
}

impl PinchGesture {
    fn new(
        conn: &mut Connection<State>,
        gesures: ZwpPointerGesturesV1,
        pointer: WlPointer,
    ) -> Self {
        Self {
            wl: gesures.get_pinch_gesture_with_cb(conn, pointer, pointer_pinch_cb),
            state: None,
        }
    }
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
            pinch_gesture: self
                .globals
                .pointer_gestures
                .map(|pg| PinchGesture::new(conn, pg, wl_pointer)),
            enter_serial: 0,
            x: 0.0,
            y: 0.0,
        });
    }

    fn pointer_removed(&mut self, conn: &mut Connection<Self>, seat: WlSeat) {
        let i = self.pointers.iter().position(|p| p.seat == seat).unwrap();
        let ptr = self.pointers.swap_remove(i);
        ptr.themed.destroy(conn);
        if let Some(pinch) = ptr.pinch_gesture {
            pinch.wl.destroy(conn);
        }
        if ptr.wl.version() >= 3 {
            ptr.wl.release(conn);
        }
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

fn wl_output_cb(ctx: EventCtx<WlOutput>) {
    if let wl_output::Event::Scale(scale) = ctx.event {
        let output = ctx
            .state
            .outputs
            .iter_mut()
            .find(|o| o.wl == ctx.proxy)
            .unwrap();
        output.scale = scale.try_into().unwrap();
        if ctx.state.window.outputs.contains(&ctx.proxy.id()) {
            Window::frame(ctx.state, ctx.conn);
        }
    }
}

fn wl_pointer_cb(ctx: EventCtx<WlPointer>) {
    const LEFT_PTR_BUTTON: u32 = 272;

    let gui_scale = ctx.state.window.get_int_scale(ctx.state);

    let ptr = ctx
        .state
        .pointers
        .iter_mut()
        .find(|s| s.wl == ctx.proxy)
        .unwrap();

    match ctx.event {
        wl_pointer::Event::Enter(args) => {
            assert_eq!(args.surface, ctx.state.window.surface.id());
            ptr.enter_serial = args.serial;
            ptr.x = args.surface_x.as_f32();
            ptr.y = args.surface_y.as_f32();
            ptr.themed.set_cursor(
                ctx.conn,
                &mut ctx.state.shm_alloc,
                &ctx.state.default_cursor,
                gui_scale,
                ptr.enter_serial,
            );
        }
        wl_pointer::Event::Leave(args) => {
            assert_eq!(args.surface, ctx.state.window.surface.id());
            if let Some(mt) = &mut ctx.state.move_transaction {
                if mt.wl_seat == ptr.seat {
                    ctx.state.move_transaction = None;
                }
            }
        }
        wl_pointer::Event::Motion(args) => {
            let x = args.surface_x.as_f32();
            let y = args.surface_y.as_f32();
            let dx = x - ptr.x;
            let dy = y - ptr.y;
            ptr.x = x;
            ptr.y = y;
            if let Some(mt) = &mut ctx.state.move_transaction {
                if mt.wl_seat == ptr.seat {
                    ctx.state.img_transform.x += dx;
                    ctx.state.img_transform.y += dy;
                    Window::frame(ctx.state, ctx.conn);
                }
            }
        }
        wl_pointer::Event::Button(args) => {
            match (args.button, args.state, &mut ctx.state.move_transaction) {
                (LEFT_PTR_BUTTON, wl_pointer::ButtonState::Pressed, None) => {
                    ctx.state.move_transaction = Some(MoveTransaction { wl_seat: ptr.seat });
                    ptr.themed.set_cursor(
                        ctx.conn,
                        &mut ctx.state.shm_alloc,
                        &ctx.state.move_cursor,
                        gui_scale,
                        ptr.enter_serial,
                    );
                }
                (LEFT_PTR_BUTTON, wl_pointer::ButtonState::Released, Some(mt))
                    if mt.wl_seat == ptr.seat =>
                {
                    ptr.themed.set_cursor(
                        ctx.conn,
                        &mut ctx.state.shm_alloc,
                        &ctx.state.default_cursor,
                        gui_scale,
                        ptr.enter_serial,
                    );
                    ctx.state.move_transaction = None;
                }
                _ => (),
            }
        }
        wl_pointer::Event::Axis(args) => {
            if args.axis == wl_pointer::Axis::VerticalScroll
                && ctx
                    .state
                    .move_transaction
                    .map_or(true, |mt| mt.wl_seat == ptr.seat)
            {
                let (x, y) = (ptr.x, ptr.y);
                ctx.state.handle_action(
                    ctx.conn,
                    Action::Zoom {
                        x,
                        y,
                        val: args.value.as_f32(),
                    },
                );
            }
        }
        _ => (),
    }
}

fn pointer_pinch_cb(ctx: EventCtx<ZwpPointerGesturePinchV1>) {
    let gui_scale = ctx.state.window.get_int_scale(ctx.state);

    let ptr = ctx
        .state
        .pointers
        .iter_mut()
        .find(|s| {
            s.pinch_gesture
                .as_ref()
                .is_some_and(|pg| pg.wl == ctx.proxy)
        })
        .unwrap();

    let pg = ptr.pinch_gesture.as_mut().unwrap();

    use zwp_pointer_gesture_pinch_v1::Event;
    match (ctx.event, &mut pg.state) {
        (Event::Begin(args), _) if args.fingers == 2 => {
            pg.state = Some(PinchGestureState {
                prev_scale: 1.0,
                fallback_transform: ctx.state.img_transform,
            });
            ptr.themed.set_cursor(
                ctx.conn,
                &mut ctx.state.shm_alloc,
                &ctx.state.move_cursor,
                gui_scale,
                ptr.enter_serial,
            );
        }
        (Event::Update(args), Some(s)) => {
            let val = (args.scale.as_f32() - s.prev_scale) * -100.0;
            let (x, y) = (ptr.x, ptr.y);
            s.prev_scale = args.scale.as_f32();
            ctx.state.img_transform.x += args.dx.as_f32();
            ctx.state.img_transform.y += args.dy.as_f32();
            ctx.state
                .handle_action(ctx.conn, Action::Zoom { x, y, val });
        }
        (Event::End(args), Some(s)) => {
            ptr.themed.set_cursor(
                ctx.conn,
                &mut ctx.state.shm_alloc,
                &ctx.state.default_cursor,
                gui_scale,
                ptr.enter_serial,
            );
            if args.cancelled == 1 {
                ctx.state.img_transform = s.fallback_transform;
            }
            pg.state = None;
            Window::frame(ctx.state, ctx.conn);
        }
        _ => (),
    }
}
