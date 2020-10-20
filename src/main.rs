/*
 *
 * This file is part of onizd, copyright ©2020 Solra Bizna.
 *
 * onizd is free software: you can redistribute it and/or modify it under the
 * terms of the GNU General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your option) any later
 * version.
 *
 * onizd is distributed in the hope that it will be useful, but WITHOUT ANY
 * WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
 * details.
 *
 * You should have received a copy of the GNU General Public License along with
 * onizd. If not, see <https://www.gnu.org/licenses/>.
 *
 */

// Note: Any time you see `writeln!(out, ...).unwrap()`, it's because
// `Outputter::write_str` cannot throw errors.

#![windows_subsystem = "windows"]

use std::{
    convert::{TryFrom,TryInto},
    net::SocketAddr,
    sync::{Arc,Mutex},
    time::Duration,
    fmt::Write,
};
#[cfg(feature = "auth")]
use std::io::SeekFrom;
use tokio::{
    net::{TcpListener, TcpStream},
    stream::StreamExt,
    sync::mpsc,
    time::{timeout,interval},
};
#[cfg(feature = "auth")]
use tokio::{
    io::AsyncReadExt,
    fs::File,
};
use futures::sink::SinkExt;
use tokio_util::codec;
use bytes::{BytesMut, buf::{Buf, BufMut}};
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
#[cfg(feature = "auth")]
use rand::{prelude::*, rngs::OsRng};
use anyhow;

mod invocation;
pub use invocation::*;
mod point;
pub use point::*;
mod map;
pub use map::*;
mod mat;
pub use mat::*;
mod elemap;
pub use elemap::*;
mod wrapped;
pub use wrapped::*;
mod mit_zlib;
pub use mit_zlib::{MitZlibReader, MitZlibWriter};
mod outputter;
pub use outputter::*;

#[cfg(feature = "gui")]
mod gui;

pub const DEFAULT_ADDR_AND_PORT: &str = "0.0.0.0:5496";
#[cfg(feature = "auth")]
pub const AUTH_BYTE_SIZE: usize = 5496;
#[cfg(feature = "auth")]
pub const NUM_CHALLENGES: usize = 3;

pub type ClientID = u64;

#[derive(Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum CompressionType { Zlib }

fn errorize(err: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, err)
}

fn expect_int<T: TryFrom<i64>>(val: &Value) -> std::io::Result<T> {
    match val {
        Value::Number(x) if x.is_i64() => match val.as_i64().unwrap().try_into() {
            Ok(x) => Ok(x),
            Err(_) => Err(errorize("Number out of range")),
        },
        _ => Err(errorize("Needed a number, got something else"))
    }
}

fn expect_string(val: &Value) -> std::io::Result<&str> {
    match val {
        Value::String(ref x) => {
            if x.len() > 30 { Err(errorize("String was too long")) }
            else { Ok(x) }
        },
        _ => Err(errorize("Needed a string, got something else")),
    }
}

fn register_maybe_offset(what: &str, recv_offset: i32) -> i32 {
    if what.ends_with("Recver") { recv_offset }
    else if what.ends_with("Sender") { -recv_offset }
    else { 0 }
}

async fn send_response(socket: &mut Client, mut json: Value,
                       cookie: &Value) -> std::io::Result<()>
{
    // TODO: debug_assert that there's a "type" key
    match cookie {
        Value::Null | Value::Object(_) | Value::Array(_) => (),
        x => json["cookie"] = x.clone(),
    }
    socket.send(json).await
}

