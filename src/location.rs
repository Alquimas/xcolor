use anyhow::{anyhow, Result};
use x11::xcursor::{XcursorImageCreate, XcursorImageDestroy, XcursorImageLoadCursor};
use xcb::{x, Connection, Xid, XidNew};

use crate::color::{self, ARGB};
use crate::draw::{draw_color_text, draw_magnifying_glass};
use crate::pixel::PixelSquare;
use crate::util::EnsureOdd;

// Left mouse button
const SELECTION_BUTTON: u8 = 1;
// ESC key code
const ESC_KEY: u8 = 9;
fn grab_mask() -> x::EventMask {
    x::EventMask::BUTTON_PRESS | x::EventMask::POINTER_MOTION
}

// Exclusively grabs the pointer so we get all its events
fn grab_pointer(conn: &Connection, root: x::Window, cursor: u32) -> Result<()> {
    let cookie = conn.send_request(&x::GrabPointer {
        owner_events: false,
        grab_window: root,
        event_mask: grab_mask(),
        pointer_mode: x::GrabMode::Async,
        keyboard_mode: x::GrabMode::Async,
        confine_to: x::Window::none(),
        cursor: x::Cursor::new(cursor),
        time: x::CURRENT_TIME,
    });
    let reply = conn.wait_for_reply(cookie)?;

    if reply.status() != x::GrabStatus::Success {
        return Err(anyhow!("Could not grab pointer"));
    }

    Ok(())
}

// Exclusively grabs the keyboard so we get all its events
fn grab_keyboard(conn: &Connection, root: x::Window) -> Result<()> {
    let cookie = conn.send_request(&x::GrabKeyboard {
        owner_events: false,
        grab_window: root,
        time: x::CURRENT_TIME,
        pointer_mode: x::GrabMode::Async,
        keyboard_mode: x::GrabMode::Async,
    });
    let reply = conn.wait_for_reply(cookie)?;

    if reply.status() != x::GrabStatus::Success {
        return Err(anyhow!("Could not grab keyboard"));
    }

    Ok(())
}

// Updates the cursor for an _already grabbed pointer_
fn update_cursor(conn: &Connection, cursor: u32) -> Result<()> {
    conn.send_and_check_request(&x::ChangeActivePointerGrab {
        cursor: x::Cursor::new(cursor),
        time: x::CURRENT_TIME,
        event_mask: grab_mask(),
    })?;

    Ok(())
}

// Creates a new `XcursorImage`, draws the picker into it and loads it, returning the id for a `Cursor`
fn create_new_xcursor(
    conn: &Connection,
    screenshot_pixels: &PixelSquare<&[ARGB]>,
    preview_width: u32,
    center_color: ARGB,
) -> Result<u32> {
    Ok(unsafe {
        let cursor_image = XcursorImageCreate(preview_width as i32, preview_width as i32);

        // set the "hot spot" - this is where the pointer actually is inside the image
        (*cursor_image).xhot = preview_width / 2;
        (*cursor_image).yhot = preview_width / 2;

        // get pixel data as a mutable Rust slice
        let mut cursor_pixels =
            PixelSquare::from_raw_parts((*cursor_image).pixels, preview_width as usize);

        // find out how large our pixels should be in the picker - this must be an odd number (so
        // there's a center pixel) and it must be slightly higher than the ratio between the
        // cursor and the screenshot (to account for integer division so no out of bounds accesses
        // occur when upscaling the image in `draw_magnifying_glass`)
        let mut pixel_size = cursor_pixels.width() / screenshot_pixels.width();
        if pixel_size % 2 == 0 {
            pixel_size += 1;
        } else {
            pixel_size += 2;
        }

        // draw our custom image
        draw_magnifying_glass(&mut cursor_pixels, screenshot_pixels, pixel_size);

        // Draw color text on the cursor
        draw_color_text(&mut cursor_pixels, center_color);

        // convert our XcursorImage into a cursor
        let cursor_id = XcursorImageLoadCursor(conn.get_raw_dpy(), cursor_image) as u32;

        // free the XcursorImage
        XcursorImageDestroy(cursor_image);

        cursor_id
    } as u32)
}

