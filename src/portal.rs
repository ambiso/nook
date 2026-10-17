//! Export host portals on a sandbox's private session bus.
//!
//! Each guest peer gets its own upstream connection. This preserves portal
//! request/session lifetimes and prevents one guest process closing another's
//! requests. libdbus owns and forwards Unix file descriptors; we never marshal
//! the wire protocol or implement ancillary-data handling ourselves.

use crate::config::Result;
use dbus::{
    Message, MessageType, Path, Signature,
    arg::messageitem::{MessageItem, MessageItemArray, MessageItemDict},
    channel::{BusType, Channel},
};
use std::{collections::HashMap, ffi::CString, time::Duration};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DOCUMENTS: &str = "org.freedesktop.portal.Documents";
const TIMEOUT: Duration = Duration::from_secs(10);

fn bus_call(channel: &Channel, member: &str, arguments: &[MessageItem]) -> Result<Message> {
    let mut call = Message::new_method_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        member,
    )?;
    call.append_items(arguments);
    Ok(channel.send_with_reply_and_block(call, TIMEOUT)?)
}

fn request_name(channel: &Channel, name: &str) -> Result<()> {
    let reply = bus_call(
        channel,
        "RequestName",
        &[name.into(), MessageItem::UInt32(4)],
    )?;
    if reply.read1::<u32>()? != 1 {
        return Err(format!("portal bridge could not own {name}").into());
    }
    Ok(())
}

// Toolkits construct a request path using their own bus name before calling
// the portal. Rewrite just that component, including paths nested in variants,
// so subscriptions installed before a fast response still match correctly.
fn translate_path(path: &str, from: &str, to: &str) -> String {
    let from = from.trim_start_matches(':').replace('.', "_");
    let to = to.trim_start_matches(':').replace('.', "_");
    for kind in ["request", "session"] {
        let prefix = format!("/org/freedesktop/portal/desktop/{kind}/{from}/");
        if let Some(rest) = path.strip_prefix(&prefix) {
            return format!("/org/freedesktop/portal/desktop/{kind}/{to}/{rest}");
        }
    }
    path.to_owned()
}