pub struct MessageCoder {
    verbosity: u32,
    out: Outputter,
}
impl codec::Decoder for MessageCoder {
    type Item = Value;
    type Error = std::io::Error;
    fn decode(&mut self, src: &mut BytesMut) -> std::io::Result<Option<Value>>{
        while !src.is_empty() && src[0] == b'\n' {
            let _ = src.get_u8();
        }
        for n in 0 .. src.len() {
            if src[n] == b'\n' {
                let splat = src.split_to(n+1);
                let as_utf8 = match std::str::from_utf8(&splat[..]) {
                    Ok(x) => x,
                    Err(_) => return Err(errorize("Received invalid UTF-8")),
                };
                match serde_json::from_str(as_utf8) {
                    Err(_) => return Err(errorize("Received invalid JSON")),
                    Ok(x) => match x {
                        Value::Object(_) => {
                            if self.verbosity >= 2 {
                                writeln!(self.out, "    → {}", x).unwrap();
                            }
                            return Ok(Some(x))
                        },
                        _ => return Err(errorize("Received non-object JSON")),
                    }
                }
            }
        }
        if src.len() > 10000 {
            return Err(errorize("Improbably long message"));
        }
        Ok(None)
    }
}
impl codec::Encoder<Value> for MessageCoder {
    type Error = std::io::Error;
    fn encode(&mut self, json: Value, dst: &mut BytesMut)
              -> std::io::Result<()> {
        let s = json.to_string();
        if self.verbosity >= 2 {
            writeln!(self.out, "    ← {}", s).unwrap();
        }
        let b = s.as_bytes();
        dst.reserve(b.len() + 1);
        dst.put(b);
        dst.put_u8(b'\n');
        Ok(())
    }
}
type Client = codec::Framed<WrappedSocket, MessageCoder>;

