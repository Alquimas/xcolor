use anyhow::{anyhow, Error, Result};
use nix::unistd::{self, fork, ForkResult};
use std::fs;
use std::io;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::str::FromStr;
use xcb::{x, Connection, Xid};

use crate::atoms;

pub fn into_daemon() -> Result<ForkResult> {
    match unsafe { fork() }? {
        parent @ ForkResult::Parent { .. } => Ok(parent),
        child @ ForkResult::Child => {
            unistd::setsid()?;
            std::env::set_current_dir("/")?;
            let dev_null = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")?
                .into_raw_fd();
            for fd in &[
                io::stdin().as_raw_fd(),
                io::stdout().as_raw_fd(),
                io::stderr().as_raw_fd(),
            ] {
                unistd::close(*fd)?;
                unistd::dup2(dev_null, *fd)?;
            }
            Ok(child)
        }
    }
}

pub enum Selection {
    Primary,
    Secondary,
    Clipboard,
}

impl FromStr for Selection {
    type Err = Error;
    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string {
            "primary" => Ok(Self::Primary),
            "secondary" => Ok(Self::Secondary),
            "clipboard" => Ok(Self::Clipboard),
            _ => Err(anyhow!("Invalid selection")),
        }
    }
}

impl Selection {
    fn to_atom(&self, conn: &Connection) -> Result<x::Atom> {
        Ok(match *self {
            Self::Primary => atoms::get(conn, "PRIMARY")?,
            Self::Secondary => atoms::get(conn, "SECONDARY")?,
            Self::Clipboard => atoms::get(conn, "CLIPBOARD")?,
        })
    }
}

pub fn set_selection(
    conn: &Connection,
    root: x::Window,
    selection: &Selection,
    string: &str,
) -> Result<()> {
    let selection = selection.to_atom(conn)?;
    let utf8_string = atoms::get(conn, "UTF8_STRING")?;
    let targets = atoms::get(conn, "TARGETS")?;
    let window: x::Window = conn.generate_id();

    conn.send_and_check_request(&x::CreateWindow {
        depth: x::COPY_FROM_PARENT as u8,
        wid: window,
        parent: root,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        border_width: 0,
        class: x::WindowClass::InputOnly,
        visual: x::COPY_FROM_PARENT,
        value_list: &[],
    })?;
    conn.send_and_check_request(&x::SetSelectionOwner {
        owner: window,
        selection,
        time: x::CURRENT_TIME,
    })?;
    let owner_cookie = conn.send_request(&x::GetSelectionOwner { selection });
    if conn.wait_for_reply(owner_cookie)?.owner() != window {
        return Err(anyhow!("Could not take selection ownership"));
    }

    loop {
        match conn.wait_for_event()? {
            xcb::Event::X(x::Event::SelectionRequest(event)) => {
                let target = event.target();
                let property = if target == utf8_string {
                    conn.send_and_check_request(&x::ChangeProperty {
                        mode: x::PropMode::Replace,
                        window: event.requestor(),
                        property: event.property(),
                        r#type: target,
                        data: string.as_bytes(),
                    })?;
                    event.property()
                } else if target == targets {
                    conn.send_and_check_request(&x::ChangeProperty {
                        mode: x::PropMode::Replace,
                        window: event.requestor(),
                        property: event.property(),
                        r#type: target,
                        data: &[targets, utf8_string],
                    })?;
                    event.property()
                } else {
                    x::Atom::none()
                };
                let response = x::SelectionNotifyEvent::new(
                    event.time(),
                    event.requestor(),
                    event.selection(),
                    target,
                    property,
                );
                conn.send_and_check_request(&x::SendEvent {
                    propagate: false,
                    destination: x::SendEventDest::Window(event.requestor()),
                    event_mask: x::EventMask::NO_EVENT,
                    event: &response,
                })?;
            }
            xcb::Event::X(x::Event::SelectionClear(_)) => break,
            _ => {}
        }
    }
    Ok(())
}