fn translate_item(item: MessageItem, from: &str, to: &str) -> Result<MessageItem> {
    Ok(match item {
        MessageItem::ObjectPath(path) => {
            MessageItem::ObjectPath(Path::new(translate_path(&path, from, to))?)
        }
        // Some portal APIs encode session_handle as a string variant rather
        // than a D-Bus object-path value. Preserve its type while translating.
        MessageItem::Str(value) => MessageItem::Str(translate_path(&value, from, to)),
        MessageItem::Variant(value) => {
            MessageItem::Variant(Box::new(translate_item(*value, from, to)?))
        }
        MessageItem::Struct(values) => MessageItem::Struct(translate_items(values, from, to)?),
        MessageItem::Array(values) => {
            let signature = values.signature().clone();
            MessageItem::Array(
                MessageItemArray::new(translate_items(values.into_vec(), from, to)?, signature)
                    .map_err(|error| format!("invalid portal array: {error:?}"))?,
            )
        }
        MessageItem::Dict(values) => {
            let signature = values.signature().to_string();
            let entries = values
                .into_vec()
                .into_iter()
                .map(|(key, value)| {
                    Ok((
                        translate_item(key, from, to)?,
                        translate_item(value, from, to)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            // D-Bus dictionary keys always have a one-character basic type.
            MessageItem::Dict(
                MessageItemDict::new(
                    entries,
                    Signature::new(&signature[2..3])?,
                    Signature::new(&signature[3..signature.len() - 1])?,
                )
                .map_err(|error| format!("invalid portal dictionary: {error:?}"))?,
            )
        }
        value => value,
    })
}

fn translate_items(items: Vec<MessageItem>, from: &str, to: &str) -> Result<Vec<MessageItem>> {
    items
        .into_iter()
        .map(|item| translate_item(item, from, to))
        .collect()
}

struct Peer {
    upstream: Channel,
    pending: HashMap<u32, Message>,
}

impl Peer {
    fn connect(address: &str) -> Result<Self> {
        let mut upstream = Channel::open_private(address)?;
        upstream.register()?;
        bus_call(
            &upstream,
            "AddMatch",
            &["type='signal',sender='org.freedesktop.portal.Desktop'".into()],
        )?;
        bus_call(
            &upstream,
            "AddMatch",
            &["type='signal',sender='org.freedesktop.portal.Documents'".into()],
        )?;
        Ok(Self {
            upstream,
            pending: HashMap::new(),
        })
    }

    fn forward_call(&mut self, call: Message, sender: &str) -> Result<()> {
        let destination = call
            .destination()
            .ok_or("portal call has no destination")?
            .to_string();
        let mut forwarded = Message::new_method_call(
            destination,
            translate_path(
                &call.path().ok_or("portal call has no path")?,
                sender,
                self.name(),
            ),
            call.interface().ok_or("portal call has no interface")?,
            call.member().ok_or("portal call has no member")?,
        )?;
        forwarded.append_items(&translate_items(call.get_items(), sender, self.name())?);
        forwarded.set_no_reply(call.get_no_reply());
        let serial = self
            .upstream
            .send(forwarded)
            .map_err(|_| "portal bus disconnected")?;
        if !call.get_no_reply() {
            self.pending.insert(serial, call);
        }
        Ok(())
    }

    fn name(&self) -> &str {
        self.upstream
            .unique_name()
            .expect("registered bus has a unique name")
    }

    fn receive(&mut self, local: &Channel, sender: &str) -> Result<()> {
        self.upstream
            .read_write(Some(Duration::ZERO))
            .map_err(|_| "host portal bus disconnected")?;
        while let Some(mut message) = self.upstream.pop_message() {
            let mut forwarded = match message.msg_type() {
                MessageType::MethodReturn | MessageType::Error => {
                    let Some(call) = message
                        .get_reply_serial()
                        .and_then(|serial| self.pending.remove(&serial))
                    else {
                        continue;
                    };
                    match message.as_result() {
                        Ok(_) => call.method_return(),
                        Err(error) => call.error(
                            &error
                                .name()
                                .unwrap_or("org.freedesktop.DBus.Error.Failed")
                                .into(),
                            &CString::new(error.message().unwrap_or("Host portal call failed"))?,
                        ),
                    }
                }
                MessageType::Signal => {
                    let Some(interface) = message.interface() else {
                        continue;
                    };
                    if !interface.starts_with("org.freedesktop.portal.") {
                        continue;
                    }
                    let mut signal = Message::new_signal(
                        translate_path(
                            &message.path().ok_or("portal signal has no path")?,
                            self.name(),
                            sender,
                        ),
                        interface.to_string(),
                        message
                            .member()
                            .ok_or("portal signal has no member")?
                            .to_string(),
                    )?;
                    signal.set_destination(Some(sender.into()));
                    signal
                }
                _ => continue,
            };
            if message.msg_type() != MessageType::Error {
                forwarded.append_items(&translate_items(message.get_items(), self.name(), sender)?);
            }
            local
                .send(forwarded)
                .map_err(|_| "private session bus disconnected")?;
        }
        Ok(())
    }
}

pub fn bridge(address: &str, appearance_only: bool) -> Result<i32> {
    let local = Channel::get_private(BusType::Session)?;
    request_name(&local, DESKTOP)?;
    if !appearance_only {
        request_name(&local, DOCUMENTS)?;
    }
    bus_call(
        &local,
        "AddMatch",
        &["type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'".into()],
    )?;
    let mut peers = HashMap::<String, Peer>::new();
    loop {
        local
            .read_write(Some(Duration::from_millis(20)))
            .map_err(|_| "private session bus disconnected")?;
        while let Some(call) = local.pop_message() {
            if call.msg_type() == MessageType::Signal
                && call.member().as_deref() == Some("NameOwnerChanged")
            {
                let (name, _, owner): (String, String, String) = call.read3()?;
                if owner.is_empty() {
                    peers.remove(&name);
                }
                continue;
            }
            if call.msg_type() != MessageType::MethodCall {
                continue;
            }
            // Appearance inheritance is deliberately separate from opting in
            // to interactive portals. Enforce that boundary both here and in
            // the host proxy, including calls to this bridge's unique name.
            if appearance_only
                && call.interface().as_deref() != Some("org.freedesktop.portal.Settings")
            {
                let reply = call.error(
                    &"org.freedesktop.DBus.Error.AccessDenied".into(),
                    &CString::new("Only appearance settings are available")?,
                );
                local
                    .send(reply)
                    .map_err(|_| "private session bus disconnected")?;
                continue;
            }
            let destination = call
                .destination()
                .map(|name| name.to_string())
                .unwrap_or_default();
            if ![DESKTOP, DOCUMENTS].contains(&destination.as_str())
                && Some(destination.as_str()) != local.unique_name()
            {
                continue;
            }
            let sender = call
                .sender()
                .ok_or("portal call has no sender")?
                .to_string();
            if !peers.contains_key(&sender) {
                peers.insert(sender.clone(), Peer::connect(address)?);
            }
            // Calls addressed to this bridge's unique name are portal object
            // calls (Request.Close / Session.Close); use the host portal name.
            let mut call = call;
            if Some(destination.as_str()) == local.unique_name() {
                let name = if call
                    .path()
                    .is_some_and(|path| path.starts_with("/org/freedesktop/portal/documents"))
                {
                    DOCUMENTS
                } else {
                    DESKTOP
                };
                call.set_destination(Some(name.into()));
            }
            peers
                .get_mut(&sender)
                .expect("peer was inserted")
                .forward_call(call, &sender)?;
        }
        for (sender, peer) in &mut peers {
            peer.receive(&local, sender)?;
        }
    }
}

pub fn document_mountpoint() -> Result<i32> {
    let bus = Channel::get_private(BusType::Session)?;
    let call = Message::new_method_call(
        DOCUMENTS,
        "/org/freedesktop/portal/documents",
        DOCUMENTS,
        "GetMountPoint",
    )?;
    let path: Vec<u8> = bus.send_with_reply_and_block(call, TIMEOUT)?.read1()?;
    println!(
        "{}",
        std::str::from_utf8(path.strip_suffix(&[0]).unwrap_or(&path))?
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_only_the_originating_peers_portal_paths() {
        let path = "/org/freedesktop/portal/desktop/request/1_42/pick";
        assert_eq!(
            translate_path(path, ":1.42", ":1.9"),
            "/org/freedesktop/portal/desktop/request/1_9/pick"
        );
        assert_eq!(translate_path(path, ":1.4", ":1.9"), path);
        assert_eq!(
            translate_path("/some/other/path", ":1.42", ":1.9"),
            "/some/other/path"
        );
    }
}