async fn inner_client(out: &mut Outputter,
                      verbosity: u32,
                      ping_interval: Option<Duration>,
                      offset_mode: bool,
                      auth_file: Option<String>,
                      map: &Arc<Mutex<Map>>,
                      socket: TcpStream,
                      peer: &SocketAddr,
                      client_id: ClientID)
                      -> std::io::Result<()> {
    socket.set_nodelay(true)?;
    let mut client = codec::Framed::new(socket, MessageCoder {
        verbosity, out: out.clone()
    });
    let recv_offset_y = if offset_mode { 1 } else { 0 };
    // make sure our client talks the right protocol at us
    // TODO: make the timeout duration configurable
    let message = match timeout(Duration::from_secs(10), client.next()).await {
        Err(_) => return Err(errorize("timed out")),
        // o_O
        Ok(None) | Ok(Some(Err(_))) =>
            return Err(errorize("invalid handshake")),
        Ok(Some(Ok(x))) => x,
    };
    match message["type"] {
        // ick...
        Value::String(ref x) if x == "hello" => (),
        _ => return Err(errorize("no \"hello\" in handshake")),
    }
    let mut client = wrap_client(client,
                                 serde_json::from_value
                                 ::<Option<CompressionType>>
                                 (message["compression"].clone())?).await?;
    match message["proto"] {
        Value::String(ref x) if x == "oniz" => (),
        _ => return Err(errorize("handshake is for wrong protocol")),
    }
    let proto_version = match &message["version"] {
        Value::Number(x) => match x.as_i64() {
            Some(x) => x,
            None => return Err(errorize("crazy \"version\" in handshake"))
        },
        _ => return Err(errorize("no \"version\" in handshake")),
    };
    match proto_version {
        0 => (), // OK
        x => {
            // (ignore an error sending this response)
            let _ = send_response(&mut client,
                                  json!({
                                      "type": "bad_version",
                                      "supported_versions": [0],
                                  }), &Value::Null).await;
            let _ = client.flush().await;
            return Err(errorize(&format!("wanted unknown protocol version {}",
                                         x)))
        },
    }
    #[cfg(feature = "auth")]
    if let Some(path) = auth_file {
        let mut file = File::open(path).await?;
        let metadata = file.metadata().await?;
        let len = metadata.len();
        if len == 0 {
            return Err(errorize("Can't authenticate using an empty \
                                 secret, silly!"))
        }
        let mut offsets = [0; NUM_CHALLENGES];
        for n in 0 .. NUM_CHALLENGES {
            offsets[n] = OsRng.next_u64() & 0x001FFFFFFFFFFFFFu64;
        }
        let mut ok_auths = 0;
        let mut buf = [0; AUTH_BYTE_SIZE];
        for n in 0 .. NUM_CHALLENGES {
            let offset = offsets[n];
            send_response(&mut client,
                          json!({
                              "type": "need_auth",
                              "offset": offset,
                          }), &Value::Null).await?;
            client.flush().await?;
            let start_pos = offset % len;
            file.seek(SeekFrom::Start(start_pos)).await?;
            let mut rem = &mut buf[..];
            while !rem.is_empty() {
                let red = file.read(rem).await?;
                if red == 0 {
                    file.seek(SeekFrom::Start(0)).await?;
                }
                rem = &mut rem[red..];
            }
            let calculated_hash = lsx::sha256::hash(&buf[..]);
            let calculated_hash = base64::encode(&calculated_hash[..]);
            let message = match client.next().await {
                Some(x) => x?,
                None => return Ok(()),
            };
            if let Value::String(typ) = &message["type"] {
                match typ.as_str() {
                    "auth" => {
                        let sent_hash = match message["hash"] {
                            Value::String(ref x) => x,
                            _ => return Err(errorize("Received a non-string \
                                                      hash?!")),
                        };
                        if sent_hash == calculated_hash.as_str() {
                            ok_auths += 1;
                        }
                    },
                    x => return Err(errorize(&format!("Received a non-auth \
                                                       message type during \
                                                       auth: {:?}", x)))
                }
            }
            else {
                return Err(errorize("Received a message with invalid type"))
            }
        }
        if ok_auths != NUM_CHALLENGES {
            writeln!(out, "  {} AUTHENTICATION FAILED!!!", peer).unwrap();
            if ok_auths != 0 {
                writeln!(out, "    WARNING!!! Passed {}/{} auths!", ok_auths,
                          NUM_CHALLENGES).unwrap();
            }
            send_response(&mut client,
                          json!({
                              "type": "auth_bad"
                          }), &Value::Null).await?;
            client.flush().await?;
            return Ok(())
        }
        else {
            writeln!(out, "  {} AUTHENTICATED", peer).unwrap();
        }
    }
    else {
        writeln!(out, "  {} AUTHENTICATED (no auth needed)", peer).unwrap();
    }
    #[cfg(not(feature = "auth"))]
    std::mem::drop(auth_file); // normally dropped by the above
    send_response(&mut client,
                  json!({
                      "type": "auth_ok"
                  }), &Value::Null).await?;
    let mut registrations = map.lock().unwrap().get_registrations();
    // send all registrations before our first flush
    while let Ok((polarity, loc, what)) = registrations.try_recv() {
        let typ = if polarity { "registered" } else { "unregistered"};
        send_response(&mut client,
                      json!({
                          "type": typ,
                          "x": loc.get_x(),
                          "y": loc.get_y(),
                          "what": what,
                      }), &Value::Null).await?;
    }
    client.flush().await?;
    // if there's no ping interval specified, ping once per day... since I
    // can't figure out how to make an optional future while using `select!`...
    let mut ping = interval(ping_interval.unwrap_or_else(|| Duration::new(86400,0)));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                send_response(&mut client,
                              json!({
                                  "type": "ping",
                              }), &Value::Null).await?;
                client.flush().await?;
            },
            Some((polarity, loc, what)) = registrations.next() => {
                let typ = if polarity { "registered" } else { "unregistered"};
                send_response(&mut client,
                              json!({
                                  "type": typ,
                                  "x": loc.get_x(),
                                  "y": loc.get_y(),
                                  "what": what,
                              }), &Value::Null).await?;
                client.flush().await?;
            },
            message = client.next() => {
                let message = match message {
                    Some(x) => x?,
                    None => return Ok(()),
                };
                if let Value::String(typ) = &message["type"] {
                    match typ.as_str() {
                        "ping" => {
                            send_response(&mut client,
                                          json!({
                                              "type": "pong",
                                          }), &message["cookie"]).await?;
                        },
                        "pong" => {},
                        "send_joules" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int(&message["y"])?;
                            let joules = expect_int(&message["joules"])?;
                            let point = Point::new(x, y);
                            let spare = map.lock().unwrap().add_joules(point, joules);
                            send_response(&mut client,
                                          json!({
                                              "type": "sent_joules",
                                              "x": x,
                                              "y": y,
                                              "spare": spare
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                if spare > 0 {
                                    writeln!(out, "  {} sent {}J to {} ({}J \
                                                   spared)",
                                             peer, joules, point, spare)
                                        .unwrap();
                                }
                                else {
                                    writeln!(out, "  {} sent {}J to {}",
                                              peer, joules, point).unwrap();
                                }
                            }
                        },
                        "recv_joules" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let max_joules = expect_int(&message["max_joules"])?;
                            let point = Point::new(x, y + recv_offset_y);
                            let joules = map.lock().unwrap().sub_joules(point,
                                                                        max_joules);
                            send_response(&mut client,
                                          json!({
                                              "type": "got_joules",
                                              "x": x,
                                              "y": y,
                                              "joules": joules,
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                writeln!(out, "  {} wanted up to {}J from {} \
                                               ({}J gotten)",
                                         peer, max_joules, point, joules)
                                    .unwrap();
                            }
                        },
                        "send_packet" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int(&message["y"])?;
                            let packet: MatPacket = serde_json::from_value(message["packet"].clone())?;
                            let phase = serde_json::from_value(message["phase"].clone())?;
                            if packet.is_oversized(phase) {
                                return Err(errorize("Received `MatPacket` had too \
                                                     much mass"))
                            }
                            let point = Point::new(x, y);
                            let accepted = map.lock().unwrap()
                                .add_packet(point, &packet, phase);
                            send_response(&mut client,
                                          json!({
                                              "type": "sent_packet",
                                              "x": x,
                                              "y": y,
                                              "accepted": accepted
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                if accepted {
                                    writeln!(out, "  {} put {} {} in {}",
                                             peer, phase, packet, point)
                                        .unwrap();
                                }
                                else {
                                    writeln!(out, "  {} put {} {} in {} \
                                                   (rejected!)",
                                             peer, phase, packet, point)
                                        .unwrap();
                                }
                            }
                        },
                        "recv_packet" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let phase = serde_json::from_value(message["phase"].clone())?;
                            let point = Point::new(x, y + recv_offset_y);
                            let packet = map.lock().unwrap().pop_packet(point, phase);
                            send_response(&mut client,
                                          json!({
                                              "type": "got_packet",
                                              "x": x,
                                              "y": y,
                                              "phase": phase,
                                              "packet": packet,
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                match packet {
                                    Some(packet) =>
                                        writeln!(out, "  {} sunk {} from {} \
                                                   (got {})",
                                                  peer, phase, point, packet),
                                    None =>
                                        writeln!(out, "  {} sunk {} from {} \
                                                   (got nothing)",
                                                  peer, phase, point),
                                }.unwrap();
                            }
                        },
                        "register" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let what = expect_string(&message["what"])?;
                            let point = Point::new(x, y + register_maybe_offset(what, recv_offset_y));
                            if !map.lock().unwrap().register(point, client_id,
                                                             what.to_owned()) {
                                return Err(errorize("Registered too many buildings at \
                                                     the same point"))
                            }
                            if verbosity >= 1 {
                                writeln!(out, "  {} registered a {:?} at {}",
                                          peer, what, point).unwrap();
                            }
                        },
                        "unregister" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let what = expect_string(&message["what"])?;
                            let point = Point::new(x, y + register_maybe_offset(what, recv_offset_y));
                            map.lock().unwrap().unregister(point, client_id, what);
                            if verbosity >= 1 {
                                writeln!(out, "  {} unregistered a {:?} at {}",
                                          peer, what, point).unwrap();
                            }
                        },
                        x => return Err(errorize(&format!("Received a message \
                                                           with unknown type: \
                                                           {:?}", x)))
                    }
                    client.flush().await?;
                }
                else {
                    return Err(errorize("Received a message with invalid \
                                         type"))
                }
            },
        }
    }
}

