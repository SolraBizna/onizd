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

use std::fmt::Display;

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord,Hash)]
pub struct Point {
    x: i32,
    y: i32,
    z: i32,
}

impl Display for Point {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.z == 0 {
            fmt.write_fmt(format_args!("{{{},{}}}", self.x, self.y))?;
        }
        else {
            fmt.write_fmt(format_args!("{{{},{},{}}}", self.x, self.y, self.z))?;
        }
        Ok(())
    }
}

impl Point {
    pub fn new(x: i32, y: i32, z: i32) -> Point {
        Point { x, y, z }
    }
    pub fn get_x(&self) -> i32 { self.x }
    pub fn get_y(&self) -> i32 { self.y }
    pub fn get_z(&self) -> i32 { self.z }
    pub fn as_string(&self) -> String {
        if self.z == 0 {
            format!("{},{}", self.x, self.y)
        }
        else {
            format!("{},{},{}", self.x, self.y, self.z)
        }
    }
}
