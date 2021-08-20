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
    collections::hash_map::{HashMap,Entry},
    fs::File,
};
use tokio::sync::mpsc;
use std::io::Result as IoResult;

use crate::*;

/// Maximum amount of energy, in joules, that can be stored in one point on the
/// map. This will limit the maximum transmission rate of energy, related to
/// ping. ONI's energy processing happens 5 times per game second, so the
/// maximum transmission rate in watts is five times this amount.
pub const MAX_STORED_ENERGY: u32 = 10000;
/// Maximum number of "packets" that can be stored in one point on the map.
/// This will limit the maximum transmission rate of materials, related to
/// ping. Similar packets will be merged, so as long as mixed pipes aren't in
/// use, things should be okay.
pub const MAX_STORED_PACKETS: usize = 10; // probably too high
/// Maximum number of registrations allowed with the same `ClientID` at one
/// point on the map.
pub const MAX_REGISTRATIONS: usize = 7;
/// Maximum number of opaque objects that can be stored in one point on the
/// map. This will limit the maximum transmission rate of solid objects,
/// related to ping. Unlike energy and packets, we can't combine "stackable"
/// objects. Hopefully that doesn't end up being much of a problem.
pub const MAX_STORED_OBJECTS: usize = 3;

struct RegSender {
    vec: Vec<mpsc::UnboundedSender<(bool, Point, String)>>
}

impl RegSender {
    pub fn new() -> RegSender { RegSender { vec: Vec::new() } }
    pub fn send(&mut self, was: (bool, Point, &str)) {
        for i in (0..self.vec.len()).rev() {
            match self.vec[i].send((was.0, was.1, was.2.to_owned())) {
                Ok(_) => (),
                Err(_) => { self.vec.remove(i); },
            }
        }
    }
    pub fn push(&mut self, was: mpsc::UnboundedSender<(bool, Point, String)>) {
        self.vec.push(was)
    }
}

/// Contains all the state for the "interlayer" map. Incorporates temporary
/// storage for energy, solids, liquids, and gases.
pub struct Map {
    energy: HashMap<Point, u32>,
    gas_packets: HashMap<Point, Vec<MatPacket>>,
    liquid_packets: HashMap<Point, Vec<MatPacket>>,
    objects: HashMap<Point, Vec<Vec<u8>>>,
    registrations: HashMap<Point, Vec<(ClientID, String)>>,
    registration_senders: RegSender,
}

