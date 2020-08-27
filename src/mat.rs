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

use std::fmt::{Debug,Display,Formatter};
use serde::{Serialize,Deserialize};
use crate::*;

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum Phase { Gas, Liquid }
#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
pub struct MatPacket {
    element: i32,
    mass: f32,
    temperature: f32,
    germs: Option<Germs>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct Germs {
    id: i32,
    count: i32,
}

impl Phase {
    pub fn get_max_stack_size(&self) -> f32 {
        match self {
            &Phase::Gas => 1.0,
            &Phase::Liquid => 10.0,
        }
    }
}

impl MatPacket {
    /// Attempt to merge two `MatPacket`s together, up to the maximum size.
    ///
    /// Returns:
    /// - `None`: The merge was impossible
    /// - `Some((MatPacket, None))`: Merging resulted in one packet
    /// - `Some((MatPacket, Some(MatPacket)))`: Merging resulted in two packets
    pub fn merge(&self, other: &MatPacket, phase: Phase)
                 -> Option<(MatPacket,Option<MatPacket>)> {
        // can't merge different elements
        if self.element != other.element { return None }
        let element = self.element;
        let max = phase.get_max_stack_size();
        let room = max - self.mass;
        // can't merge above max mass
        if room <= 0.0 { return None }
        let buff = other.mass.min(room);
        let leftover = other.mass - buff;
        let germs = Germs::merge(self.germs, other.germs, buff / other.mass);
        let merged = MatPacket {
            element,
            mass: self.mass + buff,
            temperature: (self.temperature * self.mass
                          + other.temperature * buff) / (self.mass + buff),
            germs: germs.0,
        };
        let rest = if leftover > 0.0 {
            Some(MatPacket {
                element,
                mass: leftover,
                temperature: other.temperature,
                germs: germs.1,
            })
        } else { None };
        Some((merged, rest))
    }
    /// Returns `true` if more mass could be added to this packet, `false`
    /// otherwise.
    pub fn has_room(&self, phase: Phase) -> bool {
        return self.mass < phase.get_max_stack_size();
    }
    /// Returns `true` if this packet is *larger* than it is allowed to be,
    /// false otherwise/
    pub fn is_oversized(&self, phase: Phase) -> bool {
        return self.mass > phase.get_max_stack_size();
    }
}

impl Germs {
    /// Merge two `Germs`es together, as when merging a material packet.
    ///
    /// - `frac`: Fraction of the mass to merge
    ///
    /// Returns:
    /// - `(None, None)`: There were no germs
    /// - `(Some(Germs), None)`: 100% of the germs were merged
    /// - `(Some(Germs), Some(Germs))`: Not all germs were merged (either
    ///   the stacks were incompatible, or frac was less than one)
    pub fn merge(a: Option<Germs>, b: Option<Germs>, frac: f32)
                 -> (Option<Germs>, Option<Germs>) {
        let b = match (a, b) {
            (None, None) => return (None, None),
            (Some(_), None) => return (a, b),
            (Some(a), Some(b)) => {
                // merging different germ types is impossible
                if a.id != b.id {
                    // if frac is less than 1, just return the original as the
                    // second stack
                    if frac < 1.0 {
                        return (Some(a), Some(b))
                    }
                    // otherwise, it's a full merge, and somebody's got to win
                    // and somebody's got to lose...!
                    //
                    // the stack that is larger wins. if both stacks are the
                    // same size, then the lower id wins.
                    else if a.count == b.count {
                        if a.id < b.id { return (Some(a), Some(b)) }
                        else { return (Some(b), Some(a)) }
                    }
                    else if a.count > b.count { return (Some(a), Some(b)) }
                    else { return (Some(b), Some(a)) }
                }
                b
            },
            (None, Some(b)) => b,
        };
        if frac <= 0.0 { return (a, Some(b)) }
        // if we get this far, there were some germs in `b`, and we've
        // unwrapped them. `a` may still be None, but if there are germs in it,
        // they are definitely of the same type.
        let mut b = b.split(frac);
        if let Some(a) = a {
            assert_eq!(a.id, b.0.id);
            b.0.count += a.count;
        }
        (b.0.maybe(), b.1.maybe())
    }
    /// Split germs into two based on a given mass fraction. The given fraction
    /// will end up on the LEFT side. If `frac` is not strictly equal to zero,
    /// then at least one germ will end up on the left!
    pub fn split(&self, frac: f32) -> (Germs, Germs) {
        if frac >= 1.0 || self.count==0 { (*self, Germs{id:self.id,count:0}) }
        else if frac <= 0.0 { (Germs{id:self.id,count:0}, *self) }
        else {
            let splat = ((self.count as f32 * frac).ceil() as i32).max(1);
            let splot = self.count.saturating_sub(splat);
            return (
                Germs { id: self.id, count: splat },
                Germs { id: self.id, count: splot },
            )
        }
    }
    /// Returns `None` if there aren't any actual germs in us, or `Some(self)`
    /// if there are.
    pub fn maybe(self) -> Option<Germs> {
        if self.count == 0 { None }
        else { Some(self) }
    }
}

impl Display for Phase {
    fn fmt(&self, fmt: &mut Formatter) -> std::fmt::Result {
        Debug::fmt(self, fmt)
    }
}

impl Display for Germs {
    fn fmt(&self, fmt: &mut Formatter) -> std::fmt::Result {
        fmt.write_fmt(format_args!("{}({}) x{}", get_germ_name(self.id).unwrap_or("???"), self.id, self.count))
    }
}

impl Display for MatPacket {
    fn fmt(&self, fmt: &mut Formatter) -> std::fmt::Result {
        fmt.write_fmt(format_args!("{:.2}kg of {}({}) at {:.1}°C", self.mass, get_element_name(self.element).unwrap_or("???"), self.element, self.temperature - 273.15))?;
        match self.germs {
            None => (),
            Some(ref germs) => {
                fmt.write_str(" germs ")?;
                Display::fmt(germs, fmt)?;
            }
        }
        Ok(())
    }
}

