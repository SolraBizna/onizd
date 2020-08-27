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

use crate::*;

/// Maximum amount of energy, in joules, that can be stored in one point on the
/// map. This will limit the maximum transmission rate of energy, related to
/// ping.
pub const MAX_STORED_ENERGY: u32 = 10000;
/// Maximum number of "packets" that can be stored in one point on the map.
/// This will limit the maximum transmission rate of materials, related to
/// ping. Similar packets will be merged, so as long as mixed pipes aren't in
/// use, things should be okay.
pub const MAX_STORED_PACKETS: usize = 10; // probably too high

/// Contains all the state for the "interlayer" map. Incorporates temporary
/// storage for energy, <del>solids</del>, <del>liquids</del>, <del>and
/// gases</del>.
pub struct Map {
    energy: HashMap<Point, u32>,
    gas_packets: HashMap<Point, Vec<MatPacket>>,
    liquid_packets: HashMap<Point, Vec<MatPacket>>,
}

impl Map {
    /// Creates a new, blank map.
    pub fn new() -> Map {
        Map {
            energy: HashMap::new(),
            gas_packets: HashMap::new(),
            liquid_packets: HashMap::new(),
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
}

