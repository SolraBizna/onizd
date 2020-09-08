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
};
use tokio::sync::mpsc;

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
/// storage for energy, <del>solids</del>, liquids, and gases.
pub struct Map {
    energy: HashMap<Point, u32>,
    gas_packets: HashMap<Point, Vec<MatPacket>>,
    liquid_packets: HashMap<Point, Vec<MatPacket>>,
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
}

