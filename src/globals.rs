use wayrs_client::global::{BindError, Global, GlobalsExt};
use wayrs_client::protocol::*;
use wayrs_client::{Connection, EventCtx};
use wayrs_protocols::fractional_scale_v1::*;
use wayrs_protocols::pointer_gestures_unstable_v1::*;
use wayrs_protocols::single_pixel_buffer_v1::*;
use wayrs_protocols::viewporter::*;
use wayrs_protocols::xdg_decoration_unstable_v1::*;
use wayrs_protocols::xdg_shell::*;

pub struct Globals {
    pub wl_compositor: WlCompositor,
    pub wl_subcompositor: WlSubcompositor,
    pub xdg_wm_base: XdgWmBase,
    pub wp_viewporter: WpViewporter,
    pub single_pixel_buffer_manager: WpSinglePixelBufferManagerV1,
    pub wp_fractional_scale_manager: Option<WpFractionalScaleManagerV1>,
    pub xdg_decoration_manager: Option<ZxdgDecorationManagerV1>,
    pub pointer_gestures: Option<ZwpPointerGesturesV1>,
}

impl Globals {
    pub fn bind<D: 'static>(
        conn: &mut Connection<D>,
        globals: &[Global],
    ) -> Result<Self, BindError> {
        Ok(Self {
            wl_compositor: globals.bind(conn, 1..=5)?,
            wl_subcompositor: globals.bind(conn, 1..=1)?,
            xdg_wm_base: globals.bind_with_cb(conn, 1..=5, xdg_wm_base_cb)?,
            wp_viewporter: globals.bind(conn, 1..=1)?,
            single_pixel_buffer_manager: globals.bind(conn, 1..=1)?,
            wp_fractional_scale_manager: globals.bind(conn, 1..=1).ok(),
            xdg_decoration_manager: globals.bind(conn, 1..=1).ok(),
            pointer_gestures: globals.bind(conn, 1..=3).ok(),
        })
    }
}

fn xdg_wm_base_cb<D>(ctx: EventCtx<D, XdgWmBase>) {
    if let xdg_wm_base::Event::Ping(serial) = ctx.event {
        ctx.proxy.pong(ctx.conn, serial);
    }
}