// NOTE: this works for multi-monitor configurations since it seems that X fills in the blank
// space with empty pixels when calling XGetImage with a rect that crosses the boundaries of two differently
// sized or misaligned screens
fn get_window_rect_around_pointer(
    conn: &Connection,
    screen: &x::Screen,
    (pointer_x, pointer_y): (i16, i16),
    preview_width: u32,
    scale: u32,
) -> Result<(u16, Vec<ARGB>)> {
    let root = screen.root();
    let root_width = screen.width_in_pixels() as isize;
    let root_height = screen.height_in_pixels() as isize;

    let size = ((preview_width / scale) as isize).ensure_odd();

    // the top left coordinates of the rect: make sure they don't go offscreen
    let mut x = (pointer_x as isize) - (size / 2);
    let mut y = (pointer_y as isize) - (size / 2);
    let x_offset = if x < 0 { -x } else { 0 };
    let y_offset = if y < 0 { -y } else { 0 };
    x += x_offset;
    y += y_offset;

    // the size of the rect: make sure they don't extend past the screen
    let size_x = if x + size > (root_width) {
        (root_width) - x
    } else {
        size - x_offset
    };
    let size_y = if y + size > (root_height) {
        (root_height) - y
    } else {
        size - y_offset
    };

    // grab a screenshot of the rect
    let rect = (x as i16, y as i16, size_x as u16, size_y as u16);
    let screenshot_rect = color::window_rect(conn, root, rect)?;

    // the entire portion of the screenshot is on screen
    if size_x == size && size_y == size {
        return Ok((size as u16, screenshot_rect));
    }

    // NOTE: XCB APIs fail when requesting a region outside the screen, so clamp the rect to the screen and
    // fill the clamped pixels with empty data
    let mut pixels = vec![ARGB::TRANSPARENT; (size * size) as usize];
    for x in 0..size_x {
        for y in 0..size_y {
            let screenshot_idx = (y * size_x) + x;
            let pixels_idx = (y + y_offset) * size + (x + x_offset);

            pixels[pixels_idx as usize] = screenshot_rect[screenshot_idx as usize];
        }
    }

    Ok((size as u16, pixels))
}

fn create_new_cursor(
    conn: &Connection,
    screen: &x::Screen,
    preview_width: u32,
    scale: u32,
    point: Option<(i16, i16)>,
) -> Result<u32> {
    let point = match point {
        Some(point) => point,
        None => {
            let root = screen.root();
            let cookie = conn.send_request(&x::QueryPointer { window: root });
            let pointer = conn.wait_for_reply(cookie)?;
            (pointer.root_x(), pointer.root_y())
        }
    };

    let (w, p) = get_window_rect_around_pointer(conn, screen, point, preview_width, scale)?;
    let pixels = PixelSquare::new(&p[..], w.into());

    // Get the center color (color at mouse position)
    let center_color = pixels[(pixels.width() / 2, pixels.width() / 2)];

    create_new_xcursor(conn, &pixels, preview_width, center_color)
}

pub fn wait_for_location(
    conn: &Connection,
    screen: &x::Screen,
    preview_width: u32,
    scale: u32,
) -> Result<Option<ARGB>> {
    let root = screen.root();
    let preview_width = preview_width.ensure_odd();

    // grab the cursor to listen to all of its events
    let mut cursor = create_new_cursor(conn, screen, preview_width, scale, None)?;
    grab_pointer(conn, root, cursor)?;

    // Try to grab keyboard, but don't fail if it doesn't work
    let keyboard_grabbed = grab_keyboard(conn, root).is_ok();

    let result = loop {
        match conn.wait_for_event()? {
            xcb::Event::X(x::Event::ButtonPress(event)) if event.detail() == SELECTION_BUTTON => {
                let pixels =
                    color::window_rect(conn, root, (event.root_x(), event.root_y(), 1, 1))?;
                break Some(pixels[0]);
            }
            xcb::Event::X(x::Event::KeyPress(event))
                if keyboard_grabbed && event.detail() == ESC_KEY =>
            {
                break None
            }
            xcb::Event::X(x::Event::MotionNotify(event)) => {
                let new_cursor = create_new_cursor(
                    conn,
                    screen,
                    preview_width,
                    scale,
                    Some((event.root_x(), event.root_y())),
                )?;
                update_cursor(conn, new_cursor)?;
                conn.send_request(&x::FreeCursor {
                    cursor: x::Cursor::new(cursor),
                });
                cursor = new_cursor;
            }
            _ => {}
        }
    };

    conn.send_request(&x::UngrabPointer {
        time: x::CURRENT_TIME,
    });
    if keyboard_grabbed {
        conn.send_request(&x::UngrabKeyboard {
            time: x::CURRENT_TIME,
        });
    }
    conn.send_request(&x::FreeCursor {
        cursor: x::Cursor::new(cursor),
    });
    conn.flush()?;

    Ok(result)
}
