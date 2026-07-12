//! Linux backend: drive the tray from a GTK main loop.
//!
//! The tray icon uses libappindicator / StatusNotifierItem, which require a
//! running GTK main loop on the same thread the icon was created on. We poll the
//! muda menu-event channel and run the periodic refresh from GTK timeouts.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use gtk::glib;
use muda::MenuEvent;

use crate::controller::{Controller, Dispatch, REFRESH_INTERVAL};

/// How often to drain the menu-event channel.
const MENU_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn run() -> Result<()> {
    gtk::init().context("failed to initialize GTK for the system tray")?;

    let controller = Rc::new(RefCell::new(Controller::new()?));
    controller.borrow_mut().maybe_autostart_daemon();

    // Drain menu clicks on the GTK loop.
    {
        let controller = controller.clone();
        glib::timeout_add_local(MENU_POLL_INTERVAL, move || {
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if controller.borrow_mut().handle_menu(&event.id.0) == Dispatch::Quit {
                    gtk::main_quit();
                    return glib::ControlFlow::Break;
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Periodic state refresh.
    {
        let controller = controller.clone();
        glib::timeout_add_local(REFRESH_INTERVAL, move || {
            controller.borrow_mut().refresh();
            glib::ControlFlow::Continue
        });
    }

    gtk::main();
    Ok(())
}
