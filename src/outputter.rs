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

use tokio::sync::mpsc;

/// Abstracts out the writing of log messages. Either uses `eprint!` or an
/// MPSC channel to send the messages out.
#[derive(Clone)]
pub enum Outputter {
    /// Uses `eprint!`
    Stderr,
    /// Uses an MPSC channel
    Channel(mpsc::UnboundedSender<String>),
}

impl std::fmt::Write for Outputter {
    fn write_str(&mut self, s: &str) -> Result<(), std::fmt::Error> {
        match self {
            Outputter::Stderr => eprint!("{}", s),
            Outputter::Channel(sender) => {
                let _ = sender.send(s.to_owned());
            }
        }
        Ok(())
    }
}