async fn client(mut out: Outputter,
                verbosity: u32, ping_interval: Option<Duration>,
                offset_mode: bool, auth_file: Option<String>,
                map: Arc<Mutex<Map>>, socket: TcpStream, peer: SocketAddr,
                client_id: ClientID) {
    match inner_client(&mut out, verbosity, ping_interval, offset_mode,
                       auth_file, &map, socket, &peer, client_id)
    .await {
        Ok(()) =>
            writeln!(out, "  {} DISCONNECTED", peer),
        Err(x) =>
            writeln!(out, "  {} ERROR: {}", peer, x),
    }.unwrap();
    map.lock().unwrap().unregister_all(client_id);
}

async fn server_loop(invocation: Invocation, out: &mut Outputter)
                     -> anyhow::Result<()> {
    let listen_addr = invocation.listen_addr
        .unwrap_or_else(|| DEFAULT_ADDR_AND_PORT.to_owned());
    let map = Arc::new(Mutex::new(Map::new()));
    let mut listener = TcpListener::bind(&listen_addr).await?;
    let mut next_client_id: ClientID = 0;
    loop {
        let (socket, peer) = listener.accept().await?;
        writeln!(out, "{} CONNECTED", peer).unwrap();
        let map_clone = map.clone();
        let verbosity = invocation.verbosity;
        let offset_mode = invocation.offset_mode;
        let auth_file = invocation.auth_file.clone();
        let client_id = next_client_id;
        let ping_interval = invocation.ping_interval;
        next_client_id = next_client_id.checked_add(1) // :)
            .expect("Can't have more than 2^64 clients in one session!");
        tokio::spawn(client(out.clone(), verbosity, ping_interval, offset_mode,
                            auth_file, map_clone, socket, peer,
                            client_id));
    }
}

