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

use std::{
    convert::{TryFrom,TryInto},
    io::SeekFrom,
    sync::{Arc,Mutex},
    time::Duration,
};
use std::net::SocketAddr;
use tokio::{
    net::{TcpListener, TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}},
    io::{BufReader, AsyncReadExt, AsyncBufReadExt, AsyncWriteExt},
    time::timeout,
    fs::File,
    sync::mpsc,
};
use serde_json::{Value,json};
#[cfg(feature = "auth")]
use rand::{prelude::*, rngs::OsRng};

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

pub const DEFAULT_ADDR_AND_PORT: &str = "0.0.0.0:5496";
#[cfg(feature = "auth")]
pub const AUTH_BYTE_SIZE: usize = 5496;
#[cfg(feature = "auth")]
pub const NUM_CHALLENGES: usize = 3;

pub type ClientID = u64;

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


async fn send_response(verbosity: usize, socket: &mut OwnedWriteHalf,
                       mut json: Value, cookie: &Value) -> std::io::Result<()>
{
    // TODO: debug_assert that there's a "type" key
    match cookie {
        Value::Null | Value::Object(_) | Value::Array(_) => (),
        x => json["cookie"] = x.clone(),
    }
    if verbosity >= 2 {
        eprintln!("    ← {}", json);
    }
    socket.write_all(json.to_string().as_bytes()).await?;
    socket.write_all(b"\n").await                   
}

async fn get_message(verbosity: usize, socket: &mut BufReader<OwnedReadHalf>,
                     buf: &mut Vec<u8>) -> std::io::Result<Option<Value>> {
    loop {
        buf.clear();
        socket.read_until(b'\n', buf).await?;
        if buf == b"\n" { continue }
        let buf = match std::str::from_utf8(&buf[..]) {
            Ok(x) => x,
            Err(_) => return Err(errorize("Received invalid UTF-8")),
        };
        if buf.is_empty() { return Ok(None) }
        match serde_json::from_str(buf) {
            Err(_) => return Err(errorize("Received invalid JSON")),
            Ok(x) => match x {
                Value::Object(_) => {
                    if verbosity >= 2 {
                        eprintln!("    → {}", x);
                    }
                    return Ok(Some(x));
                }
                _ => return Err(errorize("Received non-object JSON")),
            }
        }
    }
}

/// Either a message from the client, or an (un)registration notification.
enum ClientRelevantMessage {
    ClientMessage(std::io::Result<Option<Value>>),
    Registration((bool, Point, String)),
}