impl Map {
    /// Creates a new, blank map.
    pub fn new() -> Map {
        Map {
            energy: HashMap::new(),
            gas_packets: HashMap::new(),
            liquid_packets: HashMap::new(),
            objects: HashMap::new(),
            registrations: HashMap::new(),
            registration_senders: RegSender::new(),
        }
    }
    /// Attempts to insert energy into the map at a given point. Returns the
    /// amount left over, i.e. the amount that DID NOT fit.
    pub fn add_joules(&mut self, loc: Point, amt: u32) -> u32 {
        let slot = self.energy.entry(loc).or_insert(0);
        let new_amount = *slot as u64 + amt as u64;
        let capped = (MAX_STORED_ENERGY as u64).min(new_amount);
        let spill = new_amount.saturating_sub(capped);
        *slot = capped as u32;
        spill as u32
    }
    /// Attempts to remove energy from the map at a given point. Returns the
    /// amount that was successfully "removed".
    pub fn sub_joules(&mut self, loc: Point, amt: u32) -> u32 {
        match self.energy.get_mut(&loc) {
            None => 0,
            Some(slot) => {
                let slosh = (*slot).min(amt);
                *slot = slot.saturating_sub(amt);
                slosh
            },
        }
    }
    /// Attempts to add a MatPacket of the given phase to the map at the given
    /// point. Returns only `true` (the packet was entirely accepted) or
    /// `false` (the packet was entirely rejected).
    pub fn add_packet(&mut self, loc: Point, packet: &MatPacket, phase: Phase)
                      -> bool {
        let map = match phase {
            Phase::Gas => &mut self.gas_packets,
            Phase::Liquid => &mut self.liquid_packets,
        };
        let entry = map.entry(loc);
        match entry {
            Entry::Vacant(entry) => {
                let mut vec = Vec::with_capacity(MAX_STORED_PACKETS);
                vec.push(*packet);
                entry.insert(vec);
                return true;
            },
            Entry::Occupied(mut entry) => {
                // we assume that there cannot be more than one NON-FULL packet
                // of a given element in a given queue!
                //
                // BECAUSE!
                //
                // If we receive a packet, we know that it is less than MAX kg.
                // Therefore, one of the following will happen:
                //
                // - It is fully merged into an existing packet
                // - Part is merged into an existing packet (which becomes
                //   full), and the rest is  put into EXACTLY ONE new packet
                // - It is entirely rejected
                //
                // None of those three possibilities can result in there being
                // more than one FULL packet of a given element.
                let vec = entry.get_mut();
                let len = vec.len();
                for el in vec.iter_mut() {
                    if !el.has_room(phase) { continue }
                    match el.merge(&packet, phase) {
                        None => continue,
                        Some((merged, None)) => {
                            *el = merged;
                            return true;
                        },
                        Some((merged, Some(spare))) => {
                            if len >= MAX_STORED_PACKETS { return false }
                            *el = merged;
                            vec.push(spare);
                            return true;
                        },
                    }
                }
                // merging with an existing stack failed. try adding it to the
                // end.
                if len >= MAX_STORED_PACKETS { return false }
                vec.push(*packet);
                return true;
            }
        }
    }
    /// Attempts to remove a MatPacket of the given phase from the map at the
    /// given point. Returns `None` if there were no packets left, or `Some` if
    /// a packet was successfully removed.
    pub fn pop_packet(&mut self, loc: Point, phase: Phase) -> Option<MatPacket> {
        let map = match phase {
            Phase::Gas => &mut self.gas_packets,
            Phase::Liquid => &mut self.liquid_packets,
        };
        let entry = map.entry(loc);
        match entry {
            Entry::Vacant(_) => None,
            Entry::Occupied(mut entry) => {
                let vec = entry.get_mut();
                if vec.is_empty() { None }
                else { Some(vec.remove(0)) }
            }
        }
    }
    /// Attempts to register a given client's building at the given point.
    /// Returns `true` if the registration was OK, `false` if the client had
    /// too many registrations at that point.
    pub fn register(&mut self, loc: Point, client_id: ClientID,
                    what: String) -> bool {
        let slot = self.registrations.entry(loc).or_insert(Vec::new());
        let count = slot.iter().map(|x| x.0 == client_id)
            .fold(0, |a,b| if b { a + 1 } else { a });
        if count >= MAX_REGISTRATIONS { false }
        else {
            self.registration_senders.send((true, loc, &what));
            slot.push((client_id, what));
            true
        }
    }
    /// Attempts to unregister a given client's building at the given point.
    /// Unconditionally succeeds.
    ///
    /// This may also trigger removal of empty Energy/MatPacket storage at
    /// the given point, saving some memory.
    pub fn unregister(&mut self, loc: Point, client_id: ClientID,
                      what: &str) {
        let entry = self.registrations.entry(loc);
        let prune = match entry {
            Entry::Vacant(_) => true,
            Entry::Occupied(mut entry) => {
                let vec = entry.get_mut();
                for i in (0..vec.len()).rev() {
                    if vec[i].0 == client_id && vec[i].1 == what {
                        vec.remove(i);
                        self.registration_senders.send((false, loc, what));
                    }
                }
                if vec.is_empty() {
                    entry.remove();
                    true
                } else { false }
            }
        };
        if prune {
            self.prune(loc);
        }
    }
    /// Unregister *all* buildings from a given client.
    ///
    /// This may trigger removal of empty Energy/MatPackets.
    pub fn unregister_all(&mut self, client_id: ClientID) {
        let mut prunes = Vec::new();
        let registration_senders = &mut self.registration_senders;
        self.registrations.retain(|loc, vec| {
            for i in (0..vec.len()).rev() {
                if vec[i].0 == client_id {
                    registration_senders.send((false, *loc, &vec[i].1));
                    vec.remove(i);
                }
            }
            if vec.is_empty() {
                prunes.push(*loc);
                false
            } else { true }
        });
        for loc in prunes.into_iter() { self.prune(loc) }
    }
    /// Possibly prune Energy/MatPacket for the given location
    fn prune(&mut self, loc: Point) {
        match self.energy.entry(loc) {
            Entry::Vacant(_) => (),
            Entry::Occupied(entry) =>
                if *entry.get() == 0 { entry.remove(); }
        }
        match self.gas_packets.entry(loc) {
            Entry::Vacant(_) => (),
            Entry::Occupied(entry) =>
                if entry.get().is_empty() { entry.remove(); }
        }
        match self.liquid_packets.entry(loc) {
            Entry::Vacant(_) => (),
            Entry::Occupied(entry) =>
                if entry.get().is_empty() { entry.remove(); }
        }
    }
    /// Get a queue that will receive all registrations and unregistratinos
    /// that take place on the map, pre-filled with all currently-active registrations.
    pub fn get_registrations(&mut self)
                             -> mpsc::UnboundedReceiver<(bool, Point, String)>{
        let (tx, rx) = mpsc::unbounded_channel();
        for (loc, vec) in self.registrations.iter() {
            for el in vec.iter() {
                tx.send((true, *loc, el.1.clone()))
                    .expect("Couldn't send? We should be able to send!");
            }
        }
        self.registration_senders.push(tx);
        rx
    }
    /// Attempts to add an opaque object to the map at the given point. Returns
    /// only `true` (the object was entirely accepted) or `false` (the object
    /// was entirely rejected).
    pub fn add_object(&mut self, loc: Point, object: Vec<u8>) -> bool {
        let entry = self.objects.entry(loc);
        match entry {
            Entry::Vacant(entry) => {
                let mut vec = Vec::with_capacity(MAX_STORED_OBJECTS);
                vec.push(object);
                entry.insert(vec);
                return true;
            },
            Entry::Occupied(mut entry) => {
                let vec = entry.get_mut();
                let len = vec.len();
                if len >= MAX_STORED_OBJECTS { return false }
                vec.push(object);
                return true;
            }
        }
    }
    /// Attempts to remove an opaque object from the map at the given point.
    /// Returns `None` if there was no object, or `Some(...)` if there was.
    pub fn pop_object(&mut self, loc: Point) -> Option<Vec<u8>> {
        let entry = self.objects.entry(loc);
        match entry {
            Entry::Vacant(_) => None,
            Entry::Occupied(mut entry) => {
                let vec = entry.get_mut();
                if vec.is_empty() { None }
                else { Some(vec.remove(0)) }
            }
        }
    }
    /// Clears everything on the map.
    pub fn clear(&mut self) {
        self.energy = HashMap::new();
        self.gas_packets = HashMap::new();
        self.liquid_packets = HashMap::new();
        self.objects = HashMap::new();
        self.registrations = HashMap::new();
    }
    /// Attempts to initialize the map with saved data from the given path.
    /// May leave the map in a partly-populated state on failure; you should
    /// call `clear` if that happens.
    pub fn try_load(&mut self, path: &str, max_object_size: usize) -> IoResult<()> {
        let max_object_encoded_size: usize = (max_object_size + 2) * 4 / 3;
        self.clear();
        let mut file = File::open(path)?;
        let value = serde_json::from_reader(&mut file)?;
        drop(file);
        let value = match value {
            Value::Object(x) => x,
            _ => return Err(errorize("saved map is not a JSON object"))
        };
        for (k,v) in value.into_iter() {
            let mut kit = k.split(",");
            let (x, y) = match (kit.next(), kit.next(), kit.next()) {
                (Some(x), Some(y), None) => (x, y),
                _ => continue, // skip invalid points
            };
            let (x, y) = match (x.parse::<i32>(), y.parse::<i32>()) {
                (Ok(x), Ok(y)) => (x, y),
                _ => continue,
            };
            let point = Point::new(x, y);
            let tile = match v {
                Value::Object(x) => x,
                _ => continue, // skip invalid tiles
            };
            match tile.get("energy") {
                Some(Value::Number(x)) if x.is_u64() =>
                    match x.as_u64().unwrap().try_into() {
                        Ok(x) => { self.add_joules(point, x); },
                        _ => (),
                    },
                _ => (),
            };
            match tile.get("gas_packets") {
                Some(Value::Array(x)) => {
                    for packet in x.iter() {
                        let packet = match serde_json::from_value::<MatPacket>(packet.clone()) {
                            Ok(x) => x,
                            Err(_) => continue,
                        };
                        self.add_packet(point, &packet, Phase::Gas);
                    }
                },
                _ => (),
            };
            match tile.get("liquid_packets") {
                Some(Value::Array(x)) => {
                    for packet in x.iter() {
                        let packet = match serde_json::from_value::<MatPacket>(packet.clone()) {
                            Ok(x) => x,
                            Err(_) => continue,
                        };
                        self.add_packet(point, &packet, Phase::Liquid);
                    }
                },
                _ => (),
            };
            match tile.get("objects") {
                Some(Value::Array(x)) => {
                    for object in x.iter() {
                        let object = match object {
                            Value::String(x) => x,
                            _ => continue,
                        };
                        if object.len() > max_object_encoded_size { continue }
                        let decoded = match base64::decode(object) {
                            Ok(x) if x.len() <= max_object_size => { x },
                            _ => continue,
                        };
                        self.add_object(point, decoded);
                    }
                },
                _ => (),
            };
        }
        Ok(())
    }
    /// Attempt to save the map to the given path.
    pub fn try_save(&self, path: &str) -> IoResult<()> {
        let mut saved: serde_json::Map<String, Value> = serde_json::Map::new();
        for (k, v) in self.energy.iter() {
            if *v > 0 {
                set_tile_key(&mut saved, *k, "energy",
                             Value::Number((*v).into()))
            }
        }
        for (k, v) in self.gas_packets.iter() {
            if v.len() > 0 {
                let mut arr = Vec::new();
                for packet in v.iter() {
                    arr.push(serde_json::to_value(packet)?);
                }
                set_tile_key(&mut saved, *k, "gas_packets",
                             Value::Array(arr))
            }
        }
        for (k, v) in self.liquid_packets.iter() {
            if v.len() > 0 {
                let mut arr = Vec::new();
                for packet in v.iter() {
                    arr.push(serde_json::to_value(packet)?);
                }
                set_tile_key(&mut saved, *k, "liquid_packets",
                             Value::Array(arr))
            }
        }
        for (k, v) in self.objects.iter() {
            if v.len() > 0 {
                let mut arr = Vec::new();
                for object in v.iter() {
                    arr.push(Value::String(base64::encode(object)));
                }
                set_tile_key(&mut saved, *k, "objects",
                             Value::Array(arr))
            }
        }
        let mut file = File::create(path)?;
        serde_json::to_writer(&mut file, &Value::Object(saved))?;
        Ok(())
    }
}

fn set_tile_key(saved: &mut serde_json::Map<String, Value>, point: Point,
                key: &str, value: Value) {
    let point = point.as_string();
    match saved.entry(point) {
        serde_json::map::Entry::Vacant(entry) => {
            let mut map = serde_json::Map::new();
            map.insert(key.to_owned(), value);
            entry.insert(Value::Object(map));
        },
        serde_json::map::Entry::Occupied(mut obj) => {
            let v = obj.get_mut();
            match v {
                Value::Object(map) => { map.insert(key.to_owned(), value); },
                _ => panic!("we confused ourselves while saving!"),
            }
        },
    }
}