fn true_main(invocation: Invocation,
             mut termination_tx: mpsc::Sender<()>,
             mut termination_rx: mpsc::Receiver<()>,
             mut out: Outputter) {
    writeln!(out, "\n\nServer starting up...").unwrap();
    let mut runtime = tokio::runtime::Builder::new()
        .basic_scheduler().enable_all().build().unwrap();
    let mut out_clone = out.clone();
    runtime.spawn(async move {
        match server_loop(invocation, &mut out_clone).await {
            Ok(_) => (),
            Err(x) => {
                writeln!(out_clone, "\n\nError! {}", x).unwrap();
            }
        }
        // improve odds that we terminate ourselves gracefully
        let _ = termination_tx.try_send(());
    });
    runtime.block_on(async {
        termination_rx.recv().await.unwrap()
    });
    writeln!(out, "\n\nServer closing down...").unwrap();
}

fn main() {
    #[cfg(feature = "gui")]
    {
        let mut argsi = std::env::args();
        // Start the GUI if we're started with no arguments.
        if argsi.next().is_none() || argsi.next().is_none() {
            return gui::go();
        }
    }
    let invocation = match get_invocation() {
        None => std::process::exit(1),
        Some(x) => x,
    };
    let (termination_tx, termination_rx) = mpsc::channel(1);
    let mut termination_tx_clone = termination_tx.clone();
    ctrlc::set_handler(move || {
        let _ = termination_tx_clone.try_send(());
    }).unwrap();
    true_main(invocation, termination_tx, termination_rx, Outputter::Stderr);
}