async fn inner_client(verbosity: usize,
                      offset_mode: bool,
                      auth_file: Option<String>,
                      map: &Arc<Mutex<Map>>,
                      socket: TcpStream,
                      peer: &SocketAddr,
                      client_id: ClientID)
                      -> std::io::Result<()> {
    socket.set_nodelay(true)?;
    let (readsock, mut writesock) = socket.into_split();
    let recv_offset_y = if offset_mode { 1 } else { 0 };
    let mut readsock = BufReader::new(readsock);
    let mut msgbuf = Vec::new();
    // make sure our client talks the right protocol at us
    // TODO: make the timeout duration configurable
    let message = match timeout(Duration::from_secs(10),
                                get_message(verbosity, &mut readsock,
                                            &mut msgbuf)).await {
        Err(_) => return Err(errorize("timed out")),
        Ok(Err(_)) | Ok(Ok(None)) => return Err(errorize("invalid handshake")),
        Ok(Ok(Some(x))) => x,
    };
    match message["type"] {
        // ick...
        Value::String(ref x) if x == "hello" => (),
        _ => return Err(errorize("no \"hello\" in handshake")),
    }
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
            let _ = send_response(verbosity, &mut writesock,
                                  json!({
                                      "type": "bad_version",
                                      "supported_versions": [0],
                                  }), &Value::Null).await;
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
            send_response(verbosity, &mut writesock,
                          json!({
                              "type": "need_auth",
                              "offset": offset,
                          }), &Value::Null).await?;
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
            let message = match get_message(verbosity, &mut readsock,
                                            &mut msgbuf).await? {
                Some(x) => x,
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
            eprintln!("  {} AUTHENTICATION FAILED!!!", peer);
            if ok_auths != 0 {
                eprintln!("    WARNING!!! Passed {}/{} auths!", ok_auths,
                          NUM_CHALLENGES);
            }
            send_response(verbosity, &mut writesock,
                          json!({
                              "type": "auth_bad"
                          }), &Value::Null).await?;
            return Ok(())
        }
        else {
            eprintln!("  {} AUTHENTICATED", peer);
        }
    }
    else {
        eprintln!("  {} AUTHENTICATED (no auth needed)", peer);
    }
    #[cfg(not(feature = "auth"))]
    std::mem::drop(auth_file); // normally dropped by the above
    send_response(verbosity, &mut writesock,
                  json!({
                      "type": "auth_ok"
                  }), &Value::Null).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                let res = get_message(verbosity, &mut readsock, &mut msgbuf).await;
                let owari = match res {
                    Err(_) | Ok(None) => true,
                    _ => false,
                };
                match tx.send(ClientRelevantMessage::ClientMessage(res)) {
                    Err(_) => break,
                    Ok(_) => (),
                }
                if owari { break }
            }
        });
    }
    {
        let mut registrations = map.lock().unwrap().get_registrations();
        tokio::spawn(async move {
            while let Some(x) = registrations.recv().await {
                match tx.send(ClientRelevantMessage::Registration(x)) {
                    Err(_) => break,
                    Ok(_) => (),
                }
            }
        });
    }
    loop {
        match rx.recv().await {
            None => return Err(errorize("Receivers unexpectedly dropped")),
            Some(ClientRelevantMessage::Registration((polarity, loc, what)))=>{
                let typ = if polarity { "registered" } else { "unregistered "};
                send_response(verbosity, &mut writesock,
                              json!({
                                  "type": typ,
                                  "x": loc.get_x(),
                                  "y": loc.get_y(),
                                  "what": what,
                              }), &Value::Null).await?;
            },
            Some(ClientRelevantMessage::ClientMessage(message)) => {
                let message = match message? {
                    Some(x) => x,
                    None => return Ok(()),
                };
                if let Value::String(typ) = &message["type"] {
                    match typ.as_str() {
                        "send_joules" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int(&message["y"])?;
                            let joules = expect_int(&message["joules"])?;
                            let point = Point::new(x, y);
                            let spare = map.lock().unwrap().add_joules(point, joules);
                            send_response(verbosity, &mut writesock,
                                          json!({
                                              "type": "sent_joules",
                                              "x": x,
                                              "y": y,
                                              "spare": spare
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                if spare > 0 {
                                    eprintln!("  {} sent {}J to {} ({}J spared)",
                                              peer, joules, point, spare)
                                }
                                else {
                                    eprintln!("  {} sent {}J to {}",
                                              peer, joules, point)
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
                            send_response(verbosity, &mut writesock,
                                          json!({
                                              "type": "got_joules",
                                              "x": x,
                                              "y": y,
                                              "joules": joules,
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                eprintln!("  {} wanted up to {}J from {} ({}J gotten)",
                                          peer, max_joules, point, joules)
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
                            send_response(verbosity, &mut writesock,
                                          json!({
                                              "type": "sent_packet",
                                              "x": x,
                                              "y": y,
                                              "accepted": accepted
                                          }), &message["cookie"]).await?;
                            if verbosity >= 1 {
                                if accepted {
                                    eprintln!("  {} put {} {} in {}",
                                              peer, phase, packet, point)
                                }
                                else {
                                    eprintln!("  {} put {} {} in {} (rejected!)",
                                              peer, phase, packet, point)
                                }
                            }
                        },
                        "recv_packet" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let phase = serde_json::from_value(message["phase"].clone())?;
                            let point = Point::new(x, y + recv_offset_y);
                            let packet = map.lock().unwrap().pop_packet(point, phase);
                            send_response(verbosity, &mut writesock,
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
                                        eprintln!("  {} sunk {} from {} \
                                                   (got {})",
                                                  peer, phase, point, packet),
                                    None =>
                                        eprintln!("  {} sunk {} from {} \
                                                   (got nothing)",
                                                  peer, phase, point),
                                }
                            }
                        },
                        "register" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let what = expect_string(&message["what"])?;
                            let point = Point::new(x, y);
                            if !map.lock().unwrap().register(point, client_id,
                                                             what.to_owned()) {
                                return Err(errorize("Registered too many buildings at \
                                                     the same point"))
                            }
                        },
                        "unregister" => {
                            let x = expect_int(&message["x"])?;
                            let y = expect_int::<i32>(&message["y"])?;
                            let what = expect_string(&message["what"])?;
                            let point = Point::new(x, y);
                            map.lock().unwrap().unregister(point, client_id, what);
                        },
                        x => return Err(errorize(&format!("Received a message with \
                                                           unknown type: {:?}", x)))
                    }
                }
                else {
                    return Err(errorize("Received a message with invalid type"))
                }
            },
        }
    }
}

async fn client(verbosity: usize, offset_mode: bool, auth_file: Option<String>,
                map: Arc<Mutex<Map>>, socket: TcpStream, peer: SocketAddr,
                client_id: ClientID) {
    match inner_client(verbosity, offset_mode, auth_file, &map, socket, &peer,
                       client_id)
    .await {
        Ok(()) =>
            eprintln!("  {} DISCONNECTED", peer),
        Err(x) =>
            eprintln!("  {} ERROR: {}", peer, x),
    }
    map.lock().unwrap().unregister_all(client_id);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let invocation = match get_invocation() {
        None => std::process::exit(1),
        Some(x) => x,
    };
    let listen_addr = invocation.listen_addr.unwrap_or_else(|| DEFAULT_ADDR_AND_PORT.to_owned());
    let map = Arc::new(Mutex::new(Map::new()));
    let mut listener = TcpListener::bind(&listen_addr).await?;
    let mut next_client_id: ClientID = 0;
    loop {
        let (socket, peer) = listener.accept().await?;
        eprintln!("{} CONNECTED", peer);
        let map_clone = map.clone();
        let verbosity = invocation.verbosity;
        let offset_mode = invocation.offset_mode;
        let auth_file = invocation.auth_file.clone();
        let client_id = next_client_id;
        next_client_id = next_client_id.checked_add(1)
            .expect("Can't have more than 2^64 clients in one session!"); // :)
        tokio::spawn(client(verbosity, offset_mode, auth_file, map_clone, socket, peer, client_id));
    }
}
