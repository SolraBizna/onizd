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

use std::time::Duration;
use std::convert::TryInto;

#[derive(Debug,Clone)]
pub struct Invocation {
    pub listen_addr: Option<String>,
    pub auth_file: Option<String>,
    pub save_file: Option<String>,
    pub offset_mode: bool,
    pub verbosity: u32,
    pub ping_interval: Option<Duration>,
}

fn print_usage(program: &str, opts: getopts::Options) {
    let brief = format!("\
This is the server component of the Oxygen Not Included mod, Z-Transport. It is the glue that connects the different \"Z-Layers\" together.\n\
\n\
Usage: {} [options]\
", program);
    print!("{}", opts.usage(&brief));
}

pub fn get_invocation() -> Option<Invocation> {
    let args: Vec<String> = std::env::args().collect();
    let mut opts = getopts::Options::new();
    opts.optopt("l", "listen-on", "Specify address and port to listen on.", "ADDR:PORT (default 0.0.0.0:5496)");
    opts.optflag("o", "offset-mode", "Add 1 to Y coordinate of all consumers; useful for single-world testing.");
    opts.optflagmulti("v", "verbose", "Print information every time something happens (lots!). Specify twice to print every received packet.");
    #[cfg(feature = "auth")]
    opts.optopt("a", "auth-file", "Specify the shared secret file to use for authentication. If absent, authentication will not be used.", "FILE");
    opts.optopt("s", "save-file", "Specify a JSON file in which to save and restore the map state.", "FILE");
    opts.optopt("p", "ping-interval", "Send a \"ping\" message to each client roughly this often. This can help deal with broken NAT routers that aggressively drop idle connections.", "SECONDS");
    opts.optflag("?", "help", "Print this help string.");
    let matches = match opts.parse(&args[1..]) {
        Ok(x) => x,
        Err(x) => {
            eprintln!("{}", x);
            print_usage(&args[0], opts);
            return None
        },
    };
    if matches.opt_present("?") || !matches.free.is_empty() {
        print_usage(&args[0], opts);
        None
    }
    else {
        Some(Invocation {
            listen_addr: matches.opt_str("l"),
            offset_mode: matches.opt_present("o"),
            verbosity: matches.opt_count("v").try_into().expect("ridiculous \
                                                                 -v count"),
            auth_file: if cfg!(feature = "auth") { matches.opt_str("a") }
            else { None },
            save_file: matches.opt_str("s"),
            ping_interval: match matches.opt_str("p") {
                None => None,
                Some(x) => match x.parse() {
                    Ok(x) if x > 0 && x < 999 => Some(Duration::new(x, 0)),
                    _ => {
                        eprintln!("Invalid ping interval, should be between 1 and 999");
                        print_usage(&args[0], opts);
                        return None
                    }
                }
            },
        })
    }
}
