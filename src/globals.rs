use wayrs_client::connection::Connection;
use wayrs_client::global::{BindError, Global, GlobalsExt};

use wayrs_client::protocol::*;
use wayrs_protocols::fractional_scale_v1::*;
use wayrs_protocols::viewporter::*;
use wayrs_protocols::xdg_decoration_unstable_v1::*;
use wayrs_protocols::xdg_shell::*;

pub struct Globals {
    pub wl_shm: WlShm,
    pub wl_compositor: WlCompositor,
    pub wl_subcompositor: WlSubcompositor,
    pub xdg_wm_base: XdgWmBase,
    pub wp_viewporter: WpViewporter,
    pub wp_fractional_scale_manager: Option<WpFractionalScaleManagerV1>,
    pub xdg_decoration_manager: Option<ZxdgDecorationManagerV1>,
}

impl Globals {
    pub fn bind<D: 'static>(
        conn: &mut Connection<D>,
        globals: &[Global],
    ) -> Result<Self, BindError> {
        Ok(Self {
            wl_shm: globals.bind(conn, 1..=1)?,
            wl_compositor: globals.bind(conn, 1..=5)?,
            wl_subcompositor: globals.bind(conn, 1..=1)?,
            xdg_wm_base: globals.bind_with_cb(conn, 1..=5, xdg_wm_base_cb)?,
            wp_viewporter: globals.bind(conn, 1..=1)?,
            wp_fractional_scale_manager: globals.bind(conn, 1..=1).ok(),
            xdg_decoration_manager: globals.bind(conn, 1..=1).ok(),
        })
    }
}

fn xdg_wm_base_cb<D>(
    conn: &mut Connection<D>,
    _: &mut D,
    xdg_wm_base: XdgWmBase,
    event: xdg_wm_base::Event,
) {
    if let xdg_wm_base::Event::Ping(serial) = event {
        xdg_wm_base.pong(conn, serial);
    }
}
